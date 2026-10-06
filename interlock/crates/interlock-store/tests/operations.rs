//! Forge operations in the store: planned and started in their own
//! transactions before any call, settled from what the forge reports, and
//! still there for reconcile after a crash.

use chrono::{Duration, TimeZone, Utc};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::delivery::{UNKNOWN_OUTCOME, Verdict};
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, MergeReport, Signal, TaskSpec};
use interlock_core::workflow::Mode;
use interlock_schema::*;
use interlock_store::*;
use serde_json::json;

const TREE: &str = "aaaaaaa1111111111111111111111111111111111";
const HEAD: &str = "c0ffee11111111111111111111111111111111111";

fn t(m: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap() + Duration::minutes(m)
}

fn start(store: &mut Store, role: Role) -> (String, String, u32) {
    let caps: CapabilitySet =
        [Capability::SessionStart, Capability::SessionCollect, Capability::SessionCancel, Capability::ToolRestriction]
            .into_iter()
            .collect();
    let started = store
        .start_attempt(
            StartAttempt {
                task_id: "t1".into(),
                role,
                mode: Mode::Headless,
                host: HostRef { host: "test".into(), version: "0".into() },
                capabilities: caps,
                profile: Profile::Conservative,
                host_policy: HostPolicy::open(),
                agent: None,
                model: None,
                worktree: None,
            },
            t(2),
        )
        .unwrap();
    let Started::Yes { attempt, token, .. } = started else { panic!("blocked") };
    (attempt.id, token, attempt.epoch)
}

fn add(store: &mut Store, kind: EvidenceKind, a: &(String, String, u32), criterion: &str, s: Strength) {
    store
        .add_evidence(
            kind,
            AddEvidence {
                attempt_id: a.0.clone(),
                token: a.1.clone(),
                criterion_id: criterion.into(),
                strength: s,
                tree: TREE.into(),
                environment: None,
                evidence_refs: vec![],
                note: None,
                event_id: None,
            },
            t(4),
        )
        .unwrap();
}

fn grant(store: &mut Store, authority: LandingAuthority, at: i64) -> Grant {
    store
        .create_grant(
            NewGrant {
                principal: "operator".into(),
                task_scope: vec!["t1".into()],
                action_classes: vec![ActionClass::Landing],
                tools: ToolPolicy::default(),
                landing_authority: authority,
                origin: "ship it".into(),
                expires_at: None,
            },
            t(at),
        )
        .unwrap()
}

/// A task at G5 under the given landing authority: integrating, with its landing operation planned.
fn integrating_with(store: &mut Store, authority: LandingAuthority) -> Operation {
    let spec: TaskSpec = serde_json::from_value(json!({
        "id": "t1", "repository": "/r", "workflow": "bug-fix", "intent": "fix it", "integration_required": true,
        "criteria": [
            {"id": "fixed", "statement": "fixed", "min_strength": "tested", "producer": "self"},
            {"id": "checked", "statement": "checked", "min_strength": "observed", "producer": "independent"}
        ]
    }))
    .unwrap();
    store.create_task(spec, t(0)).unwrap();
    let snapshot = Snapshot {
        repository: "/r".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec![],
    };
    store.ready("t1", snapshot, t(1)).unwrap();
    let w = start(store, Role::Worker);
    store
        .submit_result(
            SubmitResult {
                attempt_id: w.0.clone(),
                token: w.1.clone(),
                epoch: w.2,
                output_tree: TREE.into(),
                changed_paths: vec![],
                summary: "done".into(),
                open_questions: vec![],
                event_id: None,
            },
            t(3),
        )
        .unwrap();
    add(store, EvidenceKind::Claim, &w, "fixed", Strength::Tested);
    let v = start(store, Role::Verifier);
    add(store, EvidenceKind::Assessment, &v, "checked", Strength::Observed);
    store.advance("t1", t(5)).unwrap();
    grant(store, authority, 5);
    let intent = OperationIntent {
        expected_head_sha: Some(HEAD.into()),
        base: Some("main".into()),
        pull_request: None,
        tree: None,
    };
    let (mv, op) = store.begin_integration("t1", OperationKind::Merge, intent, t(6)).unwrap();
    assert_eq!(mv.signal, Signal::G5);
    op.unwrap()
}

fn integrating(store: &mut Store) -> Operation {
    integrating_with(store, LandingAuthority::Coordinator)
}

fn pin(pr: Option<u64>) -> Pin {
    Pin { pull_request: pr, base: Some("main".into()), tree: Some(TREE.into()) }
}

fn merged() -> Verdict {
    Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() }, merged_at: None }
}

fn state(store: &Store) -> State {
    store.task("t1").unwrap().state
}

#[test]
fn a_started_operation_survives_a_crash_for_reconcile_to_find() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let op_id = {
        let mut store = Store::open(&path).unwrap();
        let op = integrating(&mut store);
        let started = store.start_operation(&op.id, &pin(Some(7)), t(7)).unwrap().unwrap();
        assert_eq!((started.state, started.intent.pull_request), (OperationState::Started, Some(7)));
        op.id
        // The store is closed here, after the call and before anything was settled.
    };
    let mut store = Store::open(&path).unwrap();
    let open = store.open_operations(None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!((open[0].id.as_str(), open[0].state), (op_id.as_str(), OperationState::Started));
    let settled = store.settle_operation(&op_id, &merged(), json!({ "reconciled": true }), t(9)).unwrap();
    assert_eq!(settled.operation.state, OperationState::Confirmed);
    assert_eq!(settled.moves[0].signal, Signal::G6);
    assert_eq!(state(&store), State::Done);
    assert!(store.open_operations(None).unwrap().is_empty());
}

#[test]
fn only_integrating_tasks_plan_operations_and_one_runs_at_a_time() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    let open_pr = store.plan_operation("t1", OperationKind::OpenPr, OperationIntent::default(), t(7)).unwrap();
    store.start_operation(&open_pr.id, &pin(None), t(7)).unwrap().unwrap();
    let busy = store.start_operation(&landing.id, &pin(Some(7)), t(8)).unwrap_err();
    assert!(busy.to_string().contains("still in flight"), "{busy}");
    assert!(store.start_operation(&open_pr.id, &pin(None), t(8)).is_err(), "a started operation cannot start again");

    store.block("t1", "operator wants a look", t(9)).unwrap();
    assert!(store.plan_operation("t1", OperationKind::OpenPr, OperationIntent::default(), t(9)).is_err());
}

#[test]
fn stale_evidence_refuses_the_call_and_applies_r2_in_the_same_transaction() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    // The code under evidence changes while integrating, as `interlock task tree` records after a rebase.
    store.record_new_tree("t1", "bbbbbbb2222222222222222222222222222222222", t(7)).unwrap();
    let Err(mv) = store.start_operation(&landing.id, &pin(Some(7)), t(8)).unwrap() else {
        panic!("an operation must not start on stale evidence")
    };
    assert_eq!((mv.signal, mv.to), (Signal::R2, State::AwaitingVerification));
    let op = store.operation(&landing.id).unwrap();
    assert_eq!(op.state, OperationState::Failed);
    assert_eq!(op.outcome.unwrap()["called"], false);
}

#[test]
fn a_head_built_from_another_tree_is_r2_even_with_current_evidence() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    let stale = Pin { tree: Some("ddddddd4444444444444444444444444444444444".into()), ..pin(Some(7)) };
    let Err(mv) = store.start_operation(&landing.id, &stale, t(8)).unwrap() else {
        panic!("the head must have been built from the task's current tree")
    };
    assert_eq!(mv.signal, Signal::R2);
    assert_eq!(store.operation(&landing.id).unwrap().state, OperationState::Failed);
}

#[test]
fn a_revoked_grant_fails_the_operation_before_its_call_and_blocks_the_task() {
    let mut store = Store::open_in_memory().unwrap();
    let op = integrating(&mut store);
    let grant = store.grants().unwrap().pop().unwrap();
    store.revoke_grant(&grant.id, t(7)).unwrap();
    let Err(mv) = store.start_operation(&op.id, &pin(Some(7)), t(8)).unwrap() else { panic!("must not start") };
    assert_eq!((mv.signal, mv.to), (Signal::Block, State::Blocked));
    assert!(mv.reason.starts_with("landing authority is no longer granted"), "{}", mv.reason);
    let op = store.operation(&op.id).unwrap();
    assert_eq!(op.state, OperationState::Failed);
    assert_eq!(op.outcome.unwrap()["called"], false);
}

#[test]
fn operator_authority_never_lets_interlock_start_a_merge() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating_with(&mut store, LandingAuthority::Operator);
    let err = store.start_operation(&landing.id, &pin(Some(7)), t(8)).unwrap_err();
    assert!(err.to_string().contains("the operator merges"), "{err}");
    assert_eq!(store.operation(&landing.id).unwrap().state, OperationState::Planned);
    // The operator's merge, once observed, still lands the task.
    store.pin_operation(&landing.id, &pin(Some(7)), t(9)).unwrap();
    assert_eq!(store.pinned_landings(None).unwrap().len(), 1);
    let s = store.settle_operation(&landing.id, &merged(), json!({}), t(10)).unwrap();
    assert_eq!(s.moves[0].signal, Signal::G6);
}

#[test]
fn a_merge_made_after_landing_authority_ended_is_recorded_but_not_g6() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    store.start_operation(&landing.id, &pin(Some(7)), t(7)).unwrap().unwrap();
    let grant = store.grants().unwrap().pop().unwrap();
    store.revoke_grant(&grant.id, t(8)).unwrap();
    let late = Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() }, merged_at: Some(t(9)) };
    let s = store.settle_operation(&landing.id, &late, json!({}), t(10)).unwrap();
    assert_eq!(s.operation.state, OperationState::Confirmed, "the forge did merge");
    let task = store.task("t1").unwrap();
    assert_eq!(task.state, State::Blocked);
    assert!(task.blocked_reason.unwrap().contains("when no landing authority was granted"));
}

#[test]
fn a_merge_made_before_the_grant_ended_still_lands() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    store.start_operation(&landing.id, &pin(Some(7)), t(7)).unwrap().unwrap();
    let grant = store.grants().unwrap().pop().unwrap();
    store.revoke_grant(&grant.id, t(9)).unwrap();
    let early = Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() }, merged_at: Some(t(8)) };
    let s = store.settle_operation(&landing.id, &early, json!({}), t(10)).unwrap();
    assert_eq!(s.moves[0].signal, Signal::G6);
}

#[test]
fn a_merge_never_lands_a_task_whose_evidence_went_stale() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    store.start_operation(&landing.id, &pin(Some(7)), t(7)).unwrap().unwrap();
    store.record_new_tree("t1", "bbbbbbb2222222222222222222222222222222222", t(8)).unwrap();
    let s = store.settle_operation(&landing.id, &merged(), json!({}), t(9)).unwrap();
    assert_eq!(s.operation.state, OperationState::Confirmed);
    assert_ne!(state(&store), State::Done);
    assert!(store.task("t1").unwrap().blocked_reason.unwrap().contains("no longer covers"));
}

#[test]
fn a_merge_confirmed_by_hand_never_lands_a_short_head_or_a_changed_tree() {
    // `interlock integrate confirm` reaches G6 without settle's checks; the
    // guard itself must refuse what the lifecycle property tests found.
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    assert_eq!(landing.intent.tree.as_deref(), Some(TREE), "G5 records the tree the head is built from");
    let short = MergeReport::Merged { head_sha: HEAD[..3].into() };
    let err = store.confirm_integration("t1", &landing.id, short, t(7)).unwrap_err();
    assert!(err.to_string().contains("verified head was"), "{err}");
    store.record_new_tree("t1", "bbbbbbb2222222222222222222222222222222222", t(8)).unwrap();
    let merged = MergeReport::Merged { head_sha: HEAD.into() };
    let mv = store.confirm_integration("t1", &landing.id, merged, t(9)).unwrap();
    assert_eq!((mv.signal, mv.to), (Signal::Block, State::Blocked));
    assert_eq!(store.operation(&landing.id).unwrap().state, OperationState::Confirmed, "what the forge did is kept");
    assert!(store.task("t1").unwrap().blocked_reason.unwrap().contains("the task's tree is now"));
}

/// Records a claim on a given tree.
fn claim_on(store: &mut Store, a: &(String, String, u32), criterion: &str, tree: &str) {
    let req = AddEvidence {
        attempt_id: a.0.clone(),
        token: a.1.clone(),
        criterion_id: criterion.into(),
        strength: Strength::Tested,
        tree: tree.into(),
        environment: None,
        evidence_refs: vec![],
        note: None,
        event_id: None,
    };
    store.add_evidence(EvidenceKind::Claim, req, t(4)).unwrap();
}

#[test]
fn a_hand_confirmed_merge_of_the_old_head_never_lands_a_newer_tree() {
    // Found in review: G5 pins the head to TREE; a new tree is recorded and
    // passes; the operator confirms TREE's head. That landed TREE on the
    // newer tree's evidence.
    let mut store = Store::open_in_memory().unwrap();
    let spec: TaskSpec = serde_json::from_value(json!({
        "id": "t1", "repository": "/r", "workflow": "bug-fix", "intent": "fix it", "integration_required": true,
        "criteria": [{"id": "fixed", "statement": "fixed", "min_strength": "tested", "producer": "self"}]
    }))
    .unwrap();
    store.create_task(spec, t(0)).unwrap();
    let snapshot = Snapshot {
        repository: "/r".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec![],
    };
    store.ready("t1", snapshot, t(1)).unwrap();
    let w = start(&mut store, Role::Worker);
    let result = SubmitResult {
        attempt_id: w.0.clone(),
        token: w.1.clone(),
        epoch: w.2,
        output_tree: TREE.into(),
        changed_paths: vec![],
        summary: "done".into(),
        open_questions: vec![],
        event_id: None,
    };
    store.submit_result(result, t(3)).unwrap();
    claim_on(&mut store, &w, "fixed", TREE);
    store.advance("t1", t(5)).unwrap();
    grant(&mut store, LandingAuthority::Coordinator, 5);
    let (_, op) = store.begin_integration("t1", OperationKind::Merge, OperationIntent::default(), t(6)).unwrap();
    let op = op.unwrap();
    assert_eq!((op.intent.expected_head_sha.as_deref(), op.intent.tree.as_deref()), (Some(TREE), Some(TREE)));

    let newer = "bbbbbbb2222222222222222222222222222222222";
    store.record_new_tree("t1", newer, t(7)).unwrap();
    claim_on(&mut store, &w, "fixed", newer);
    assert!(store.evaluate("t1").unwrap().1.all_pass, "the newer tree passes");
    let mv = store.confirm_integration("t1", &op.id, MergeReport::Merged { head_sha: TREE.into() }, t(9)).unwrap();
    assert_eq!((mv.signal, mv.to), (Signal::Block, State::Blocked));
    let task = store.task("t1").unwrap();
    assert!(task.blocked_reason.unwrap().contains("the task's tree is now"), "the reason names the change");
    assert_eq!(store.operation(&op.id).unwrap().state, OperationState::Confirmed, "what the forge did is kept");
}

#[test]
fn a_hand_confirmed_merge_refuses_an_operation_already_settled() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    store.start_operation(&landing.id, &pin(Some(7)), t(7)).unwrap().unwrap();
    store
        .settle_operation(&landing.id, &Verdict::Failed { reason: "the call failed".into() }, json!({}), t(8))
        .unwrap();
    assert_eq!(state(&store), State::Integrating);
    let merged = MergeReport::Merged { head_sha: HEAD.into() };
    let err = store.confirm_integration("t1", &landing.id, merged.clone(), t(9)).unwrap_err();
    assert!(err.to_string().contains("already Failed"), "{err}");
    assert_eq!(state(&store), State::Integrating, "nothing moved");

    let other = integrating_again(&mut store);
    store.confirm_integration("t1", &other.id, merged.clone(), t(10)).unwrap();
    assert_eq!(state(&store), State::Done);
    let err = store.confirm_integration("t1", &other.id, merged, t(11)).unwrap_err();
    assert!(err.to_string().contains("already Confirmed"), "{err}");
}

/// A second landing operation for the integrating task in `store`.
fn integrating_again(store: &mut Store) -> Operation {
    let intent = OperationIntent { expected_head_sha: Some(HEAD.into()), ..OperationIntent::default() };
    store.plan_operation("t1", OperationKind::Merge, intent, t(9)).unwrap()
}

#[test]
fn a_hand_confirmed_merge_needs_landing_authority_when_it_is_reported() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    for g in store.grants().unwrap() {
        store.revoke_grant(&g.id, t(7)).unwrap();
    }
    let mv = store.confirm_integration("t1", &landing.id, MergeReport::Merged { head_sha: HEAD.into() }, t(8)).unwrap();
    assert_eq!((mv.signal, mv.to), (Signal::Block, State::Blocked));
    assert!(store.task("t1").unwrap().blocked_reason.unwrap().contains("no landing authority"));
    assert_eq!(store.operation(&landing.id).unwrap().state, OperationState::Confirmed, "what the forge did is kept");
}

#[test]
fn settled_operations_are_final() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    let pending = Verdict::Pending { reason: "x".into() };
    assert!(store.settle_operation(&landing.id, &pending, json!({}), t(7)).is_err(), "a planned op cannot be pending");
    store.start_operation(&landing.id, &pin(Some(7)), t(7)).unwrap().unwrap();
    store.settle_operation(&landing.id, &merged(), json!({}), t(8)).unwrap();
    let again = store.settle_operation(&landing.id, &Verdict::Failed { reason: "x".into() }, json!({}), t(9));
    assert!(again.unwrap_err().to_string().contains("already Confirmed"));
}

#[test]
fn unknown_outcomes_block_until_every_one_is_known() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    let open_pr = store.plan_operation("t1", OperationKind::OpenPr, OperationIntent::default(), t(7)).unwrap();
    store.start_operation(&open_pr.id, &pin(None), t(7)).unwrap().unwrap();
    let unknown = Verdict::Unknown { reason: "no answer".into() };
    let s = store.settle_operation(&open_pr.id, &unknown, json!({}), t(8)).unwrap();
    assert_eq!(s.operation.state, OperationState::Unknown);
    let task = store.task("t1").unwrap();
    assert_eq!((task.state, task.resume_point), (State::Blocked, Some(State::Integrating)));
    assert!(task.blocked_reason.unwrap().starts_with(UNKNOWN_OUTCOME));

    // The forge answers: the pull request exists. The block lifts and work resumes.
    let s = store.settle_operation(&open_pr.id, &Verdict::Confirmed, json!({}), t(9)).unwrap();
    assert_eq!(s.moves.iter().map(|m| m.signal).collect::<Vec<_>>(), [Signal::Unblock]);
    assert_eq!(state(&store), State::Integrating);

    // A refusal settles the landing operation: R2.
    store.start_operation(&landing.id, &pin(Some(7)), t(10)).unwrap().unwrap();
    let refused = Verdict::Land { report: MergeReport::Refused { reason: "head moved".into() }, merged_at: None };
    let s = store.settle_operation(&landing.id, &refused, json!({}), t(11)).unwrap();
    assert_eq!((s.operation.state, s.moves[0].signal), (OperationState::Failed, Signal::R2));
    assert_eq!(state(&store), State::AwaitingVerification);
}

#[test]
fn an_operator_block_is_not_lifted_by_the_forge() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    store.start_operation(&landing.id, &pin(Some(7)), t(7)).unwrap().unwrap();
    store.block("t1", "operator wants a look", t(8)).unwrap();
    let s = store.settle_operation(&landing.id, &merged(), json!({}), t(9)).unwrap();
    assert_eq!(s.operation.state, OperationState::Confirmed, "what the forge did is kept");
    assert!(s.moves.is_empty());
    assert_eq!(state(&store), State::Blocked);
}

#[test]
fn a_moved_base_records_the_new_input_and_sends_the_task_back() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    let new_tree = "bbbbbbb2222222222222222222222222222222222";
    let s = store.rebase_and_withdraw(&landing.id, "f00dbabe", new_tree, "the base moved", t(7)).unwrap();
    assert_eq!((s.operation.state, s.moves[0].signal), (OperationState::Failed, Signal::R2));
    let task = store.task("t1").unwrap();
    assert_eq!(task.state, State::AwaitingVerification);
    assert_eq!(task.current_tree.as_deref(), Some(new_tree));
    assert_eq!(task.input_snapshot.unwrap().base_commit, "f00dbabe");
    let (_, report) = store.evaluate("t1").unwrap();
    assert!(!report.all_pass, "the old evidence no longer counts");
    assert!(store.advance("t1", t(8)).unwrap().is_empty(), "no G4 until the new tree is verified");
}
