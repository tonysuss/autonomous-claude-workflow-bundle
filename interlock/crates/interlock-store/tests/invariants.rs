//! Fault-injection tests for the design's invariants 1-5, run against a real
//! SQLite store. Each must pass before phase 2 closes.

use chrono::{Duration, TimeZone, Utc};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, Signal, TaskSpec};
use interlock_core::workflow::Mode;
use interlock_schema::*;
use interlock_store::*;

const TREE_A: &str = "aaaaaaa1111111111111111111111111111111111";
const TREE_B: &str = "bbbbbbb2222222222222222222222222222222222";

fn t(m: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap() + Duration::minutes(m)
}

fn caps() -> CapabilitySet {
    [
        Capability::SessionStart,
        Capability::SessionCollect,
        Capability::SessionCancel,
        Capability::ToolRestriction,
        Capability::CustomAgents,
        Capability::ModelSelection,
        Capability::Parallel,
    ]
    .into_iter()
    .collect()
}

fn store_with_task(max_attempts: u32) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    let spec: TaskSpec = serde_json::from_value(serde_json::json!({
        "id": "export-retry",
        "repository": "/work/repo",
        "workflow": "bug-fix",
        "intent": "Fix duplicate rows when an export retries",
        "environment": "linux-x86_64",
        "budget": {"max_attempts": max_attempts},
        "criteria": [
            {"id": "repro", "statement": "Retrying an export produces no duplicate rows",
             "check": "checks/export-retry.sh", "min_strength": "observed", "producer": "independent"},
            {"id": "regression", "statement": "The regression suite passes",
             "min_strength": "tested", "producer": "self"}
        ]
    }))
    .unwrap();
    store.create_task(spec, t(0)).unwrap();
    store
        .ready(
            "export-retry",
            Snapshot { repository: "/work/repo".into(), base_commit: "e43c7ee".into(), untracked_hash: None },
            t(1),
        )
        .unwrap();
    store
}

struct Handle {
    id: String,
    token: String,
    epoch: u32,
}

fn start(store: &mut Store, role: Role, at: i64) -> Handle {
    let started = store
        .start_attempt(
            StartAttempt {
                task_id: "export-retry".into(),
                role,
                mode: Mode::Headless,
                host: HostRef { host: "test".into(), version: "0".into() },
                capabilities: caps(),
                profile: Profile::Conservative,
                host_policy: HostPolicy::open(),
                agent: None,
                model: None,
                worktree: None,
            },
            t(at),
        )
        .unwrap();
    match started {
        Started::Yes { attempt, token, .. } => Handle { id: attempt.id, token, epoch: attempt.epoch },
        Started::Blocked { moved } => panic!("blocked: {}", moved.reason),
    }
}

fn submit(store: &mut Store, h: &Handle, tree: &str, event: Option<&str>, at: i64) -> Result<Applied<ResultApplied>> {
    store.submit_result(
        SubmitResult {
            attempt_id: h.id.clone(),
            token: h.token.clone(),
            epoch: h.epoch,
            output_tree: tree.into(),
            changed_paths: vec!["src/export/retry.rs".into()],
            summary: "made retries idempotent".into(),
            open_questions: vec![],
            event_id: event.map(str::to_string),
        },
        t(at),
    )
}

fn evidence(
    store: &mut Store,
    kind: EvidenceKind,
    h: &Handle,
    criterion: &str,
    s: Strength,
    tree: &str,
    at: i64,
) -> Result<Applied<EvidenceApplied>> {
    store.add_evidence(
        kind,
        AddEvidence {
            attempt_id: h.id.clone(),
            token: h.token.clone(),
            criterion_id: criterion.into(),
            strength: s,
            tree: tree.into(),
            environment: None,
            evidence_refs: vec![],
            note: None,
            event_id: None,
        },
        t(at),
    )
}

fn state(store: &Store) -> State {
    store.task("export-retry").unwrap().state
}

/// Worker submits TREE_A and claims the regression suite passes.
fn to_awaiting(store: &mut Store) -> Handle {
    let w = start(store, Role::Worker, 2);
    submit(store, &w, TREE_A, None, 3).unwrap();
    evidence(store, EvidenceKind::Claim, &w, "regression", Strength::Tested, TREE_A, 4).unwrap();
    assert_eq!(state(store), State::AwaitingVerification);
    w
}

#[test]
fn invariant_1_no_current_evidence_no_pass() {
    let mut store = store_with_task(3);
    to_awaiting(&mut store);
    let v = start(&mut store, Role::Verifier, 5);
    // The verifier checked a different tree: kept, but it does not count.
    let applied = evidence(&mut store, EvidenceKind::Assessment, &v, "repro", Strength::Observed, TREE_B, 6).unwrap();
    assert!(!applied.outcome.current);
    assert!(store.advance("export-retry", t(7)).unwrap().is_empty());
    assert_eq!(state(&store), State::AwaitingVerification);
    assert_eq!(store.assessments("export-retry").unwrap().len(), 1, "stale evidence is kept");
}

#[test]
fn invariant_2_worker_cannot_override_a_verifier_failure() {
    let mut store = store_with_task(3);
    let w = to_awaiting(&mut store);
    let v = start(&mut store, Role::Verifier, 5);
    evidence(&mut store, EvidenceKind::Assessment, &v, "repro", Strength::Failed, TREE_A, 6).unwrap();
    // The worker then reports a pass for the same criterion and tree.
    evidence(&mut store, EvidenceKind::Claim, &w, "repro", Strength::Observed, TREE_A, 7).unwrap();
    // And tries to record an assessment with its own token.
    let err = evidence(&mut store, EvidenceKind::Assessment, &w, "repro", Strength::Observed, TREE_A, 8).unwrap_err();
    assert!(matches!(err, StoreError::Refused(ref r) if r.code == interlock_core::RefusalCode::WrongRole), "{err}");
    // The failure stands: the task goes back for rework, never to verified.
    let moves = store.advance("export-retry", t(9)).unwrap();
    assert_eq!(moves[0].signal, Signal::R1);
    assert_eq!(state(&store), State::Ready);
}

#[test]
fn invariant_2_records_cannot_be_rewritten_in_place() {
    let mut store = store_with_task(3);
    to_awaiting(&mut store);
    // Row triggers fire per row, so give every table at least one.
    let v = start(&mut store, Role::Verifier, 5);
    evidence(&mut store, EvidenceKind::Assessment, &v, "repro", Strength::Failed, TREE_A, 6).unwrap();
    let conn = store.connection();
    for sql in [
        "UPDATE assessments SET record = '{}'",
        "UPDATE claims SET record = '{}'",
        "DELETE FROM claims",
        "DELETE FROM assessments",
        "UPDATE results SET status = 'accepted'",
        "DELETE FROM events",
        "DELETE FROM transitions",
    ] {
        let err = conn.execute(sql, []).unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
}

#[test]
fn invariant_2_tokens_and_grants() {
    let mut store = store_with_task(3);
    let w = start(&mut store, Role::Worker, 2);
    let forged = Handle { id: w.id.clone(), token: "0".repeat(64), epoch: w.epoch };
    assert!(matches!(submit(&mut store, &forged, TREE_A, None, 3), Err(StoreError::BadToken(_))));
    let err = store
        .create_grant(
            NewGrant {
                principal: "worker".into(),
                task_scope: vec!["*".into()],
                action_classes: vec![ActionClass::Irreversible],
                tools: ToolPolicy::default(),
                landing_authority: LandingAuthority::None,
                origin: "self-granted".into(),
                expires_at: None,
            },
            t(3),
        )
        .unwrap_err();
    assert!(err.to_string().contains("irreversible"));
}

#[test]
fn invariant_3_a_rebase_after_verification_sends_the_task_back() {
    let mut store = store_with_task(3);
    to_awaiting(&mut store);
    let v = start(&mut store, Role::Verifier, 5);
    evidence(&mut store, EvidenceKind::Assessment, &v, "repro", Strength::Observed, TREE_A, 6).unwrap();
    // Without delivery, verification finishes the task.
    assert!(!store.task("export-retry").unwrap().integration_required);
    let moves = store.advance("export-retry", t(7)).unwrap();
    let signals: Vec<Signal> = moves.iter().map(|m| m.signal).collect();
    assert_eq!(signals, vec![Signal::G4, Signal::G7], "no delivery needed: verified, then done");

    // Same setup with delivery required, then a rebase before landing.
    let mut store = store_with_task(3);
    {
        let conn = store.connection();
        let mut task = interlock_store_task(conn);
        task.integration_required = true;
        conn.execute("UPDATE tasks SET record = ?1 WHERE id = 'export-retry'", [serde_json::to_string(&task).unwrap()])
            .unwrap();
    }
    to_awaiting(&mut store);
    let v = start(&mut store, Role::Verifier, 5);
    evidence(&mut store, EvidenceKind::Assessment, &v, "repro", Strength::Observed, TREE_A, 6).unwrap();
    store.advance("export-retry", t(7)).unwrap();
    assert_eq!(state(&store), State::Verified);
    store.record_new_tree("export-retry", TREE_B, t(8)).unwrap();
    let moves = store.advance("export-retry", t(9)).unwrap();
    assert_eq!(moves[0].signal, Signal::R2);
    assert_eq!(state(&store), State::AwaitingVerification);
    // Landing is refused while the evidence is stale.
    assert!(store.begin_integration("export-retry", OperationKind::Merge, OperationIntent::default(), t(10)).is_err());
}

fn interlock_store_task(conn: &rusqlite::Connection) -> Task {
    let rec: String = conn.query_row("SELECT record FROM tasks WHERE id = 'export-retry'", [], |r| r.get(0)).unwrap();
    serde_json::from_str(&rec).unwrap()
}

#[test]
fn invariant_4_a_late_worker_cannot_advance_the_task() {
    let mut store = store_with_task(3);
    let w1 = start(&mut store, Role::Worker, 2);
    // The supervisor cancels the first worker and respawns.
    assert_eq!(store.retry("export-retry", "attempt timed out", t(3)).unwrap().signal, Signal::R3);
    let w2 = start(&mut store, Role::Worker, 4);
    assert_eq!(w2.epoch, 2);
    // The first worker's result arrives late: stored as superseded, never applied.
    let late = submit(&mut store, &w1, TREE_A, None, 5).unwrap();
    assert_eq!(late.outcome.result.status, ResultStatus::Superseded);
    assert!(late.outcome.moved.is_none());
    assert_eq!(state(&store), State::Running);
    // Its claims are refused too.
    assert!(evidence(&mut store, EvidenceKind::Claim, &w1, "regression", Strength::Tested, TREE_A, 6).is_err());
    // The current worker's result is applied.
    let ok = submit(&mut store, &w2, TREE_B, None, 7).unwrap();
    assert_eq!(ok.outcome.result.status, ResultStatus::Accepted);
    assert_eq!(store.task("export-retry").unwrap().current_tree.as_deref(), Some(TREE_B));
}

#[test]
fn invariant_5_a_duplicate_event_is_a_no_op() {
    let mut store = store_with_task(3);
    let w = start(&mut store, Role::Worker, 2);
    let first = submit(&mut store, &w, TREE_A, Some("evt-1"), 3).unwrap();
    let second = submit(&mut store, &w, TREE_A, Some("evt-1"), 4).unwrap();
    assert!(!first.duplicate && second.duplicate);
    assert_eq!(first.outcome.result.id, second.outcome.result.id, "the first outcome is returned again");
    assert_eq!(store.results("export-retry").unwrap().len(), 1);
    assert_eq!(store.transitions("export-retry").unwrap().iter().filter(|r| r.signal == "G3").count(), 1);
}

#[test]
fn invariant_5_a_failure_mid_apply_leaves_nothing_then_replay_applies_once() {
    let mut store = store_with_task(3);
    let w = start(&mut store, Role::Worker, 2);
    let events_before = store.event_count("export-retry").unwrap();
    store.set_fault(Some(Fault::ErrorBeforeCommit));
    assert!(matches!(submit(&mut store, &w, TREE_A, Some("evt-9"), 3), Err(StoreError::Fault(_))));
    assert_eq!(state(&store), State::Running, "the effect rolled back with the event");
    assert!(store.results("export-retry").unwrap().is_empty());
    assert_eq!(store.event_count("export-retry").unwrap(), events_before);
    store.set_fault(None);
    let replay = submit(&mut store, &w, TREE_A, Some("evt-9"), 4).unwrap();
    assert!(!replay.duplicate);
    assert_eq!(state(&store), State::AwaitingVerification);
    assert!(submit(&mut store, &w, TREE_A, Some("evt-9"), 5).unwrap().duplicate);
}

#[test]
fn every_transition_is_logged_in_order() {
    let mut store = store_with_task(3);
    to_awaiting(&mut store);
    let rows = store.transitions("export-retry").unwrap();
    let path: Vec<(&str, &str)> = rows.iter().map(|r| (r.signal.as_str(), r.to.as_str())).collect();
    assert_eq!(path, vec![("G1", "ready"), ("G2", "running"), ("G3", "awaiting_verification")]);
}

#[test]
fn cancel_leaves_no_attempt_running() {
    let mut store = store_with_task(3);
    to_awaiting(&mut store);
    start(&mut store, Role::Verifier, 5);
    store.stop("export-retry", true, "operator cancelled", t(6)).unwrap();
    assert_eq!(state(&store), State::Cancelled);
    assert!(store.attempts("export-retry").unwrap().iter().all(|a| a.status != AttemptStatus::Running));
}

#[test]
fn reopening_a_file_store_keeps_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let mut store = Store::open(&path).unwrap();
        let spec: TaskSpec = serde_json::from_value(serde_json::json!({
            "id": "t1", "repository": ".", "workflow": "investigation", "intent": "Why is export slow?",
            "criteria": [{"id": "answer", "statement": "Root cause named with evidence", "min_strength": "static", "producer": "self"}]
        }))
        .unwrap();
        store.create_task(spec, t(0)).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.task("t1").unwrap().workflow.name, "investigation");
}
