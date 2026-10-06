//! Evidence policy v2: checked criteria, baselines, vacuous runs, and output scope.

mod common;

use RunProducer::{Operator, Verifier, Worker};
use RunTarget::{Base, Output};
use common::*;
use interlock_core::evidence::{CriterionVerdict, Records, baseline_problems, decide, evaluate};
use interlock_core::lifecycle::{self, Submission};
use interlock_schema::*;
use proptest::prelude::*;

/// A bug-fix task awaiting verification of TREE_A, with one checked,
/// independent criterion whose check must fail on the input snapshot.
fn task(baseline: Baseline) -> Task {
    let mut c = criterion("repro", MinStrength::Observed, Producer::Independent);
    c.check = Some("sh checks/repro.sh".into());
    c.baseline = baseline;
    let mut task = lifecycle::create(spec(vec![c]), &bug_fix(), t(0)).unwrap();
    task.state = State::AwaitingVerification;
    task.current_tree = Some(TREE_A.into());
    task.input_snapshot = Some(Snapshot {
        repository: "/work/repo".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec!["checks/repro.sh".into()],
    });
    task.scope = Scope { paths: vec!["src/**".into()], description: None };
    task
}

fn verdict(task: &Task, assessments: &[Evidence], runs: &[CheckRun]) -> CriterionVerdict {
    decide(task, &task.criteria[0], Records { claims: &[], assessments, runs }).verdict
}

fn observed(task: &Task) -> Vec<Evidence> {
    vec![evidence(task, "a1", "repro", "v1", Strength::Observed, TREE_A)]
}

#[test]
fn a_checked_criterion_needs_interlock_to_have_seen_the_check_pass() {
    let task = task(Baseline::Any);
    let CriterionVerdict::NotYet { reason } = verdict(&task, &observed(&task), &[]) else { panic!() };
    assert!(reason.contains("interlock check run --criterion repro"), "{reason}");
    let pass = run(&task, "r1", "repro", Verifier, Output, TREE_A, 0);
    assert!(matches!(verdict(&task, &observed(&task), &[pass]), CriterionVerdict::Pass { .. }));
}

#[test]
fn the_run_must_come_from_an_allowed_producer_and_this_tree() {
    let task = task(Baseline::Any);
    let by_worker = run(&task, "r1", "repro", Worker, Output, TREE_A, 0);
    assert!(matches!(verdict(&task, &observed(&task), &[by_worker]), CriterionVerdict::NotYet { .. }));
    let other_tree = run(&task, "r2", "repro", Verifier, Output, TREE_B, 0);
    assert!(matches!(verdict(&task, &observed(&task), &[other_tree]), CriterionVerdict::NotYet { .. }));
    let by_operator = run(&task, "r3", "repro", Operator, Output, TREE_A, 0);
    assert!(matches!(verdict(&task, &observed(&task), &[by_operator]), CriterionVerdict::Pass { .. }));
}

#[test]
fn a_verifier_run_that_fails_fails_the_criterion_but_a_worker_run_does_not() {
    let task = task(Baseline::Any);
    let failing = run(&task, "r1", "repro", Verifier, Output, TREE_A, 1);
    let CriterionVerdict::Fail { note, .. } = verdict(&task, &observed(&task), &[failing]) else { panic!() };
    assert!(note.unwrap().contains("exited 1"));
    let worker_fail = run(&task, "r2", "repro", Worker, Output, TREE_A, 1);
    assert!(matches!(verdict(&task, &[], &[worker_fail]), CriterionVerdict::NotYet { .. }));
}

#[test]
fn a_run_that_checked_nothing_never_counts() {
    let task = task(Baseline::Any);
    let mut empty = run(&task, "r1", "repro", Verifier, Output, TREE_A, 0);
    empty.vacuous = Some("unittest ran 0 tests".into());
    let CriterionVerdict::NotYet { reason } = verdict(&task, &observed(&task), &[empty.clone()]) else { panic!() };
    assert!(reason.contains("checked nothing (unittest ran 0 tests)"), "{reason}");
    empty.exit_code = Some(1);
    assert!(matches!(verdict(&task, &observed(&task), &[empty]), CriterionVerdict::NotYet { .. }), "nor fails");
}

#[test]
fn a_reproduction_must_fail_on_the_input_snapshot() {
    let task = task(Baseline::Fails);
    let output = run(&task, "r1", "repro", Verifier, Output, TREE_A, 0);
    let CriterionVerdict::NotYet { reason } = verdict(&task, &observed(&task), std::slice::from_ref(&output)) else {
        panic!()
    };
    assert!(reason.contains("on the input snapshot"), "{reason}");

    let base_passes = run(&task, "b1", "repro", Operator, Base, "e43c7ee000", 0);
    let problems = baseline_problems(&task, std::slice::from_ref(&base_passes));
    assert!(problems[0].1.contains("already passes on the input snapshot"), "{problems:?}");
    assert!(matches!(
        verdict(&task, &observed(&task), &[output.clone(), base_passes]),
        CriterionVerdict::NotYet { .. }
    ));

    let base_fails = run(&task, "b2", "repro", Operator, Base, "e43c7ee000", 1);
    assert!(baseline_problems(&task, std::slice::from_ref(&base_fails)).is_empty());
    assert!(matches!(verdict(&task, &observed(&task), &[output, base_fails]), CriterionVerdict::Pass { .. }));
}

#[test]
fn a_regression_guard_must_pass_and_test_something_on_the_input_snapshot() {
    let task = task(Baseline::Passes);
    let mut base = run(&task, "b1", "repro", Operator, Base, "e43c7ee000", 0);
    base.vacuous = Some("unittest ran 0 tests".into());
    let problems = baseline_problems(&task, &[base.clone()]);
    assert!(problems[0].1.contains("ran nothing on the input snapshot (unittest ran 0 tests)"), "{problems:?}");
    base.vacuous = None;
    base.exit_code = Some(1);
    assert!(baseline_problems(&task, &[base])[0].1.contains("already fails"));
}

#[test]
fn results_that_leave_the_scope_or_touch_the_checks_are_rejected() {
    let mut task = task(Baseline::Any);
    task.state = State::Running;
    task.lease_epoch = 1;
    task.current_attempt = Some("w1".into());
    let worker = attempt(&task, "w1", Role::Worker, 1);
    let submit = |paths: &[&str]| {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        lifecycle::submit_result(&task, &worker, 1, TREE_A, &paths, t(3)).unwrap()
    };
    assert!(matches!(submit(&["src/export.rs"]), Submission::Accepted(_)));
    let Submission::Rejected { reason } = submit(&["src/export.rs", "checks/repro.sh"]) else { panic!() };
    assert!(reason.contains("checks/repro.sh (a file the task's checks run)"), "{reason}");
    let Submission::Rejected { reason } = submit(&["README.md"]) else { panic!() };
    assert!(reason.contains("README.md (outside the scope src/**)"), "{reason}");
}

/// A running task at epoch 1 with worker w1, for G3 tests.
fn running(mut task: Task) -> (Task, Attempt) {
    task.state = State::Running;
    task.lease_epoch = 1;
    task.current_attempt = Some("w1".into());
    let worker = attempt(&task, "w1", Role::Worker, 1);
    (task, worker)
}

#[test]
fn an_empty_scope_allows_no_change_and_double_star_allows_any() {
    let mut empty = task(Baseline::Any);
    empty.scope = Scope { paths: vec![], description: None };
    let (empty, worker) = running(empty);
    let changed = vec!["src/export.rs".to_string()];
    let Submission::Rejected { reason } = lifecycle::submit_result(&empty, &worker, 1, TREE_A, &changed, t(3)).unwrap()
    else {
        panic!("an empty scope used to allow everything")
    };
    assert!(reason.contains("scope is empty"), "{reason}");
    assert!(matches!(
        lifecycle::submit_result(&empty, &worker, 1, TREE_A, &[], t(3)).unwrap(),
        Submission::Accepted(_)
    ));

    let mut any = task(Baseline::Any);
    any.scope = Scope { paths: vec!["**".into()], description: None };
    let (any, worker) = running(any);
    let changed = vec!["deep/any/where.rs".to_string()];
    assert!(matches!(
        lifecycle::submit_result(&any, &worker, 1, TREE_A, &changed, t(3)).unwrap(),
        Submission::Accepted(_)
    ));
}

#[test]
fn an_investigation_rejects_any_change_whatever_its_scope() {
    let mut inv = task(Baseline::Any);
    inv.workflow = WorkflowRef { name: "investigation".into(), version: 1 };
    inv.scope = Scope { paths: vec!["**".into()], description: None };
    let (inv, worker) = running(inv);
    let changed = vec!["notes.md".to_string()];
    let Submission::Rejected { reason } = lifecycle::submit_result(&inv, &worker, 1, TREE_A, &changed, t(3)).unwrap()
    else {
        panic!("an investigation changed a file")
    };
    assert!(reason.contains("an investigation changes no files"), "{reason}");
    assert!(matches!(lifecycle::submit_result(&inv, &worker, 1, TREE_A, &[], t(3)).unwrap(), Submission::Accepted(_)));
}

#[test]
fn an_unbound_verifier_cannot_pass_a_criterion_without_a_check() {
    let mut task = task(Baseline::Any);
    task.criteria[0].check = None;
    let with = |via: Option<BoundVia>, strength: Strength| {
        let mut e = evidence(&task, "a1", "repro", "v1", strength, TREE_A);
        e.bound_via = via;
        verdict(&task, &[e], &[])
    };
    let unbound = with(Some(BoundVia::Unbound), Strength::Observed);
    let CriterionVerdict::NotYet { reason } = &unbound else { panic!("unbound passed: {unbound:?}") };
    assert!(reason.contains("unbound verifier"), "{reason}");
    for via in [Some(BoundVia::Subagent), Some(BoundVia::InterlockLaunched), None] {
        assert!(matches!(with(via, Strength::Observed), CriterionVerdict::Pass { .. }), "{via:?}");
    }
    assert!(
        matches!(with(Some(BoundVia::Unbound), Strength::Failed), CriterionVerdict::Fail { .. }),
        "a failure counts from anyone"
    );

    // Where interlock ran the check, an unbound verifier's assessment counts alongside the run.
    let checked = self::task(Baseline::Any);
    let mut e = evidence(&checked, "a1", "repro", "v1", Strength::Observed, TREE_A);
    e.bound_via = Some(BoundVia::Unbound);
    let runs = [run(&checked, "r1", "repro", Verifier, Output, TREE_A, 0)];
    assert!(matches!(verdict(&checked, &[e], &runs), CriterionVerdict::Pass { .. }));
}

proptest! {
    /// Vacuous runs never move a verdict, whatever their exit code or producer.
    #[test]
    fn vacuous_runs_change_nothing(exits in prop::collection::vec((-1i32..3, 0usize..3), 0..8)) {
        let task = task(Baseline::Any);
        let producers = [Worker, Verifier, Operator];
        let runs: Vec<CheckRun> = exits
            .iter()
            .enumerate()
            .map(|(i, (code, p))| {
                let mut r = run(&task, &format!("r{i}"), "repro", producers[*p], Output, TREE_A, *code);
                r.vacuous = Some("pytest collected no tests".into());
                r
            })
            .collect();
        let with = evaluate(&task, Records { claims: &[], assessments: &observed(&task), runs: &runs });
        let without = evaluate(&task, Records { claims: &[], assessments: &observed(&task), runs: &[] });
        prop_assert_eq!(with.all_pass, without.all_pass);
        prop_assert_eq!(with.any_fail, without.any_fail);
    }
}
