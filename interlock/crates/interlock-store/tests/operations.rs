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

/// A task at G5: integrating, with its landing operation planned.
fn integrating(store: &mut Store) -> Operation {
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
    store
        .create_grant(
            NewGrant {
                principal: "operator".into(),
                task_scope: vec!["t1".into()],
                action_classes: vec![ActionClass::Landing],
                tools: ToolPolicy::default(),
                landing_authority: LandingAuthority::Operator,
                origin: "ship it".into(),
                expires_at: None,
            },
            t(5),
        )
        .unwrap();
    let intent =
        OperationIntent { expected_head_sha: Some(HEAD.into()), base: Some("main".into()), pull_request: None };
    let (mv, op) = store.begin_integration("t1", OperationKind::Merge, intent, t(6)).unwrap();
    assert_eq!(mv.signal, Signal::G5);
    op.unwrap()
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
        let started = store.start_operation(&op.id, Some(7), t(7)).unwrap().unwrap();
        assert_eq!((started.state, started.intent.pull_request), (OperationState::Started, Some(7)));
        op.id
        // The process dies here, after the call and before anything was settled.
    };
    let mut store = Store::open(&path).unwrap();
    let open = store.open_operations(None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!((open[0].id.as_str(), open[0].state), (op_id.as_str(), OperationState::Started));
    let verdict = Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() } };
    let settled = store.settle_operation(&op_id, &verdict, json!({ "reconciled": true }), t(9)).unwrap();
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
    store.start_operation(&open_pr.id, None, t(7)).unwrap().unwrap();
    let busy = store.start_operation(&landing.id, Some(7), t(8)).unwrap_err();
    assert!(busy.to_string().contains("still in flight"), "{busy}");
    assert!(store.start_operation(&open_pr.id, None, t(8)).is_err(), "a started operation cannot start again");

    store.block("t1", "operator wants a look", t(9)).unwrap();
    assert!(store.plan_operation("t1", OperationKind::OpenPr, OperationIntent::default(), t(9)).is_err());
}

#[test]
fn a_revoked_grant_fails_the_operation_before_its_call_and_blocks_the_task() {
    let mut store = Store::open_in_memory().unwrap();
    let op = integrating(&mut store);
    let grant = store.grants().unwrap().pop().unwrap();
    store.revoke_grant(&grant.id, t(7)).unwrap();
    let Err(mv) = store.start_operation(&op.id, Some(7), t(8)).unwrap() else { panic!("must not start") };
    assert_eq!((mv.signal, mv.to), (Signal::Block, State::Blocked));
    assert!(mv.reason.starts_with("landing authority is no longer granted"), "{}", mv.reason);
    let op = store.operation(&op.id).unwrap();
    assert_eq!(op.state, OperationState::Failed);
    assert_eq!(op.outcome.unwrap()["called"], false);
}

#[test]
fn unknown_outcomes_block_until_every_one_is_known() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    let open_pr = store.plan_operation("t1", OperationKind::OpenPr, OperationIntent::default(), t(7)).unwrap();
    store.start_operation(&open_pr.id, None, t(7)).unwrap().unwrap();
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
    store.start_operation(&landing.id, Some(7), t(10)).unwrap().unwrap();
    let refused = Verdict::Land { report: MergeReport::Refused { reason: "head moved".into() } };
    let s = store.settle_operation(&landing.id, &refused, json!({}), t(11)).unwrap();
    assert_eq!((s.operation.state, s.moves[0].signal), (OperationState::Failed, Signal::R2));
    assert_eq!(state(&store), State::AwaitingVerification);
}

#[test]
fn an_operator_block_is_not_lifted_by_the_forge() {
    let mut store = Store::open_in_memory().unwrap();
    let landing = integrating(&mut store);
    store.start_operation(&landing.id, Some(7), t(7)).unwrap().unwrap();
    store.block("t1", "operator wants a look", t(8)).unwrap();
    let merged = Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() } };
    let s = store.settle_operation(&landing.id, &merged, json!({}), t(9)).unwrap();
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
