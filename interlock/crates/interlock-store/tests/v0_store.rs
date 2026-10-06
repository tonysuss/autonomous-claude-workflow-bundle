//! A store written by the last build with v0 schemas opens with this build,
//! reads unchanged, conforms to the v1 schemas, and takes new moves. The
//! fixture (`fixtures/v0-store`) was written by that build; `make.rs` beside
//! it is the program that wrote it.

use std::path::{Path, PathBuf};

use chrono::{Duration, TimeZone, Utc};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::Signal;
use interlock_core::workflow::Mode;
use interlock_schema::*;
use interlock_store::*;

fn t(m: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap() + Duration::minutes(m)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A copy of the fixture as a repository's `.interlock` directory: opening a
/// store writes to it.
fn v0_store() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0-store");
    let store_dir = dir.path().join(".interlock");
    copy_dir(&fixture.join("artifacts"), &store_dir.join("artifacts"));
    std::fs::copy(fixture.join("state.db"), store_dir.join("state.db")).unwrap();
    (dir, store_dir)
}

const TABLES: [(&str, RecordKind); 9] = [
    ("tasks", RecordKind::Task),
    ("attempts", RecordKind::Attempt),
    ("results", RecordKind::Result),
    ("claims", RecordKind::Claim),
    ("assessments", RecordKind::Assessment),
    ("events", RecordKind::Event),
    ("grants", RecordKind::Grant),
    ("operations", RecordKind::Operation),
    ("check_runs", RecordKind::CheckRun),
];

#[test]
fn every_record_the_v0_build_wrote_conforms_to_v1_as_stored() {
    let (_dir, store_dir) = v0_store();
    let store = Store::open(&store_dir.join("state.db")).unwrap();
    let v = store.validators();
    let mut seen = 0;
    for (table, kind) in TABLES {
        let mut stmt = store.connection().prepare(&format!("SELECT record FROM {table}")).unwrap();
        let rows: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert!(!rows.is_empty(), "the fixture has {table}");
        for raw in rows {
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert!(value.get("$id").is_none() && value.get("$schema").is_none(), "records name no schema version");
            v.validate(kind, &value).unwrap_or_else(|e| panic!("{table}: {e}\n{raw}"));
            if kind == RecordKind::Task {
                for c in value["criteria"].as_array().unwrap() {
                    v.validate(RecordKind::Criterion, c).unwrap();
                }
            }
            seen += 1;
        }
    }
    assert_eq!(seen, 5 + 8 + 5 + 2 + 4 + 20 + 2 + 1 + 5);
}

#[test]
fn a_v0_store_reads_unchanged_and_its_evidence_still_counts() {
    let (_dir, store_dir) = v0_store();
    let store = Store::open(&store_dir.join("state.db")).unwrap();
    let states: Vec<(String, State)> = store.tasks().unwrap().into_iter().map(|t| (t.id, t.state)).collect();
    assert_eq!(
        states,
        [
            ("landed".to_string(), State::Done),
            ("reworked".to_string(), State::Done),
            ("paused".to_string(), State::Blocked),
            ("asked".to_string(), State::Cancelled),
            ("waiting".to_string(), State::Pending),
        ]
    );
    let signals = |id: &str| store.transitions(id).unwrap().into_iter().map(|r| r.signal).collect::<Vec<_>>();
    assert_eq!(signals("landed"), ["G1", "G2", "G3", "G4", "G5", "G6"]);
    assert_eq!(signals("reworked"), ["G1", "G2", "G3", "R1", "G2", "G3", "G4", "G7"]);

    // Every typed record decodes and, written again by this build, still conforms.
    let v = store.validators();
    for task in store.tasks().unwrap() {
        v.check(RecordKind::Task, &task).unwrap();
        store.attempts(&task.id).unwrap().iter().for_each(|a| v.check(RecordKind::Attempt, a).unwrap());
        store.results(&task.id).unwrap().iter().for_each(|r| v.check(RecordKind::Result, r).unwrap());
        store.claims(&task.id).unwrap().iter().for_each(|c| v.check(RecordKind::Claim, c).unwrap());
        store.assessments(&task.id).unwrap().iter().for_each(|a| v.check(RecordKind::Assessment, a).unwrap());
        store.events(&task.id).unwrap().iter().for_each(|e| v.check(RecordKind::Event, e).unwrap());
        store.operations(&task.id).unwrap().iter().for_each(|o| v.check(RecordKind::Operation, o).unwrap());
        for run in store.check_runs(&task.id).unwrap() {
            v.check(RecordKind::CheckRun, &run).unwrap();
            let output = run.output_ref.as_deref().expect("output kept");
            assert!(store_dir.join(output).is_file(), "{output} is beside the store");
        }
        store.brief(&task.id, Role::Worker, Profile::Conservative, &HostPolicy::open(), t(0)).unwrap();
    }
    store.grants().unwrap().iter().for_each(|g| v.check(RecordKind::Grant, g).unwrap());

    // The policy digest and currency keys did not change, so evidence recorded under v0 still decides.
    let (_, report) = store.evaluate("landed").unwrap();
    assert!(report.all_pass, "{report:?}");
    let (_, report) = store.evaluate("reworked").unwrap();
    assert!(report.all_pass, "{report:?}");
    let vacuous = store.check_runs("reworked").unwrap().into_iter().filter(|r| r.vacuous.is_some()).count();
    assert_eq!(vacuous, 1, "a run that tested nothing is still marked");
    let attempts = store.attempts("landed").unwrap();
    assert!(attempts.iter().all(|a| a.binding.as_ref().is_some_and(|b| b.via == BoundVia::InterlockLaunched)));
    assert_eq!(attempts[0].spent.as_ref().and_then(|s| s.model.as_deref()), Some("gpt-4.1"));
    assert!(store.host_report("copilot").unwrap().is_some());
    let unended: Vec<String> = store.unended_sessions().unwrap().into_iter().map(|a| a.task_id).collect();
    assert_eq!(unended, ["asked"], "the cancelled task's session still needs reconciling");
}

#[test]
fn a_v0_store_takes_new_moves() {
    let (_dir, store_dir) = v0_store();
    let mut store = Store::open(&store_dir.join("state.db")).unwrap();
    assert_eq!(store.unblock("paused", t(0)).unwrap().to, State::Ready);
    let caps: CapabilitySet =
        [Capability::SessionStart, Capability::SessionCollect, Capability::SessionCancel, Capability::ToolRestriction]
            .into_iter()
            .collect();
    let started = store
        .start_attempt(
            StartAttempt {
                task_id: "paused".into(),
                role: Role::Worker,
                mode: Mode::Headless,
                host: HostRef { host: "copilot".into(), version: "1.0.91".into() },
                capabilities: caps,
                profile: Profile::Conservative,
                host_policy: HostPolicy::open(),
                agent: Some("interlock-worker".into()),
                model: None,
                worktree: None,
            },
            t(1),
        )
        .unwrap();
    let Started::Yes { attempt, moved, .. } = started else { panic!("blocked") };
    assert_eq!((attempt.epoch, moved.map(|m| m.signal)), (2, Some(Signal::G2)));
    let grant = store.grants().unwrap().into_iter().find(|g| g.revoked_at.is_none()).unwrap();
    store.revoke_grant(&grant.id, t(2)).unwrap();
    store.stop("waiting", true, "no longer needed", t(3)).unwrap();
    drop(store);

    // Reopened, the store holds both the v0 records and the new ones.
    let store = Store::open(&store_dir.join("state.db")).unwrap();
    assert_eq!(store.task("paused").unwrap().state, State::Running);
    assert_eq!(store.task("waiting").unwrap().state, State::Cancelled);
    assert!(store.grants().unwrap().iter().all(|g| g.revoked_at.is_some()));
}
