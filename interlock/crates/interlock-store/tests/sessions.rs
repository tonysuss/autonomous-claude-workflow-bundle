//! Session bookkeeping: handoffs written before a session starts, restart
//! reconcile, classified ends, spending and budgets. P3's gate needs every
//! started attempt to end in a terminal row; these tests hold the store to it.

use chrono::{Duration, TimeZone, Utc};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, Signal, TaskSpec};
use interlock_core::workflow::Mode;
use interlock_schema::*;
use interlock_store::*;

const TREE_A: &str = "aaaaaaa1111111111111111111111111111111111";
const TREE_B: &str = "bbbbbbb2222222222222222222222222222222222";
const TASK: &str = "export-retry";

fn t(m: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap() + Duration::minutes(m)
}

fn caps() -> CapabilitySet {
    [Capability::SessionStart, Capability::SessionCollect, Capability::SessionCancel, Capability::ToolRestriction]
        .into_iter()
        .collect()
}

fn create(store: &mut Store, budget: serde_json::Value) {
    let spec: TaskSpec = serde_json::from_value(serde_json::json!({
        "id": TASK,
        "repository": "/work/repo",
        "workflow": "bug-fix",
        "intent": "Fix duplicate rows when an export retries",
        "environment": "linux-x86_64",
        "budget": budget,
        "criteria": [
            {"id": "repro", "statement": "Retrying an export produces no duplicate rows",
             "min_strength": "observed", "producer": "independent"}
        ]
    }))
    .unwrap();
    store.create_task(spec, t(0)).unwrap();
    let snapshot = Snapshot {
        repository: "/work/repo".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec![],
    };
    store.ready(TASK, snapshot, t(1)).unwrap();
}

fn store_with(budget: serde_json::Value) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    create(&mut store, budget);
    store
}

fn store() -> Store {
    store_with(serde_json::json!({"max_attempts": 3}))
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
                task_id: TASK.into(),
                role,
                mode: Mode::Headless,
                host: HostRef { host: "copilot".into(), version: "1.0.91".into() },
                capabilities: caps(),
                profile: Profile::Conservative,
                host_policy: HostPolicy::open(),
                agent: None,
                model: None,
                worktree: Some(format!("/work/wt/{role:?}-{at}")),
            },
            t(at),
        )
        .unwrap();
    match started {
        Started::Yes { attempt, token, .. } => Handle { id: attempt.id, token, epoch: attempt.epoch },
        Started::Blocked { moved } => panic!("blocked: {}", moved.reason),
    }
}

fn submit(store: &mut Store, h: &Handle, tree: &str, at: i64) -> Applied<ResultApplied> {
    store
        .submit_result(
            SubmitResult {
                attempt_id: h.id.clone(),
                token: h.token.clone(),
                epoch: h.epoch,
                output_tree: tree.into(),
                changed_paths: vec!["src/export/retry.rs".into()],
                summary: "made retries idempotent".into(),
                open_questions: vec![],
                event_id: None,
            },
            t(at),
        )
        .unwrap()
}

fn handoff(pid: u32, at: i64) -> Handoff {
    Handoff {
        host: "copilot".into(),
        pid,
        pgid: pid,
        process_start: Some(12_345),
        started_at: t(at),
        deadline: t(at + 20),
        budget_deadline: false,
        transcript: format!("/work/.interlock/transcripts/{pid}.jsonl"),
        host_session_id: Some("0cb916db-26aa-40f2-86b5-1ba81b225fd2".into()),
        supervisor_pid: 99,
        reattached_at: vec![],
    }
}

fn end(reason: EndReason, detail: &str) -> AttemptEnd {
    AttemptEnd { reason, detail: Some(detail.into()), synthetic: false }
}

fn ended(store: &mut Store, h: &Handle, status: Option<AttemptStatus>, end: AttemptEnd, wall_ms: u64) -> Attempt {
    let spent = Spent { wall_ms, cost_usd: Some(0.1), premium_requests: None, turns: Some(3) };
    store.end_session(&h.id, &h.token, SessionEnd { status, end, spent: Some(spent) }, t(9)).unwrap()
}

fn state(store: &Store) -> State {
    store.task(TASK).unwrap().state
}

fn attempt_events(store: &Store, attempt: &str) -> Vec<Event> {
    store.events(TASK).unwrap().into_iter().filter(|e| e.attempt_id.as_deref() == Some(attempt)).collect()
}

#[test]
fn reconcile_running_ends_every_running_attempt_with_a_synthetic_report() {
    let mut store = store();
    let w = start(&mut store, Role::Worker, 2);
    assert_eq!(state(&store), State::Running);

    let ended = store.reconcile_running(TASK, "the supervisor restarted; the session is gone", t(5)).unwrap();
    assert_eq!(ended.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec![w.id.as_str()]);
    let a = store.attempt(&w.id).unwrap();
    assert_eq!(a.status, AttemptStatus::Failed);
    assert_eq!(a.ended_at, Some(t(5)));
    let report = a.end.unwrap();
    assert_eq!((report.reason, report.synthetic), (EndReason::Crash, true));
    assert_eq!(report.detail.as_deref(), Some("the supervisor restarted; the session is gone"));
    let events = attempt_events(&store, &w.id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::AttemptFailed);
    assert_eq!(events[0].outcome.as_ref().unwrap()["synthetic"], true);
    assert_eq!(events[0].outcome.as_ref().unwrap()["reason"], "crash");
    // The store does not move the task; the supervisor applies R3.
    assert_eq!(state(&store), State::Running);

    // Reconciling again finds nothing and writes nothing.
    let count = store.event_count(TASK).unwrap();
    assert!(store.reconcile_running(TASK, "again", t(6)).unwrap().is_empty());
    assert_eq!(store.event_count(TASK).unwrap(), count);

    // R3, then a fresh attempt at the next epoch; the reconciled worker's late result is superseded.
    assert_eq!(store.retry(TASK, "reconciled after a supervisor restart", t(7)).unwrap().signal, Signal::R3);
    let w2 = start(&mut store, Role::Worker, 8);
    assert_eq!(w2.epoch, 2);
    let late = submit(&mut store, &w, TREE_A, 9);
    assert_eq!(late.outcome.result.status, ResultStatus::Superseded);
    assert_eq!(state(&store), State::Running);
}

#[test]
fn reconcile_running_leaves_submitted_and_finished_attempts_alone() {
    let mut store = store();
    let w = start(&mut store, Role::Worker, 2);
    submit(&mut store, &w, TREE_A, 3);
    let v = start(&mut store, Role::Verifier, 4);
    let ended = store.reconcile_running(TASK, "restart", t(5)).unwrap();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].id, v.id, "only the running verifier ends");
    assert_eq!(store.attempt(&w.id).unwrap().status, AttemptStatus::Submitted);
    assert_eq!(store.attempt(&v.id).unwrap().status, AttemptStatus::Failed);
    assert_eq!(state(&store), State::AwaitingVerification, "the work is kept for the next verifier");
}

#[test]
fn reconcile_after_a_crash_finds_what_the_dead_controller_left() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let id = {
        let mut store = Store::open(&path).unwrap();
        create(&mut store, serde_json::json!({"max_attempts": 2}));
        let w = start(&mut store, Role::Worker, 2);
        store.record_handoff(&w.id, &w.token, handoff(4242, 2)).unwrap();
        w.id
        // The controller dies here without ending the attempt.
    };
    let mut store = Store::open(&path).unwrap();
    let a = store.attempt(&id).unwrap();
    assert_eq!(a.handoff.as_ref().map(|h| h.pid), Some(4242), "the handoff survived the crash");
    let spent = Spent { wall_ms: 30_000, ..Spent::default() };
    let a = store
        .reconcile_attempt(
            &id,
            EndReason::Crash,
            "the session's process (pid 4242) was gone",
            Some(spent.clone()),
            t(3),
        )
        .unwrap();
    assert_eq!(a.status, AttemptStatus::Failed);
    assert_eq!(a.spent, Some(spent));
    assert!(a.end.unwrap().synthetic);
    // A second reconcile of the same attempt changes nothing.
    let again = store.reconcile_attempt(&id, EndReason::Timeout, "later", None, t(4)).unwrap();
    assert_eq!(again.end.unwrap().reason, EndReason::Crash);
    assert_eq!(store.retry(TASK, "the session was gone after a restart", t(5)).unwrap().to, State::Ready);
}

#[test]
fn an_attempt_gets_one_session_and_its_handoff_is_written_first() {
    let mut store = store();
    let w = start(&mut store, Role::Worker, 2);
    assert!(store.attempt(&w.id).unwrap().handoff.is_none());
    assert!(matches!(store.record_handoff(&w.id, &"0".repeat(64), handoff(10, 2)), Err(StoreError::BadToken(_))));
    let a = store.record_handoff(&w.id, &w.token, handoff(10, 2)).unwrap();
    assert_eq!(a.handoff.as_ref().unwrap().host_session_id.as_deref(), Some("0cb916db-26aa-40f2-86b5-1ba81b225fd2"));
    // A restart or a second supervisor cannot start another session for it.
    let err = store.record_handoff(&w.id, &w.token, handoff(11, 3)).unwrap_err();
    assert!(err.to_string().contains("never gets a second one"), "{err}");
    assert_eq!(store.attempt(&w.id).unwrap().handoff.unwrap().pid, 10);

    let a = store.note_reattach(&w.id, &w.token, t(4)).unwrap();
    assert_eq!(a.handoff.unwrap().reattached_at, vec![t(4)]);

    // No session starts for an attempt that has ended.
    store.retry(TASK, "timed out", t(5)).unwrap();
    let w2 = start(&mut store, Role::Worker, 6);
    store
        .end_session(
            &w2.id,
            &w2.token,
            SessionEnd { status: Some(AttemptStatus::Failed), end: end(EndReason::HostError, "x"), spent: None },
            t(7),
        )
        .unwrap();
    let err = store.record_handoff(&w2.id, &w2.token, handoff(12, 7)).unwrap_err();
    assert!(err.to_string().contains("no session may start"), "{err}");
}

#[test]
fn session_ends_are_classified_and_recorded_once() {
    let mut store = store();
    let w = start(&mut store, Role::Worker, 2);
    let a = ended(&mut store, &w, Some(AttemptStatus::Failed), end(EndReason::Timeout, "no result in 20m"), 1_200_000);
    assert_eq!((a.status, a.end.clone().unwrap().reason), (AttemptStatus::Failed, EndReason::Timeout));
    assert_eq!(a.spent.as_ref().unwrap().wall_ms, 1_200_000);
    // The first end stands.
    let again = ended(&mut store, &w, Some(AttemptStatus::Completed), end(EndReason::Completed, "late"), 5);
    assert_eq!(again.end.unwrap().reason, EndReason::Timeout);
    assert_eq!(again.status, AttemptStatus::Failed);
    let events = attempt_events(&store, &w.id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome.as_ref().unwrap()["reason"], "timeout");
    assert_eq!(events[0].outcome.as_ref().unwrap()["spent"]["wall_ms"], 1_200_000);
    assert!(
        store
            .end_session(
                &w.id,
                &w.token,
                SessionEnd { status: Some(AttemptStatus::Running), end: end(EndReason::Completed, ""), spent: None },
                t(9)
            )
            .is_err()
    );

    // A worker whose result was accepted stays submitted, with its session's end recorded.
    store.retry(TASK, "timed out", t(10)).unwrap();
    let w2 = start(&mut store, Role::Worker, 11);
    submit(&mut store, &w2, TREE_A, 12);
    let a = ended(&mut store, &w2, None, end(EndReason::Completed, "result accepted"), 60_000);
    assert_eq!(a.status, AttemptStatus::Submitted);
    assert_eq!(attempt_events(&store, &w2.id).iter().filter(|e| e.kind == EventKind::AttemptCompleted).count(), 1);

    // The operator cancels while a verifier runs: the attempt keeps its
    // cancelled status, and the supervisor still records why its session ended.
    let v = start(&mut store, Role::Verifier, 13);
    store.stop(TASK, true, "operator cancelled", t(14)).unwrap();
    let a = ended(
        &mut store,
        &v,
        Some(AttemptStatus::Cancelled),
        end(EndReason::Cancelled, "the operator cancelled the task"),
        3_000,
    );
    assert_eq!(a.status, AttemptStatus::Cancelled);
    assert_eq!(a.end.unwrap().reason, EndReason::Cancelled);
    assert_eq!(attempt_events(&store, &v.id)[0].kind, EventKind::AttemptCancelled);
}

#[test]
fn spending_is_summed_and_a_spent_budget_fails_the_task() {
    let mut store = store_with(serde_json::json!({"max_attempts": 5, "max_wall_secs": 60, "max_cost_usd": 0.25}));
    let w = start(&mut store, Role::Worker, 2);
    ended(&mut store, &w, Some(AttemptStatus::Failed), end(EndReason::Crash, "killed"), 30_000);
    assert_eq!(store.enforce_budget(TASK, t(3)).unwrap(), None, "30s and $0.10 of 60s and $0.25");
    store.retry(TASK, "crashed", t(4)).unwrap();
    let w2 = start(&mut store, Role::Worker, 5);
    ended(&mut store, &w2, Some(AttemptStatus::Failed), end(EndReason::Timeout, "slow"), 31_000);
    let spent = store.spent(TASK).unwrap();
    assert_eq!(spent.wall_ms, 61_000);
    assert!((spent.cost_usd.unwrap() - 0.2).abs() < 1e-9);
    let mv = store.enforce_budget(TASK, t(6)).unwrap().expect("the wall-clock budget is spent");
    assert_eq!((mv.signal, mv.to), (Signal::Fail, State::Failed));
    assert!(mv.reason.contains("wall-clock budget of 60s is spent"), "{}", mv.reason);
    assert!(store.attempts(TASK).unwrap().iter().all(|a| !a.status.is_open()), "nothing is left open");
    assert_eq!(store.enforce_budget(TASK, t(7)).unwrap(), None, "a failed task is not failed twice");

    let mut store = store_with(serde_json::json!({"max_attempts": 5, "max_cost_usd": 0.15}));
    let w = start(&mut store, Role::Worker, 2);
    submit(&mut store, &w, TREE_A, 3);
    ended(&mut store, &w, None, end(EndReason::Completed, "done"), 1_000);
    assert_eq!(store.enforce_budget(TASK, t(4)).unwrap(), None);
    let v = start(&mut store, Role::Verifier, 5);
    ended(&mut store, &v, Some(AttemptStatus::Completed), end(EndReason::Completed, "done"), 1_000);
    let mv = store.enforce_budget(TASK, t(6)).unwrap().unwrap();
    assert!(mv.reason.contains("cost budget of $0.15 is spent"), "{}", mv.reason);
    assert_eq!(store.attempt(&w.id).unwrap().status, AttemptStatus::Completed, "its result was judged");
}

#[test]
fn a_finished_task_leaves_no_attempt_open() {
    let mut store = store();
    let w = start(&mut store, Role::Worker, 2);
    submit(&mut store, &w, TREE_A, 3);
    let v = start(&mut store, Role::Verifier, 4);
    store
        .add_evidence(
            EvidenceKind::Assessment,
            AddEvidence {
                attempt_id: v.id.clone(),
                token: v.token.clone(),
                criterion_id: "repro".into(),
                strength: Strength::Observed,
                tree: TREE_A.into(),
                environment: None,
                evidence_refs: vec![],
                note: None,
                event_id: None,
            },
            t(5),
        )
        .unwrap();
    let signals: Vec<Signal> = store.advance(TASK, t(6)).unwrap().iter().map(|m| m.signal).collect();
    assert_eq!(signals, vec![Signal::G4, Signal::G7]);
    let statuses: Vec<AttemptStatus> = store.attempts(TASK).unwrap().iter().map(|a| a.status).collect();
    assert_eq!(statuses, vec![AttemptStatus::Completed, AttemptStatus::Completed]);
}

#[test]
fn resuming_sets_where_the_next_worker_starts() {
    let mut store = store();
    let task = store.resume_from(TASK, TREE_B, t(2)).unwrap();
    assert_eq!((task.state, task.current_tree.as_deref()), (State::Ready, Some(TREE_B)));
    start(&mut store, Role::Worker, 3);
    let err = store.resume_from(TASK, TREE_A, t(4)).unwrap_err();
    assert!(err.to_string().contains("needs the task ready"), "{err}");
}
