mod common;

use common::*;
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::grants::{self, HostPolicy, Profile};
use interlock_core::lifecycle::{self, MergeReport, RefusalCode, Signal, Start, Submission};
use interlock_core::workflow::Mode;
use interlock_schema::*;

fn snapshot() -> Snapshot {
    Snapshot {
        repository: "/work/repo".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec![],
    }
}

/// Moves a fresh task to awaiting verification with TREE_A, returning the worker attempt.
fn to_awaiting() -> (Task, Attempt) {
    let task = bug_fix_task();
    let task = lifecycle::ready(&task, &[], snapshot(), t(1)).unwrap().task;
    let start = lifecycle::start_worker(
        &task,
        &bug_fix(),
        Mode::Headless,
        &all_capabilities(),
        &grant_for(&task, Role::Worker),
        "w1",
        t(2),
    )
    .unwrap();
    let Start::Allowed { epoch, outcome: Some(out), .. } = start else { panic!("expected start") };
    assert_eq!(epoch, 1);
    let task = out.task;
    let worker = attempt(&task, "w1", Role::Worker, 1);
    let Submission::Accepted(out) = lifecycle::submit_result(&task, &worker, 1, TREE_A, &[], t(3)).unwrap() else {
        panic!("expected acceptance")
    };
    assert_eq!(out.mv.signal, Signal::G3);
    (out.task, worker)
}

#[test]
fn bug_fix_happy_path_reaches_done_through_g7() {
    let (task, _) = to_awaiting();
    assert_eq!(task.state, State::AwaitingVerification);
    let claims = vec![evidence(&task, "c1", "regression", "w1", Strength::Tested, TREE_A)];
    let assessments = vec![evidence(&task, "a1", "repro", "v1", Strength::Observed, TREE_A)];
    let report = eval(&task, &claims, &assessments);
    assert!(report.all_pass, "{report:?}");
    let out = lifecycle::advance(&task, &report, t(6)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::G4, State::Verified));
    let out = lifecycle::advance(&out.task, &report, t(7)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::G7, State::Done));
}

#[test]
fn creation_rejects_tasks_that_could_never_be_verified() {
    let err = lifecycle::create(spec(vec![]), &bug_fix(), t(0)).unwrap_err();
    assert_eq!(err.code, RefusalCode::InvalidTask);
    let mut s = spec(vec![criterion("x", MinStrength::Static, Producer::SelfReport)]);
    s.integration_required = true;
    let investigation = interlock_core::workflow::builtin("investigation", 1).unwrap();
    assert!(lifecycle::create(s, &investigation, t(0)).is_err(), "investigations never integrate");
}

#[test]
fn g1_waits_for_dependencies() {
    let mut task = bug_fix_task();
    task.dependencies = vec!["schema-change".into()];
    let err = lifecycle::ready(&task, &[("schema-change".into(), State::Running)], snapshot(), t(1)).unwrap_err();
    assert_eq!(err.code, RefusalCode::DependenciesNotDone);
    assert!(lifecycle::ready(&task, &[("schema-change".into(), State::Done)], snapshot(), t(1)).is_ok());
}

#[test]
fn g2_blocks_when_a_required_capability_has_no_fallback() {
    let task = lifecycle::ready(&bug_fix_task(), &[], snapshot(), t(1)).unwrap().task;
    // Headless needs session start; this host only restricts tools.
    let caps: CapabilitySet = [Capability::ToolRestriction].into_iter().collect();
    let start =
        lifecycle::start_worker(&task, &bug_fix(), Mode::Headless, &caps, &grant_for(&task, Role::Worker), "w1", t(2))
            .unwrap();
    let Start::Blocked(out) = start else { panic!("expected blocked") };
    assert_eq!(out.task.state, State::Blocked);
    assert_eq!(out.task.resume_point, Some(State::Ready));
    // Unblocking returns to the exact state it left.
    assert_eq!(lifecycle::unblock(&out.task, t(3)).unwrap().task.state, State::Ready);
}

#[test]
fn g2_refuses_when_the_host_policy_takes_away_a_tool_the_role_needs() {
    let task = lifecycle::ready(&bug_fix_task(), &[], snapshot(), t(1)).unwrap().task;
    let workflow = bug_fix();
    let grant_with = |host: &HostPolicy| {
        grants::effective(&task.id, Profile::Conservative, &[], host, workflow.role(Role::Worker), t(0))
    };
    // A narrower deny leaves the tool usable.
    let mut narrow = HostPolicy::open();
    narrow.tools.deny.push("shell:curl".into());
    let ok = lifecycle::start_worker(
        &task,
        &workflow,
        Mode::Headless,
        &all_capabilities(),
        &grant_with(&narrow),
        "w1",
        t(2),
    );
    assert!(matches!(ok, Ok(Start::Allowed { .. })), "{ok:?}");
    // Denying a whole tool the worker needs refuses G2 and names it.
    let mut no_edit = HostPolicy::open();
    no_edit.tools.deny.push("edit".into());
    let err = lifecycle::start_worker(
        &task,
        &workflow,
        Mode::Headless,
        &all_capabilities(),
        &grant_with(&no_edit),
        "w1",
        t(2),
    )
    .unwrap_err();
    assert_eq!(err.code, RefusalCode::GrantInsufficient);
    assert_eq!(err.missing, ["edit"]);
    // So does a host that never allows it.
    let mut no_shell = HostPolicy::open();
    no_shell.tools.allow.retain(|t| t != "shell");
    let err = lifecycle::start_worker(
        &task,
        &workflow,
        Mode::Headless,
        &all_capabilities(),
        &grant_with(&no_shell),
        "w1",
        t(2),
    )
    .unwrap_err();
    assert!(err.missing.contains(&"shell".to_string()), "{err:?}");
}

#[test]
fn g2_uses_declared_fallbacks_for_optional_capabilities() {
    let task = lifecycle::ready(&bug_fix_task(), &[], snapshot(), t(1)).unwrap().task;
    let caps: CapabilitySet = [Capability::PerCallPolicy].into_iter().collect();
    let start = lifecycle::start_worker(
        &task,
        &bug_fix(),
        Mode::Interactive,
        &caps,
        &grant_for(&task, Role::Worker),
        "w1",
        t(2),
    )
    .unwrap();
    let Start::Allowed { fallbacks, .. } = start else { panic!("expected start") };
    assert_eq!(fallbacks.len(), 2, "sequential and current-model fallbacks");
}

#[test]
fn a_missing_independent_verifier_blocks_and_keeps_the_work() {
    let (task, _) = to_awaiting();
    let caps: CapabilitySet = [Capability::ToolRestriction].into_iter().collect();
    let start =
        lifecycle::start_verifier(&task, &bug_fix(), Mode::Interactive, &caps, &grant_for(&task, Role::Verifier), t(4))
            .unwrap();
    let Start::Blocked(out) = start else { panic!("expected blocked") };
    assert!(out.task.blocked_reason.unwrap().starts_with("no independent verifier"));
    assert_eq!(out.task.current_tree.as_deref(), Some(TREE_A), "work kept");
    assert_eq!(out.task.resume_point, Some(State::AwaitingVerification));
}

#[test]
fn failed_check_sends_work_back_through_r1_then_fails_when_budget_is_spent() {
    let (task, _) = to_awaiting();
    let assessments = vec![evidence(&task, "a1", "repro", "v1", Strength::Failed, TREE_A)];
    let out = lifecycle::advance(&task, &eval(&task, &[], &assessments), t(6)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::R1, State::Ready));
    assert_eq!(out.task.current_attempt, None);

    // Second and last attempt also fails verification.
    let task = out.task;
    let Start::Allowed { outcome: Some(out), epoch, .. } = lifecycle::start_worker(
        &task,
        &bug_fix(),
        Mode::Headless,
        &all_capabilities(),
        &grant_for(&task, Role::Worker),
        "w2",
        t(7),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(epoch, 2);
    let w2 = attempt(&out.task, "w2", Role::Worker, 2);
    let Submission::Accepted(out) = lifecycle::submit_result(&out.task, &w2, 2, TREE_B, &[], t(8)).unwrap() else {
        panic!()
    };
    let task = out.task;
    let assessments = vec![evidence(&task, "a2", "repro", "v2", Strength::Failed, TREE_B)];
    let out = lifecycle::advance(&task, &eval(&task, &[], &assessments), t(9)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::Fail, State::Failed));
}

#[test]
fn g5_without_landing_authority_blocks_at_verified() {
    let (mut task, _) = to_awaiting();
    task.integration_required = true;
    let assessments = vec![
        evidence(&task, "a1", "repro", "v1", Strength::Observed, TREE_A),
        evidence(&task, "a2", "regression", "v1", Strength::Tested, TREE_A),
    ];
    let report = eval(&task, &[], &assessments);
    let task = lifecycle::advance(&task, &report, t(6)).unwrap().task;
    assert_eq!(task.state, State::Verified);
    assert!(lifecycle::advance(&task, &report, t(7)).is_none(), "G7 does not fire when integration is required");

    let out = lifecycle::begin_integration(&task, &report, LandingAuthority::None, t(8)).unwrap();
    assert_eq!(out.task.state, State::Blocked);
    assert_eq!(out.task.blocked_reason.as_deref(), Some("no landing authority is granted for this task"));

    let out = lifecycle::begin_integration(&task, &report, LandingAuthority::Operator, t(8)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::G5, State::Integrating));

    let op = Operation {
        id: "op1".into(),
        task_id: task.id.clone(),
        kind: OperationKind::Merge,
        intent: OperationIntent { expected_head_sha: Some(TREE_A.into()), base: None, pull_request: Some(7) },
        state: OperationState::Started,
        outcome: None,
        created_at: t(9),
        updated_at: t(9),
    };
    // The head moved: the forge refuses the pinned merge and R2 re-verifies.
    let refused =
        lifecycle::confirm_integration(&out.task, &op, &MergeReport::Refused { reason: "head moved".into() }, t(10))
            .unwrap();
    assert_eq!((refused.mv.signal, refused.task.state), (Signal::R2, State::AwaitingVerification));
    // A confirmed merge of the verified head finishes it.
    let done = lifecycle::confirm_integration(&out.task, &op, &MergeReport::Merged { head_sha: TREE_A.into() }, t(10))
        .unwrap();
    assert_eq!((done.mv.signal, done.task.state), (Signal::G6, State::Done));
    let wrong = lifecycle::confirm_integration(&out.task, &op, &MergeReport::Merged { head_sha: TREE_B.into() }, t(10));
    assert_eq!(wrong.unwrap_err().code, RefusalCode::HeadMismatch);
}

#[test]
fn verifiers_cannot_claim_and_workers_cannot_assess() {
    let (task, worker) = to_awaiting();
    let verifier = attempt(&task, "v1", Role::Verifier, 1);
    use lifecycle::EvidenceKind::*;
    assert_eq!(
        lifecycle::check_evidence_source(&task, &worker, Assessment, "repro").unwrap_err().code,
        RefusalCode::WrongRole
    );
    assert_eq!(
        lifecycle::check_evidence_source(&task, &verifier, Claim, "repro").unwrap_err().code,
        RefusalCode::WrongRole
    );
    assert_eq!(
        lifecycle::check_evidence_source(&task, &verifier, Assessment, "nope").unwrap_err().code,
        RefusalCode::UnknownCriterion
    );
    lifecycle::check_evidence_source(&task, &verifier, Assessment, "repro").unwrap();
}

#[test]
fn terminal_tasks_refuse_every_move() {
    let task = lifecycle::cancel(&bug_fix_task(), "operator", t(1)).unwrap().task;
    assert_eq!(lifecycle::ready(&task, &[], snapshot(), t(2)).unwrap_err().code, RefusalCode::Terminal);
    assert_eq!(lifecycle::fail(&task, "x", t(2)).unwrap_err().code, RefusalCode::Terminal);
    assert!(lifecycle::next_moves(&task, &eval(&task, &[], &[])).is_empty());
}

#[test]
fn changing_a_criterion_changes_the_policy_digest() {
    let a = bug_fix_task();
    let mut s = spec(vec![
        criterion("repro", MinStrength::Tested, Producer::Independent),
        criterion("regression", MinStrength::Tested, Producer::SelfReport),
    ]);
    s.id = a.id.clone();
    let b = lifecycle::create(s, &bug_fix(), t(0)).unwrap();
    assert_ne!(a.policy_digest, b.policy_digest);
}
