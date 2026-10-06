//! Delivery rules: what each forge operation needs, and what the forge's
//! answer means for an operation whose outcome nobody confirmed.

mod common;

use common::*;
use interlock_core::delivery::{self, Observation, PrState, PullRequest, UNKNOWN_OUTCOME, Verdict};
use interlock_core::evidence::{self, Records};
use interlock_core::lifecycle::{self, MergeReport, RefusalCode, Signal};
use interlock_schema::*;
use proptest::prelude::*;

const HEAD: &str = "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "2222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn integrating() -> Task {
    let mut task = bug_fix_task();
    task.integration_required = true;
    task.state = State::Integrating;
    task.current_tree = Some(TREE_A.into());
    task.input_snapshot = Some(Snapshot {
        repository: "/work/repo".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec![],
    });
    task
}

fn op(kind: OperationKind, state: OperationState) -> Operation {
    Operation {
        id: "op-1".into(),
        task_id: "export-retry".into(),
        kind,
        intent: OperationIntent {
            expected_head_sha: Some(HEAD.into()),
            base: Some("main".into()),
            pull_request: Some(7),
        },
        state,
        outcome: None,
        created_at: t(0),
        updated_at: t(0),
    }
}

fn pr(state: PrState, head: &str, auto_merge: bool) -> Observation {
    Observation::Pr(PullRequest { number: 7, state, head: head.into(), auto_merge })
}

#[test]
fn a_merge_of_the_expected_head_is_g6_and_any_other_head_is_not() {
    let merge = op(OperationKind::Merge, OperationState::Started);
    assert_eq!(
        delivery::reconcile(&merge, &pr(PrState::Merged, HEAD, false)),
        Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() } }
    );
    let Verdict::Block { reason } = delivery::reconcile(&merge, &pr(PrState::Merged, OTHER, false)) else {
        panic!("a merge of another head never confirms")
    };
    assert!(reason.contains("reconcile by hand"), "{reason}");
}

#[test]
fn an_open_pull_request_tells_a_moved_head_from_a_merge_that_never_happened() {
    let merge = op(OperationKind::Merge, OperationState::Started);
    let Verdict::Land { report: MergeReport::Refused { reason } } =
        delivery::reconcile(&merge, &pr(PrState::Open, OTHER, false))
    else {
        panic!("a moved head is R2")
    };
    assert!(reason.contains("moved"), "{reason}");
    assert!(matches!(delivery::reconcile(&merge, &pr(PrState::Open, HEAD, false)), Verdict::Failed { .. }));
    assert!(matches!(delivery::reconcile(&merge, &pr(PrState::Open, HEAD, true)), Verdict::Pending { .. }));
    assert!(matches!(delivery::reconcile(&merge, &pr(PrState::Closed, HEAD, false)), Verdict::Block { .. }));
}

#[test]
fn no_answer_is_unknown_never_a_guess() {
    for kind in [OperationKind::OpenPr, OperationKind::Merge, OperationKind::ArmAutoMerge] {
        let v = delivery::reconcile(&op(kind, OperationState::Started), &Observation::Unreachable("timeout".into()));
        assert!(matches!(v, Verdict::Unknown { .. }), "{kind:?}: {v:?}");
    }
    // A landing whose pull request the forge cannot find is unknown too.
    let v = delivery::reconcile(&op(OperationKind::Merge, OperationState::Started), &Observation::NoPr);
    assert!(matches!(v, Verdict::Unknown { .. }));
}

#[test]
fn opening_a_pull_request_is_confirmed_only_at_the_expected_head() {
    let open = op(OperationKind::OpenPr, OperationState::Started);
    assert_eq!(delivery::reconcile(&open, &pr(PrState::Open, HEAD, false)), Verdict::Confirmed);
    assert!(matches!(delivery::reconcile(&open, &pr(PrState::Open, OTHER, false)), Verdict::Failed { .. }));
    assert!(matches!(delivery::reconcile(&open, &Observation::NoPr), Verdict::Failed { .. }));
}

proptest! {
    /// Whatever the forge shows, a landing is confirmed only for the head it was pinned to.
    #[test]
    fn only_the_pinned_head_ever_lands(
        head in "[0-9a-f]{40}",
        state in prop_oneof![Just(PrState::Open), Just(PrState::Closed), Just(PrState::Merged)],
        auto_merge: bool,
        kind in prop_oneof![Just(OperationKind::Merge), Just(OperationKind::ArmAutoMerge)],
    ) {
        let v = delivery::reconcile(&op(kind, OperationState::Started), &pr(state, &head, auto_merge));
        if let Verdict::Land { report: MergeReport::Merged { head_sha } } = v {
            prop_assert!(delivery::same_sha(&head_sha, HEAD));
            prop_assert_eq!(head, HEAD);
        }
    }
}

#[test]
fn every_call_needs_landing_authority_at_the_time_of_the_call() {
    let task = integrating();
    let planned = op(OperationKind::Merge, OperationState::Planned);
    assert!(delivery::authorize(&task, &planned, LandingAuthority::Operator).is_ok());
    let refused = delivery::authorize(&task, &planned, LandingAuthority::None).unwrap_err();
    assert_eq!(refused.code, RefusalCode::GrantInsufficient);
    let open = op(OperationKind::OpenPr, OperationState::Planned);
    assert_eq!(
        delivery::authorize(&task, &open, LandingAuthority::None).unwrap_err().code,
        RefusalCode::GrantInsufficient
    );
    // Only a planned operation of an integrating task may start.
    let started = op(OperationKind::Merge, OperationState::Started);
    assert_eq!(
        delivery::authorize(&task, &started, LandingAuthority::Operator).unwrap_err().code,
        RefusalCode::WrongState
    );
    let mut verified = task.clone();
    verified.state = State::Verified;
    assert_eq!(
        delivery::authorize(&verified, &planned, LandingAuthority::Operator).unwrap_err().code,
        RefusalCode::WrongState
    );
    assert_eq!(delivery::action_class(OperationKind::OpenPr), ActionClass::ExternalReversible);
    assert_eq!(delivery::action_class(OperationKind::Merge), ActionClass::Landing);
}

#[test]
fn a_moved_base_makes_the_evidence_stale_and_r2_follows() {
    let task = integrating();
    let assessments = vec![
        Evidence {
            id: "a1".into(),
            task_id: task.id.clone(),
            criterion_id: "repro".into(),
            attempt_id: "v1".into(),
            strength: Strength::Observed,
            currency: evidence::currency_for(&task, &task.criteria[0]).unwrap(),
            evidence_refs: vec![],
            note: None,
            recorded_at: t(1),
        },
        Evidence {
            id: "a2".into(),
            task_id: task.id.clone(),
            criterion_id: "regression".into(),
            attempt_id: "v1".into(),
            strength: Strength::Tested,
            currency: evidence::currency_for(&task, &task.criteria[1]).unwrap(),
            evidence_refs: vec![],
            note: None,
            recorded_at: t(1),
        },
    ];
    let records = Records { claims: &[], assessments: &assessments, runs: &[] };
    assert!(evidence::evaluate(&task, records).all_pass);

    let rebased = delivery::rebase(&task, "f00dbabe", TREE_B, t(2)).unwrap();
    assert_eq!(rebased.input_snapshot.as_ref().unwrap().base_commit, "f00dbabe");
    assert_eq!(rebased.current_tree.as_deref(), Some(TREE_B));
    let report = evidence::evaluate(&rebased, records);
    assert!(!report.all_pass, "evidence about the old tree no longer counts");
    let out = lifecycle::advance(&rebased, &report, t(3)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::R2, State::AwaitingVerification));
}

#[test]
fn only_a_block_on_an_unknown_outcome_lifts_when_the_forge_answers() {
    let task = integrating();
    let blocked = lifecycle::block(&task, &format!("{UNKNOWN_OUTCOME}: op-1 ..."), t(1)).unwrap().task;
    let out = delivery::resume(&blocked, t(2)).unwrap();
    assert_eq!((out.mv.signal, out.task.state), (Signal::Unblock, State::Integrating));
    let by_operator = lifecycle::block(&task, "operator wants to look first", t(1)).unwrap().task;
    assert!(delivery::resume(&by_operator, t(2)).is_none());
}

#[test]
fn branch_names_are_valid_for_any_task_id() {
    assert_eq!(delivery::branch_for("fix-add"), "interlock/fix-add");
    assert_eq!(delivery::branch_for("team:fix..add."), "interlock/team-fix-add");
    assert_eq!(delivery::branch_for("x.lock"), "interlock/x.lock-");
}
