//! The program that wrote the v0 store fixture beside it (`state.db` and
//! `artifacts/`): a database with every record kind, in the states the
//! lifecycle leaves them, as the last build with v0 schemas (commit a2ec7b9)
//! writes it. `tests/v0_store.rs` opens it with the current build.
//!
//! The store keeps its database in WAL mode. The fixture's journal mode was
//! then switched to DELETE (`PRAGMA journal_mode=DELETE`, which leaves the
//! records as they are), so reading it in place leaves no `-wal` or `-shm`
//! file beside it; opening a copy with the store switches it back.
//!
//! Kept as a record, not compiled. To write it again, copy it to
//! `crates/interlock-store/tests/make_v0_fixture.rs` in a checkout of a2ec7b9
//! and run:
//!
//! INTERLOCK_FIXTURE_OUT=<dir> cargo test -p interlock-store --test make_v0_fixture -- --ignored

use chrono::{Duration, TimeZone, Utc};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::delivery::Verdict;
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, MergeReport, TaskSpec};
use interlock_core::workflow::Mode;
use interlock_schema::*;
use interlock_store::*;
use serde_json::json;

const TREE_1: &str = "aaaaaaa1111111111111111111111111111111111";
const TREE_2: &str = "bbbbbbb2222222222222222222222222222222222";
const BASE: &str = "e43c7ee0000000000000000000000000000000000";
const HEAD: &str = "c0ffee11111111111111111111111111111111111";

fn t(m: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap() + Duration::minutes(m)
}

struct Handle {
    id: String,
    token: String,
    epoch: u32,
}

fn caps() -> CapabilitySet {
    [
        Capability::SessionStart,
        Capability::SessionCollect,
        Capability::SessionCancel,
        Capability::EventStream,
        Capability::ToolRestriction,
        Capability::PerCallPolicy,
        Capability::StopGuard,
        Capability::ModelSelection,
        Capability::CustomAgents,
    ]
    .into_iter()
    .collect()
}

fn start(store: &mut Store, task: &str, role: Role, at: i64) -> Handle {
    let label = if role == Role::Worker { "worker" } else { "verifier" };
    let started = store
        .start_attempt(
            StartAttempt {
                task_id: task.into(),
                role,
                mode: Mode::Headless,
                host: HostRef { host: "copilot".into(), version: "1.0.91".into() },
                capabilities: caps(),
                profile: Profile::Conservative,
                host_policy: HostPolicy::open(),
                agent: Some(format!("interlock-{label}")),
                model: Some("gpt-4.1".into()),
                worktree: Some(format!("/work/repo/.interlock/worktrees/{task}-{label}-{at}")),
            },
            t(at),
        )
        .unwrap();
    let Started::Yes { attempt, token, .. } = started else { panic!("blocked") };
    store.bind_attempt(&attempt.id, Binding::interlock_launched()).unwrap();
    let handoff = Handoff {
        host: "copilot".into(),
        pid: 4000 + at as u32,
        pgid: 4000 + at as u32,
        process_start: Some(123_456),
        started_at: t(at),
        deadline: t(at + 20),
        budget_deadline: false,
        transcript: format!("/work/repo/.interlock/transcripts/{}.jsonl", attempt.id),
        host_session_id: Some(format!("0cb916db-26aa-40f2-86b5-1ba81b2250{at:02}")),
        supervisor_pid: 99,
        supervisor_start: Some(654_321),
        start_commit: Some(BASE.into()),
        env: vec!["PATH".into(), "HOME".into(), "INTERLOCK_ATTEMPT".into()],
        effort: Some("medium".into()),
        reattached_at: vec![],
    };
    store.record_handoff(&attempt.id, &token, handoff).unwrap();
    Handle { id: attempt.id, token, epoch: attempt.epoch }
}

fn submit(store: &mut Store, h: &Handle, epoch: u32, tree: &str, paths: &[&str], at: i64) -> ResultStatus {
    store
        .submit_result(
            SubmitResult {
                attempt_id: h.id.clone(),
                token: h.token.clone(),
                epoch,
                output_tree: tree.into(),
                changed_paths: paths.iter().map(|p| p.to_string()).collect(),
                summary: "made retries idempotent".into(),
                open_questions: vec!["should the retry limit be configurable?".into()],
                event_id: Some(format!("evt-result-{}-{at}", h.id)),
            },
            t(at),
        )
        .unwrap()
        .outcome
        .result
        .status
}

#[allow(clippy::too_many_arguments)]
fn evidence(store: &mut Store, kind: EvidenceKind, h: &Handle, criterion: &str, s: Strength, tree: &str, at: i64) {
    store
        .add_evidence(
            kind,
            AddEvidence {
                attempt_id: h.id.clone(),
                token: h.token.clone(),
                criterion_id: criterion.into(),
                strength: s,
                tree: tree.into(),
                environment: None,
                evidence_refs: vec!["sh checks/repro.sh".into()],
                note: (s == Strength::Failed).then(|| "the export still writes row 7 twice".to_string()),
                event_id: None,
            },
            t(at),
        )
        .unwrap();
}

fn check(store: &mut Store, task: &str, h: Option<&Handle>, target: RunTarget, tree: &str, exit: i32, out: &str) {
    store
        .record_check_run(
            NewCheckRun {
                task_id: task.into(),
                criterion_id: "repro".into(),
                attempt: h.map(|h| (h.id.clone(), h.token.clone())),
                target,
                tree: tree.into(),
                exit_code: Some(exit),
                timed_out: false,
                duration_ms: 42,
                output: out.as_bytes().to_vec(),
            },
            t(5),
        )
        .unwrap();
}

fn end(store: &mut Store, h: &Handle, status: Option<AttemptStatus>, reason: EndReason, at: i64) {
    let spent = Spent {
        wall_ms: 61_000,
        cost_usd: None,
        premium_requests: Some(2.0),
        turns: Some(7),
        model: Some("gpt-4.1".into()),
    };
    let end = AttemptEnd { reason, detail: Some("session ended".into()), synthetic: false };
    store.end_session(&h.id, &h.token, SessionEnd { status, end, spent: Some(spent) }, t(at)).unwrap();
}

fn spec(value: serde_json::Value) -> TaskSpec {
    serde_json::from_value(value).unwrap()
}

fn snapshot(protected: &[&str]) -> Snapshot {
    Snapshot {
        repository: "/work/repo".into(),
        base_commit: BASE.into(),
        untracked_hash: Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".into()),
        protected_paths: protected.iter().map(|p| p.to_string()).collect(),
    }
}

fn grant(store: &mut Store, task: &str, authority: LandingAuthority, at: i64) -> Grant {
    store
        .create_grant(
            NewGrant {
                principal: "operator".into(),
                task_scope: vec![task.into()],
                action_classes: vec![ActionClass::ExternalReversible, ActionClass::Landing],
                tools: ToolPolicy { allow: vec!["shell:gh pr view".into()], deny: vec!["web".into()] },
                landing_authority: authority,
                origin: "land it once verified".into(),
                expires_at: Some(t(24 * 60)),
            },
            t(at),
        )
        .unwrap()
}

#[test]
#[ignore = "writes the v0 fixture; run by hand against the v0 build"]
fn write_the_v0_fixture() {
    let out = std::path::PathBuf::from(std::env::var("INTERLOCK_FIXTURE_OUT").expect("INTERLOCK_FIXTURE_OUT"));
    let mut store = Store::open(&out.join("state.db")).unwrap();
    let checked = json!([
        {"id": "repro", "statement": "Retrying an export writes each row once", "check": "sh checks/repro.sh",
         "min_strength": "observed", "producer": "independent", "baseline": "fails"},
        {"id": "regression", "statement": "The export suite passes", "min_strength": "tested", "producer": "self"}
    ]);

    // 1. Landed: G1 to G6 through a pinned merge, with check runs, a rejected result and spending.
    store
        .create_task(
            spec(json!({
                "id": "landed", "repository": "/work/repo", "workflow": "bug-fix",
                "intent": "Fix duplicate rows when an export retries", "integration_required": true,
                "scope": {"paths": ["src/**"], "description": "the export module"},
                "budget": {"max_attempts": 3, "max_wall_secs": 3600, "max_cost_usd": 5.0, "max_premium_requests": 40},
                "environment": "linux-x86_64", "criteria": checked
            })),
            t(0),
        )
        .unwrap();
    store.ready("landed", snapshot(&["checks/repro.sh"]), t(1)).unwrap();
    check(&mut store, "landed", None, RunTarget::Base, BASE, 1, "AssertionError: row 7 written twice\n");
    let w = start(&mut store, "landed", Role::Worker, 2);
    assert_eq!(submit(&mut store, &w, w.epoch, TREE_1, &["README.md"], 3), ResultStatus::Rejected);
    assert_eq!(submit(&mut store, &w, w.epoch, TREE_1, &["src/export/retry.py"], 4), ResultStatus::Accepted);
    evidence(&mut store, EvidenceKind::Claim, &w, "regression", Strength::Tested, TREE_1, 4);
    end(&mut store, &w, None, EndReason::Completed, 5);
    let v = start(&mut store, "landed", Role::Verifier, 6);
    check(&mut store, "landed", Some(&v), RunTarget::Output, TREE_1, 0, "Ran 3 tests\nOK\n");
    evidence(&mut store, EvidenceKind::Assessment, &v, "repro", Strength::Observed, TREE_1, 7);
    evidence(&mut store, EvidenceKind::Assessment, &v, "regression", Strength::Tested, TREE_1, 7);
    store.end_attempt(&v.id, &v.token, false, Some("both criteria hold".into()), t(8)).unwrap();
    end(&mut store, &v, None, EndReason::Completed, 8);
    store.advance("landed", t(9)).unwrap();
    let revoked = grant(&mut store, "landed", LandingAuthority::Owner, 9);
    store.revoke_grant(&revoked.id, t(10)).unwrap();
    grant(&mut store, "landed", LandingAuthority::Coordinator, 10);
    let intent = OperationIntent { expected_head_sha: Some(HEAD.into()), base: Some("main".into()), pull_request: None };
    let (_, op) = store.begin_integration("landed", OperationKind::Merge, intent, t(11)).unwrap();
    let op = op.unwrap();
    let pin = Pin { pull_request: Some(7), base: Some("main".into()), tree: Some(TREE_1.into()) };
    store.start_operation(&op.id, &pin, t(12)).unwrap().unwrap();
    let merged = Verdict::Land { report: MergeReport::Merged { head_sha: HEAD.into() }, merged_at: Some(t(12)) };
    store.settle_operation(&op.id, &merged, json!({"state": "MERGED", "headRefOid": HEAD}), t(13)).unwrap();

    // 2. Reworked: a failed verification sends the work back (R1); the first worker's late result is superseded.
    store
        .create_task(
            spec(json!({
                "id": "reworked", "repository": "/work/repo", "workflow": "bug-fix",
                "intent": "Retry limit off by one", "scope": {"paths": ["src/**"]}, "criteria": checked
            })),
            t(20),
        )
        .unwrap();
    store.ready("reworked", snapshot(&["checks/repro.sh"]), t(21)).unwrap();
    check(&mut store, "reworked", None, RunTarget::Base, BASE, 1, "FAILED (failures=1)\n");
    let w1 = start(&mut store, "reworked", Role::Worker, 22);
    submit(&mut store, &w1, w1.epoch, TREE_1, &["src/export/retry.py"], 23);
    end(&mut store, &w1, None, EndReason::Completed, 23);
    let v1 = start(&mut store, "reworked", Role::Verifier, 24);
    check(&mut store, "reworked", Some(&v1), RunTarget::Output, TREE_1, 0, "Ran 0 tests in 0.000s\n\nOK\n");
    evidence(&mut store, EvidenceKind::Assessment, &v1, "repro", Strength::Failed, TREE_1, 25);
    end(&mut store, &v1, Some(AttemptStatus::Completed), EndReason::Completed, 25);
    store.advance("reworked", t(26)).unwrap();
    let w2 = start(&mut store, "reworked", Role::Worker, 27);
    assert_eq!(submit(&mut store, &w1, w1.epoch, TREE_1, &["src/export/retry.py"], 28), ResultStatus::Superseded);
    assert_eq!(submit(&mut store, &w2, w2.epoch, TREE_2, &["src/export/retry.py"], 29), ResultStatus::Accepted);
    evidence(&mut store, EvidenceKind::Claim, &w2, "regression", Strength::Tested, TREE_2, 29);
    end(&mut store, &w2, None, EndReason::Completed, 30);
    let v2 = start(&mut store, "reworked", Role::Verifier, 31);
    check(&mut store, "reworked", Some(&v2), RunTarget::Output, TREE_2, 0, "Ran 4 tests\nOK\n");
    evidence(&mut store, EvidenceKind::Assessment, &v2, "repro", Strength::Observed, TREE_2, 32);
    end(&mut store, &v2, Some(AttemptStatus::Completed), EndReason::Completed, 32);
    store.advance("reworked", t(33)).unwrap();
    assert_eq!(store.task("reworked").unwrap().state, State::Done);

    // 3. Blocked at ready: a crashed session reconciled with a synthetic report, R3, resumed from exported work.
    store
        .create_task(
            spec(json!({
                "id": "paused", "repository": "/work/repo", "workflow": "feature", "intent": "Add CSV export",
                "scope": {"paths": ["src/**", "tests/**"]},
                "criteria": [{"id": "csv", "statement": "CSV export works", "min_strength": "tested", "producer": "self"}]
            })),
            t(40),
        )
        .unwrap();
    store.ready("paused", snapshot(&[]), t(41)).unwrap();
    let w = start(&mut store, "paused", Role::Worker, 42);
    let detail = "the session's process (pid 4042) was gone when the supervisor restarted";
    store.reconcile_attempt(&w.id, EndReason::Crash, detail, None, t(43)).unwrap();
    store.retry("paused", "the session crashed", t(43)).unwrap();
    store.resume_from("paused", HEAD, TREE_2, t(44)).unwrap();
    store.block("paused", "waiting for the operator to review the exported work", t(45)).unwrap();

    // 4. Cancelled: an investigation whose dependency is done, cancelled mid-attempt.
    store
        .create_task(
            spec(json!({
                "id": "asked", "repository": "/work/repo", "workflow": "investigation",
                "intent": "Why does the export retry the whole batch?", "dependencies": ["reworked"],
                "criteria": [{"id": "answer", "statement": "The answer cites the code",
                              "min_strength": "observed", "producer": "independent"}]
            })),
            t(50),
        )
        .unwrap();
    store.ready("asked", snapshot(&[]), t(51)).unwrap();
    start(&mut store, "asked", Role::Worker, 52);
    store.stop("asked", true, "operator cancelled", t(53)).unwrap();

    // 5. Pending: its dependency is not done.
    store
        .create_task(
            spec(json!({
                "id": "waiting", "repository": "/work/repo", "workflow": "refactor", "intent": "Split the exporter",
                "dependencies": ["paused"], "scope": {"paths": ["src/**"]},
                "criteria": [{"id": "same", "statement": "Behaviour is unchanged", "min_strength": "tested",
                              "producer": "independent"}]
            })),
            t(60),
        )
        .unwrap();

    let report = json!({
        "host": "copilot", "contract_version": 1, "installed": true, "binary": "/usr/local/bin/copilot",
        "version": "1.0.91", "auth": "unknown", "capabilities": [], "notes": []
    });
    store.save_host_report("copilot", &report, t(61)).unwrap();
    store.connection().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
}
