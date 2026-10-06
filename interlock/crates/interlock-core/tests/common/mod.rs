#![allow(dead_code)]

use chrono::{Duration, TimeZone, Utc};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::grants::{self, HostPolicy, Profile};
use interlock_core::lifecycle::{self, TaskSpec};
use interlock_core::workflow::{self, Workflow};
use interlock_schema::*;

pub const TREE_A: &str = "aaaaaaa1111111111111111111111111111111111";
pub const TREE_B: &str = "bbbbbbb2222222222222222222222222222222222";

pub fn t(minutes: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap() + Duration::minutes(minutes)
}

pub fn bug_fix() -> Workflow {
    workflow::builtin("bug-fix", 1).unwrap()
}

pub fn criterion(id: &str, min: MinStrength, producer: Producer) -> Criterion {
    Criterion {
        id: id.into(),
        statement: format!("{id} holds"),
        check: None,
        check_version: "1".into(),
        min_strength: min,
        producer,
        baseline: Baseline::Any,
    }
}

pub fn spec(criteria: Vec<Criterion>) -> TaskSpec {
    TaskSpec {
        id: "export-retry".into(),
        repository: "/work/repo".into(),
        workflow: "bug-fix".into(),
        intent: "Fix duplicate rows when an export retries".into(),
        scope: Scope { paths: vec!["src/export/**".into()], description: None },
        dependencies: vec![],
        criteria,
        environment: "linux-x86_64".into(),
        budget: Budget::attempts(2),
        integration_required: false,
    }
}

pub fn bug_fix_task() -> Task {
    lifecycle::create(
        spec(vec![
            criterion("repro", MinStrength::Observed, Producer::Independent),
            criterion("regression", MinStrength::Tested, Producer::SelfReport),
        ]),
        &bug_fix(),
        t(0),
    )
    .unwrap()
}

pub fn all_capabilities() -> CapabilitySet {
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
        Capability::Parallel,
    ]
    .into_iter()
    .collect()
}

pub fn grant_for(task: &Task, role: Role) -> EffectiveGrant {
    grants::effective(&task.id, Profile::Conservative, &[], &HostPolicy::open(), bug_fix().role(role), t(0))
}

pub fn attempt(task: &Task, id: &str, role: Role, epoch: u32) -> Attempt {
    Attempt {
        id: id.into(),
        task_id: task.id.clone(),
        epoch,
        role,
        host: HostRef { host: "test".into(), version: "0".into() },
        agent: None,
        model: None,
        worktree: None,
        effective_grant: grant_for(task, role),
        token_hash: "0".repeat(64),
        status: AttemptStatus::Running,
        started_at: t(1),
        ended_at: None,
        handoff: None,
        end: None,
        spent: None,
        binding: None,
    }
}

pub fn evidence(task: &Task, id: &str, criterion: &str, attempt: &str, strength: Strength, tree: &str) -> Evidence {
    Evidence {
        id: id.into(),
        task_id: task.id.clone(),
        criterion_id: criterion.into(),
        attempt_id: attempt.into(),
        strength,
        currency: Currency {
            tree: tree.into(),
            check_version: "1".into(),
            environment: task.environment.clone(),
            policy_digest: task.policy_digest.clone(),
        },
        evidence_refs: vec![],
        note: None,
        recorded_at: t(5),
        bound_via: None,
    }
}

/// Evaluates claims and assessments with no check runs.
pub fn eval(task: &Task, claims: &[Evidence], assessments: &[Evidence]) -> interlock_core::EvidenceReport {
    interlock_core::evidence::evaluate(task, interlock_core::evidence::Records { claims, assessments, runs: &[] })
}

/// A check run interlock recorded.
pub fn run(
    task: &Task,
    id: &str,
    criterion: &str,
    producer: RunProducer,
    target: RunTarget,
    tree: &str,
    exit: i32,
) -> CheckRun {
    CheckRun {
        id: id.into(),
        task_id: task.id.clone(),
        criterion_id: criterion.into(),
        attempt_id: Some("att".into()),
        producer,
        target,
        tree: tree.into(),
        check_version: "1".into(),
        environment: task.environment.clone(),
        command: format!("sh checks/{criterion}.sh"),
        exit_code: Some(exit),
        timed_out: false,
        vacuous: None,
        duration_ms: 10,
        output_ref: None,
        output_tail: if exit == 0 { "ok".into() } else { "assertion failed".into() },
        recorded_at: t(4),
    }
}
