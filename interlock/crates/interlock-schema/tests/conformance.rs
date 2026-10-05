//! Every Rust record must serialize to JSON its schema accepts, and the
//! schemas must reject the shapes the design forbids.

use chrono::{TimeZone, Utc};
use interlock_schema::*;
use serde_json::json;

fn validators() -> Validators {
    Validators::new().expect("shipped schemas load")
}

fn t0() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap()
}

fn currency() -> Currency {
    Currency {
        tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        check_version: "1".into(),
        environment: "linux-x86_64".into(),
        policy_digest: "sha256:abc".into(),
    }
}

fn grant_view() -> EffectiveGrant {
    EffectiveGrant {
        action_classes: vec![ActionClass::Read, ActionClass::LocalReversible],
        tools: ToolPolicy { allow: vec!["Read".into()], deny: vec!["Bash(git push:*)".into()] },
        landing_authority: LandingAuthority::None,
        grant_ids: vec![],
    }
}

fn task() -> Task {
    Task {
        id: "export-retry".into(),
        repository: "/work/repo".into(),
        workflow: WorkflowRef { name: "bug-fix".into(), version: 1 },
        intent: "Fix duplicate rows when an export retries".into(),
        scope: Scope { paths: vec!["src/export/**".into()], description: None },
        dependencies: vec![],
        criteria: vec![Criterion {
            id: "repro".into(),
            statement: "Retrying an export produces no duplicate rows".into(),
            check: Some("checks/export-retry.sh".into()),
            check_version: "1".into(),
            min_strength: MinStrength::Observed,
            producer: Producer::Independent,
        }],
        input_snapshot: Some(Snapshot {
            repository: "/work/repo".into(),
            base_commit: "e43c7ee".into(),
            untracked_hash: None,
        }),
        environment: "linux-x86_64".into(),
        policy_digest: "sha256:abc".into(),
        budget: Budget { max_attempts: 3 },
        integration_required: false,
        state: State::Pending,
        resume_point: None,
        blocked_reason: None,
        current_tree: None,
        lease_epoch: 0,
        current_attempt: None,
        attempts_used: 0,
        created_at: t0(),
        updated_at: t0(),
    }
}

#[test]
fn every_record_kind_round_trips_through_its_schema() {
    let v = validators();
    let task = task();
    v.check(RecordKind::Task, &task).unwrap();
    v.check(RecordKind::Criterion, &task.criteria[0]).unwrap();

    let attempt = Attempt {
        id: "att-1".into(),
        task_id: task.id.clone(),
        epoch: 1,
        role: Role::Worker,
        host: HostRef { host: "claude-code".into(), version: "2.1.289".into() },
        agent: Some("worker".into()),
        model: None,
        worktree: Some("/work/wt/att-1".into()),
        effective_grant: grant_view(),
        token_hash: "0".repeat(64),
        status: AttemptStatus::Running,
        started_at: t0(),
        ended_at: None,
    };
    v.check(RecordKind::Attempt, &attempt).unwrap();

    let result = TaskResult {
        id: "res-1".into(),
        task_id: task.id.clone(),
        attempt_id: attempt.id.clone(),
        epoch: 1,
        output_tree: currency().tree,
        changed_paths: vec!["src/export/retry.rs".into()],
        summary: "Made the retry idempotent".into(),
        open_questions: vec![],
        status: ResultStatus::Accepted,
        superseded_reason: None,
        received_at: t0(),
    };
    v.check(RecordKind::Result, &result).unwrap();

    let evidence = Evidence {
        id: "ev-1".into(),
        task_id: task.id.clone(),
        criterion_id: "repro".into(),
        attempt_id: attempt.id.clone(),
        strength: Strength::Observed,
        currency: currency(),
        evidence_refs: vec!["artifacts/run.log".into()],
        note: None,
        recorded_at: t0(),
    };
    v.check(RecordKind::Claim, &evidence).unwrap();
    v.check(RecordKind::Assessment, &evidence).unwrap();

    let event = Event {
        id: "evt-1".into(),
        task_id: task.id.clone(),
        attempt_id: Some(attempt.id.clone()),
        epoch: Some(1),
        kind: EventKind::ResultSubmitted,
        payload_ref: None,
        received_at: t0(),
        acknowledged: true,
        outcome: Some(json!({"applied": true})),
    };
    v.check(RecordKind::Event, &event).unwrap();

    let grant = Grant {
        id: "g-1".into(),
        principal: "operator".into(),
        task_scope: vec![task.id.clone()],
        action_classes: vec![ActionClass::ExternalReversible],
        tools: ToolPolicy::default(),
        landing_authority: LandingAuthority::Operator,
        origin: "going to bed, keep going".into(),
        created_at: t0(),
        expires_at: Some(t0()),
        revoked_at: None,
    };
    v.check(RecordKind::Grant, &grant).unwrap();

    let op = Operation {
        id: "op-1".into(),
        task_id: task.id.clone(),
        kind: OperationKind::Merge,
        intent: OperationIntent {
            expected_head_sha: Some(currency().tree),
            base: Some("main".into()),
            pull_request: Some(42),
        },
        state: OperationState::Planned,
        outcome: None,
        created_at: t0(),
        updated_at: t0(),
    };
    v.check(RecordKind::Operation, &op).unwrap();
}

#[test]
fn no_grant_can_cover_the_irreversible_class() {
    let v = validators();
    let mut grant = serde_json::to_value(Grant {
        id: "g-2".into(),
        principal: "operator".into(),
        task_scope: vec!["*".into()],
        action_classes: vec![ActionClass::Read],
        tools: ToolPolicy::default(),
        landing_authority: LandingAuthority::None,
        origin: "test".into(),
        created_at: t0(),
        expires_at: None,
        revoked_at: None,
    })
    .unwrap();
    grant["action_classes"] = json!(["read", "irreversible"]);
    assert!(v.validate(RecordKind::Grant, &grant).is_err());
}

#[test]
fn unknown_fields_and_states_are_rejected() {
    let v = validators();
    let mut value = serde_json::to_value(task()).unwrap();
    value["state"] = json!("almost_done");
    assert!(v.validate(RecordKind::Task, &value).is_err());

    let mut value = serde_json::to_value(task()).unwrap();
    value["done_by_worker"] = json!(true);
    assert!(v.validate(RecordKind::Task, &value).is_err());
}

#[test]
fn evidence_must_carry_a_full_currency_key() {
    let v = validators();
    let mut value = json!({
        "id": "ev-2", "task_id": "t", "criterion_id": "c", "attempt_id": "a",
        "strength": "tested",
        "currency": {"tree": "abcdef1", "check_version": "1", "environment": ""},
        "evidence_refs": [], "recorded_at": "2026-10-05T12:00:00Z"
    });
    assert!(v.validate(RecordKind::Claim, &value).is_err(), "missing policy_digest");
    value["currency"]["policy_digest"] = json!("sha256:abc");
    v.validate(RecordKind::Claim, &value).unwrap();
}
