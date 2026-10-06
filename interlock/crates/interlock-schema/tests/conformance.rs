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
            baseline: Baseline::Fails,
        }],
        input_snapshot: Some(Snapshot {
            repository: "/work/repo".into(),
            base_commit: "e43c7ee".into(),
            untracked_hash: None,
            protected_paths: vec!["checks/export-retry.sh".into()],
        }),
        environment: "linux-x86_64".into(),
        policy_digest: "sha256:abc".into(),
        budget: Budget::attempts(3),
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
        handoff: None,
        end: None,
        spent: None,
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

#[test]
fn check_runs_conform_and_pass_only_when_they_checked_something() {
    let v = validators();
    let mut run = CheckRun {
        id: "run-1".into(),
        task_id: "export-retry".into(),
        criterion_id: "repro".into(),
        attempt_id: Some("att-1".into()),
        producer: RunProducer::Verifier,
        target: RunTarget::Output,
        tree: currency().tree,
        check_version: "1".into(),
        environment: "linux-x86_64".into(),
        command: "sh checks/export-retry.sh".into(),
        exit_code: Some(0),
        timed_out: false,
        vacuous: None,
        duration_ms: 120,
        output_ref: Some("artifacts/sha256/ab".into()),
        output_tail: "ok".into(),
        recorded_at: t0(),
    };
    v.check(RecordKind::CheckRun, &run).unwrap();
    assert!(run.passed() && !run.failed());
    run.vacuous = Some("unittest: Ran 0 tests".into());
    assert!(!run.passed() && !run.failed(), "a run that checked nothing neither passes nor fails");
    run.vacuous = None;
    run.exit_code = None;
    run.timed_out = true;
    assert!(!run.passed() && !run.failed());
    v.check(RecordKind::CheckRun, &run).unwrap();
}

fn running_attempt() -> Attempt {
    Attempt {
        id: "att-2".into(),
        task_id: "export-retry".into(),
        epoch: 2,
        role: Role::Worker,
        host: HostRef { host: "copilot".into(), version: "1.0.91".into() },
        agent: None,
        model: None,
        worktree: Some("/work/wt/att-2".into()),
        effective_grant: grant_view(),
        token_hash: "0".repeat(64),
        status: AttemptStatus::Running,
        started_at: t0(),
        ended_at: None,
        handoff: None,
        end: None,
        spent: None,
    }
}

#[test]
fn session_handoff_end_and_spending_conform() {
    let v = validators();
    let mut attempt = running_attempt();
    // Optional fields that are absent are not written, so older records still match.
    let value = serde_json::to_value(&attempt).unwrap();
    assert!(value.get("handoff").is_none() && value.get("end").is_none() && value.get("spent").is_none());
    v.check(RecordKind::Attempt, &attempt).unwrap();

    attempt.handoff = Some(Handoff {
        host: "copilot".into(),
        pid: 4242,
        pgid: 4242,
        process_start: Some(987_654),
        started_at: t0(),
        deadline: t0() + chrono::Duration::minutes(20),
        budget_deadline: false,
        transcript: "/repo/.interlock/transcripts/att-2.jsonl".into(),
        host_session_id: Some("0cb916db-26aa-40f2-86b5-1ba81b225fd2".into()),
        supervisor_pid: 4200,
        supervisor_start: Some(77),
        start_commit: Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".into()),
        env: vec!["PATH".into(), "INTERLOCK_ATTEMPT".into()],
        effort: Some("low".into()),
        reattached_at: vec![t0() + chrono::Duration::minutes(3)],
    });
    attempt.status = AttemptStatus::Cancelled;
    attempt.ended_at = Some(t0() + chrono::Duration::minutes(4));
    attempt.end = Some(AttemptEnd {
        reason: EndReason::Cancelled,
        detail: Some("interrupted by SIGINT".into()),
        synthetic: false,
    });
    attempt.spent = Some(Spent { wall_ms: 240_000, cost_usd: None, premium_requests: Some(1.0), turns: Some(3) });
    v.check(RecordKind::Attempt, &attempt).unwrap();
    let back: Attempt = serde_json::from_value(serde_json::to_value(&attempt).unwrap()).unwrap();
    assert_eq!(back, attempt);

    let mut value = serde_json::to_value(&attempt).unwrap();
    value["end"]["reason"] = json!("gave_up");
    assert!(v.validate(RecordKind::Attempt, &value).is_err(), "end reasons are a closed set");
    let mut value = serde_json::to_value(&attempt).unwrap();
    value["spent"]["cost_usd"] = json!(-1.0);
    assert!(v.validate(RecordKind::Attempt, &value).is_err(), "spending is never negative");
    let mut value = serde_json::to_value(&attempt).unwrap();
    value["handoff"]["pid"] = json!(0);
    assert!(v.validate(RecordKind::Attempt, &value).is_err(), "a handoff names a real process");

    let event = Event {
        id: "evt-2".into(),
        task_id: "export-retry".into(),
        attempt_id: Some(attempt.id.clone()),
        epoch: Some(2),
        kind: EventKind::AttemptCancelled,
        payload_ref: None,
        received_at: t0(),
        acknowledged: true,
        outcome: Some(json!({"reason": "cancelled", "detail": "interrupted by SIGINT", "synthetic": false})),
    };
    v.check(RecordKind::Event, &event).unwrap();
}

#[test]
fn budgets_may_limit_time_and_cost() {
    let v = validators();
    let mut task = task();
    let value = serde_json::to_value(&task).unwrap();
    assert_eq!(value["budget"], json!({"max_attempts": 3}), "unset limits are not written");
    task.budget.max_wall_secs = Some(1800);
    task.budget.max_cost_usd = Some(2.5);
    task.budget.max_premium_requests = Some(10.0);
    v.check(RecordKind::Task, &task).unwrap();
    let mut value = serde_json::to_value(&task).unwrap();
    value["budget"]["max_cost_usd"] = json!(0);
    assert!(v.validate(RecordKind::Task, &value).is_err(), "a zero cost budget would allow nothing");
    let parsed: Budget = serde_json::from_value(json!({"max_attempts": 2})).unwrap();
    assert_eq!(parsed, Budget::attempts(2));
}
