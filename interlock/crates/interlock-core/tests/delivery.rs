//! Delivery rules: what each forge operation needs, and what the forge's
//! answer means for an operation whose outcome nobody confirmed.

mod common;

use common::*;
use interlock_core::delivery::{
    self, Landed, Observation, PrState, PullRequest, UNKNOWN_OUTCOME, Verdict, branch_for, task_for_branch,
};
use interlock_core::evidence::{self, Records};
use interlock_core::lifecycle::{self, MergeReport, RefusalCode, Signal};
use interlock_schema::*;
use proptest::prelude::*;

const HEAD: &str = "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "2222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BASE: &str = "e43c7ee000000000000000000000000000000000";

fn integrating() -> Task {
    let mut task = bug_fix_task();
    task.integration_required = true;
    task.state = State::Integrating;
    task.current_tree = Some(TREE_A.into());
    task.input_snapshot = Some(Snapshot {
        repository: "/work/repo".into(),
        base_commit: BASE.into(),
        untracked_hash: None,
        protected_paths: vec![],
    });
    task
}

fn op_at(kind: OperationKind, state: OperationState, head: &str) -> Operation {
    Operation {
        id: "op-1".into(),
        task_id: "export-retry".into(),
        kind,
        intent: OperationIntent {
            expected_head_sha: Some(head.into()),
            base: Some("main".into()),
            pull_request: Some(7),
            tree: None,
        },
        state,
        outcome: None,
        created_at: t(0),
        updated_at: t(0),
    }
}

fn op(kind: OperationKind, state: OperationState) -> Operation {
    op_at(kind, state, HEAD)
}

/// What git sees when the merge landed exactly what was verified.
fn landed_right() -> Option<Landed> {
    Some(Landed { on_base: true, onto: Some(BASE.into()), tree: Some(TREE_A.into()) })
}

fn pr_with(state: PrState, head: &str, auto_merge: bool, landed: Option<Landed>) -> Observation {
    Observation::Pr(PullRequest {
        number: 7,
        state,
        head: head.into(),
        auto_merge,
        base_branch: Some("main".into()),
        merged_at: None,
        landed,
    })
}

fn pr(state: PrState, head: &str, auto_merge: bool) -> Observation {
    let landed = if state == PrState::Merged { landed_right() } else { None };
    pr_with(state, head, auto_merge, landed)
}

fn reconcile(op: &Operation, seen: &Observation) -> Verdict {
    delivery::reconcile(op, seen, &integrating())
}

#[test]
fn a_merge_of_the_expected_head_onto_the_verified_base_is_g6_and_any_other_head_is_not() {
    let merge = op(OperationKind::Merge, OperationState::Started);
    assert_eq!(
        reconcile(&merge, &pr(PrState::Merged, HEAD, false)),
        Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() }, merged_at: None }
    );
    let Verdict::Block { reason, took_effect: true } = reconcile(&merge, &pr(PrState::Merged, OTHER, false)) else {
        panic!("a merge of another head never confirms")
    };
    assert!(reason.contains("reconcile by hand"), "{reason}");
}

#[test]
fn a_merge_that_landed_on_a_base_nobody_verified_blocks_instead_of_g6() {
    let merge = op(OperationKind::ArmAutoMerge, OperationState::Started);
    let moved = Some(Landed { on_base: true, onto: Some(OTHER.into()), tree: Some(TREE_B.into()) });
    let Verdict::Block { reason, took_effect: true } = reconcile(&merge, &pr_with(PrState::Merged, HEAD, false, moved))
    else {
        panic!("a merge onto another base is not the verified landing")
    };
    assert!(reason.starts_with("landed on a base nobody verified"), "{reason}");
    // Same base, but the tree differs from the evidence.
    let odd_tree = Some(Landed { on_base: true, onto: Some(BASE.into()), tree: Some(TREE_B.into()) });
    let Verdict::Block { reason, .. } = reconcile(&merge, &pr_with(PrState::Merged, HEAD, false, odd_tree)) else {
        panic!("a merge that left another tree is not the verified landing")
    };
    assert!(reason.starts_with("landed a tree nobody verified"), "{reason}");
}

#[test]
fn a_reported_merge_git_cannot_see_is_unknown() {
    let merge = op(OperationKind::Merge, OperationState::Started);
    let not_on_base = Some(Landed { on_base: false, onto: None, tree: None });
    assert!(matches!(reconcile(&merge, &pr_with(PrState::Merged, HEAD, false, not_on_base)), Verdict::Unknown { .. }));
    assert!(matches!(reconcile(&merge, &pr_with(PrState::Merged, HEAD, false, None)), Verdict::Unknown { .. }));
}

#[test]
fn a_merge_into_another_branch_never_lands_the_task() {
    let merge = op(OperationKind::Merge, OperationState::Started);
    let mut into_release = PullRequest {
        number: 7,
        state: PrState::Merged,
        head: HEAD.into(),
        auto_merge: false,
        base_branch: Some("release-1".into()),
        merged_at: None,
        landed: landed_right(),
    };
    let Verdict::Block { reason, took_effect: true } = reconcile(&merge, &Observation::Pr(into_release.clone())) else {
        panic!("merged into the wrong branch")
    };
    assert!(reason.contains("release-1, not main"), "{reason}");
    into_release.state = PrState::Open;
    assert!(matches!(reconcile(&merge, &Observation::Pr(into_release)), Verdict::Block { took_effect: false, .. }));
}

#[test]
fn an_open_pull_request_tells_a_moved_head_from_a_merge_that_never_happened() {
    let merge = op(OperationKind::Merge, OperationState::Started);
    let Verdict::Land { report: MergeReport::Refused { reason }, .. } =
        reconcile(&merge, &pr(PrState::Open, OTHER, false))
    else {
        panic!("a moved head is R2")
    };
    assert!(reason.contains("moved"), "{reason}");
    assert!(matches!(reconcile(&merge, &pr(PrState::Open, HEAD, false)), Verdict::Failed { .. }));
    assert!(matches!(reconcile(&merge, &pr(PrState::Open, HEAD, true)), Verdict::Pending { .. }));
    assert!(matches!(reconcile(&merge, &pr(PrState::Closed, HEAD, false)), Verdict::Block { .. }));
}

#[test]
fn a_planned_landing_changes_only_when_someone_merged_or_moved_the_head() {
    let planned = op(OperationKind::Merge, OperationState::Planned);
    assert!(matches!(reconcile(&planned, &pr(PrState::Open, HEAD, false)), Verdict::Pending { .. }));
    assert!(matches!(reconcile(&planned, &Observation::Unreachable("x".into())), Verdict::Pending { .. }));
    assert!(matches!(reconcile(&planned, &pr(PrState::Merged, HEAD, false)), Verdict::Land { .. }));
    assert!(matches!(
        reconcile(&planned, &pr(PrState::Open, OTHER, false)),
        Verdict::Land { report: MergeReport::Refused { .. }, .. }
    ));
}

#[test]
fn disarming_is_confirmed_only_when_auto_merge_is_off() {
    let disarm = op(OperationKind::DisarmAutoMerge, OperationState::Started);
    assert_eq!(reconcile(&disarm, &pr(PrState::Open, HEAD, false)), Verdict::Confirmed);
    assert!(matches!(reconcile(&disarm, &pr(PrState::Open, HEAD, true)), Verdict::Failed { .. }));
    assert!(matches!(reconcile(&disarm, &pr(PrState::Merged, HEAD, false)), Verdict::Failed { .. }));
}

#[test]
fn no_answer_is_unknown_never_a_guess() {
    for kind in [OperationKind::OpenPr, OperationKind::Merge, OperationKind::ArmAutoMerge] {
        let v = reconcile(&op(kind, OperationState::Started), &Observation::Unreachable("timeout".into()));
        assert!(matches!(v, Verdict::Unknown { .. }), "{kind:?}: {v:?}");
    }
    // A landing whose pull request the forge cannot find is unknown too.
    let v = reconcile(&op(OperationKind::Merge, OperationState::Started), &Observation::NoPr);
    assert!(matches!(v, Verdict::Unknown { .. }));
}

#[test]
fn opening_a_pull_request_is_confirmed_only_at_the_expected_head() {
    let open = op(OperationKind::OpenPr, OperationState::Started);
    assert_eq!(reconcile(&open, &pr(PrState::Open, HEAD, false)), Verdict::Confirmed);
    assert!(matches!(reconcile(&open, &pr(PrState::Open, OTHER, false)), Verdict::Failed { .. }));
    assert!(matches!(reconcile(&open, &Observation::NoPr), Verdict::Failed { .. }));
}

proptest! {
    /// Whatever the forge shows, a landing is confirmed only for the head it
    /// was pinned to, full or abbreviated to at least 7 hex digits.
    #[test]
    fn only_the_pinned_head_ever_lands(
        pinned in "[0-9a-f]{40}",
        other in "[0-9a-f]{40}",
        cut in 0usize..=40,
        pinned_cut in 7usize..=40,
        seen_kind in 0u8..3,
        state in prop_oneof![Just(PrState::Open), Just(PrState::Closed), Just(PrState::Merged)],
        auto_merge: bool,
        kind in prop_oneof![Just(OperationKind::Merge), Just(OperationKind::ArmAutoMerge)],
        started: bool,
    ) {
        // The forge may report the pinned head, a prefix of it (possibly too short), or another head.
        let seen_head = match seen_kind {
            0 => pinned.clone(),
            1 => pinned[..cut].to_string(),
            _ => other.clone(),
        };
        let state_of_op = if started { OperationState::Started } else { OperationState::Planned };
        let op = op_at(kind, state_of_op, &pinned[..pinned_cut]);
        let v = reconcile(&op, &pr(state, &seen_head, auto_merge));
        if let Verdict::Land { report: MergeReport::Merged { head_sha }, .. } = v {
            prop_assert!(head_sha.len() >= 7, "an id shorter than 7 digits never lands");
            prop_assert!(pinned.starts_with(&head_sha) || head_sha.starts_with(&pinned[..pinned_cut]));
            prop_assert!(seen_kind != 2 || other.starts_with(&pinned[..pinned_cut]));
        }
    }

    /// Distinct task ids never share a branch, and every branch name is one git accepts.
    #[test]
    fn branch_names_are_injective_and_valid(id in "[A-Za-z0-9][A-Za-z0-9._:-]{0,40}") {
        let branch = branch_for(&id);
        prop_assert_eq!(task_for_branch(&branch), Some(id.clone()));
        let name = branch.strip_prefix("interlock/").unwrap();
        prop_assert!(!name.contains("..") && !name.contains(':') && !name.ends_with('.') && !name.ends_with(".lock"));
        prop_assert!(!name.starts_with('.') && !name.starts_with('-'));
    }
}

#[test]
fn every_call_needs_landing_authority_at_the_time_of_the_call() {
    let task = integrating();
    let planned = op(OperationKind::Merge, OperationState::Planned);
    assert!(delivery::authorize(&task, &planned, LandingAuthority::Coordinator).is_ok());
    assert!(delivery::authorize(&task, &planned, LandingAuthority::Owner).is_ok());
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
        delivery::authorize(&task, &started, LandingAuthority::Coordinator).unwrap_err().code,
        RefusalCode::WrongState
    );
    let mut verified = task.clone();
    verified.state = State::Verified;
    assert_eq!(
        delivery::authorize(&verified, &planned, LandingAuthority::Coordinator).unwrap_err().code,
        RefusalCode::WrongState
    );
    assert_eq!(delivery::action_class(OperationKind::OpenPr), ActionClass::ExternalReversible);
    assert_eq!(delivery::action_class(OperationKind::Merge), ActionClass::Landing);
    // Withdrawing an armed auto-merge needs no authority.
    let disarm = op(OperationKind::DisarmAutoMerge, OperationState::Planned);
    assert!(delivery::authorize(&task, &disarm, LandingAuthority::None).is_ok());
}

#[test]
fn with_operator_authority_interlock_opens_the_pull_request_but_never_merges() {
    let task = integrating();
    let open = op(OperationKind::OpenPr, OperationState::Planned);
    assert!(delivery::authorize(&task, &open, LandingAuthority::Operator).is_ok());
    for kind in [OperationKind::Merge, OperationKind::ArmAutoMerge] {
        let refused =
            delivery::authorize(&task, &op(kind, OperationState::Planned), LandingAuthority::Operator).unwrap_err();
        assert_eq!(refused.code, RefusalCode::IntegrationNotAllowed, "{kind:?}");
    }
    assert!(!delivery::interlock_merges(LandingAuthority::Operator));
    assert!(delivery::interlock_merges(LandingAuthority::Coordinator));
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
            bound_via: None,
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
            bound_via: None,
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
fn branch_names_escape_what_git_refuses_and_stay_distinct() {
    assert_eq!(branch_for("fix-add"), "interlock/fix-add");
    assert_eq!(branch_for("team:fix..add."), "interlock/team%3Afix%2E%2Eadd%2E");
    assert_eq!(branch_for("x.lock"), "interlock/x%2Elock");
    assert_eq!(branch_for("v1.2-fix"), "interlock/v1.2-fix");
    // These collided before: `:` and `.` sequences used to fold into `-`.
    assert_ne!(branch_for("a:b"), branch_for("a-b"));
    assert_ne!(branch_for("a..b"), branch_for("a-b"));
}
