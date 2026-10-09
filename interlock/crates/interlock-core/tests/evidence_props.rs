//! Property tests for evidence policy v1.

mod common;

use common::*;
use interlock_core::evidence::{CriterionVerdict, Records, decide};
use interlock_schema::*;
use proptest::prelude::*;

fn strength() -> impl Strategy<Value = Strength> {
    prop_oneof![
        Just(Strength::Observed),
        Just(Strength::Tested),
        Just(Strength::Static),
        Just(Strength::Blocked),
        Just(Strength::Failed),
    ]
}

fn tree() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(TREE_A), Just(TREE_B)]
}

/// A task whose output tree is TREE_A, with one criterion of each producer.
fn task() -> Task {
    let mut task = bug_fix_task();
    task.current_tree = Some(TREE_A.into());
    task.state = State::AwaitingVerification;
    task
}

fn records(task: &Task, raw: &[(Strength, &'static str, bool)], prefix: &str) -> Vec<Evidence> {
    raw.iter()
        .enumerate()
        .map(|(i, (s, tree, repro))| {
            evidence(task, &format!("{prefix}{i}"), if *repro { "repro" } else { "regression" }, "att", *s, tree)
        })
        .collect()
}

fn raw() -> impl Strategy<Value = Vec<(Strength, &'static str, bool)>> {
    prop::collection::vec((strength(), tree(), any::<bool>()), 0..12)
}

proptest! {
    /// Invariant 2: a current independent failure fails the criterion whatever else says pass.
    #[test]
    fn current_independent_failure_always_wins(claims in raw(), assessments in raw()) {
        let task = task();
        let claims = records(&task, &claims, "c");
        let mut assessments = records(&task, &assessments, "a");
        assessments.push(evidence(&task, "fail", "repro", "v", Strength::Failed, TREE_A));
        let report = decide(&task, &task.criteria[0], Records { claims: &claims, assessments: &assessments, runs: &[] });
        let is_fail = matches!(report.verdict, CriterionVerdict::Fail { .. });
        prop_assert!(is_fail);
        prop_assert!(!eval(&task, &claims, &assessments).all_pass);
    }

    /// Invariant 1 and 3: evidence for another tree never changes a verdict.
    #[test]
    fn stale_evidence_never_counts(claims in raw(), assessments in raw(), extra in raw()) {
        let task = task();
        let claims = records(&task, &claims, "c");
        let assessments = records(&task, &assessments, "a");
        let before = eval(&task, &claims, &assessments);
        let stale: Vec<Evidence> = records(&task, &extra, "s")
            .into_iter()
            .map(|mut e| { e.currency.tree = "ccccccc3333333333333333333333333333333333".into(); e })
            .collect();
        let mut more_claims = claims.clone();
        more_claims.extend(stale.iter().cloned());
        let mut more_assessments = assessments.clone();
        more_assessments.extend(stale);
        let after = eval(&task, &more_claims, &more_assessments);
        prop_assert_eq!(before.all_pass, after.all_pass);
        prop_assert_eq!(before.any_fail, after.any_fail);
    }

    /// Worker claims never satisfy a criterion that needs an independent producer.
    #[test]
    fn claims_never_pass_independent_criteria(claims in raw()) {
        let task = task();
        let claims = records(&task, &claims, "c");
        let report = decide(&task, &task.criteria[0], Records { claims: &claims, assessments: &[], runs: &[] });
        let is_pass = matches!(report.verdict, CriterionVerdict::Pass { .. });
        prop_assert!(!is_pass);
    }

    /// The decision does not depend on the order records arrive in.
    #[test]
    fn order_does_not_matter(claims in raw(), assessments in raw()) {
        let task = task();
        let claims = records(&task, &claims, "c");
        let assessments = records(&task, &assessments, "a");
        let forward = eval(&task, &claims, &assessments);
        let mut rc = claims.clone();
        rc.reverse();
        let mut ra = assessments.clone();
        ra.reverse();
        let backward = eval(&task, &rc, &ra);
        prop_assert_eq!(forward.all_pass, backward.all_pass);
        prop_assert_eq!(forward.any_fail, backward.any_fail);
    }

    /// Changing the policy digest makes all existing evidence stale.
    #[test]
    fn a_new_policy_digest_invalidates_everything(claims in raw(), assessments in raw()) {
        let mut task = task();
        let claims = records(&task, &claims, "c");
        let assessments = records(&task, &assessments, "a");
        task.policy_digest = "sha256:changed".into();
        let report = eval(&task, &claims, &assessments);
        prop_assert!(!report.all_pass && !report.any_fail);
    }
}

#[test]
fn no_output_tree_means_not_yet() {
    let task = bug_fix_task();
    let assessments = vec![evidence(&task, "a", "repro", "v", Strength::Observed, TREE_A)];
    let report = eval(&task, &[], &assessments);
    assert!(!report.all_pass);
}
