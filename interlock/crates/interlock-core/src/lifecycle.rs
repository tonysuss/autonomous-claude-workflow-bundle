//! The task lifecycle. Every state change goes through one of these functions,
//! which take the current records and return the updated task plus the move,
//! or a refusal. Nothing here touches storage or the clock.

use interlock_schema::{
    Attempt, AttemptStatus, Budget, Criterion, EffectiveGrant, Id, LandingAuthority, Operation, Role, Scope, Snapshot,
    State, Task, Timestamp, WorkflowRef,
};
use serde::{Deserialize, Serialize};

use crate::capability::{self, CapabilitySet, Fallback};
use crate::digest::policy_digest;
use crate::evidence::EvidenceReport;
use crate::grants;
use crate::workflow::{Integration, Mode, Workflow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Signal {
    G1,
    G2,
    G3,
    G4,
    G5,
    G6,
    G7,
    R1,
    R2,
    /// Running back to ready: the current attempt was cancelled or timed out.
    R3,
    #[serde(rename = "block")]
    Block,
    #[serde(rename = "unblock")]
    Unblock,
    #[serde(rename = "fail")]
    Fail,
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub signal: Signal,
    pub from: State,
    pub to: State,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    InvalidTask,
    WrongState,
    Terminal,
    DependenciesNotDone,
    BudgetExhausted,
    GrantInsufficient,
    WrongTask,
    WrongRole,
    AttemptNotActive,
    UnknownCriterion,
    EvidenceNotCurrent,
    IntegrationNotAllowed,
    HeadMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub signal: Option<Signal>,
    pub code: RefusalCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if !self.missing.is_empty() {
            write!(f, " (missing: {})", self.missing.join("; "))?;
        }
        Ok(())
    }
}

impl std::error::Error for Refusal {}

fn refuse(signal: Option<Signal>, code: RefusalCode, message: impl Into<String>) -> Refusal {
    Refusal { signal, code, message: message.into(), missing: vec![] }
}

fn require_state(task: &Task, signal: Signal, allowed: &[State]) -> Result<(), Refusal> {
    if allowed.contains(&task.state) {
        return Ok(());
    }
    let code = if task.state.is_terminal() { RefusalCode::Terminal } else { RefusalCode::WrongState };
    let names: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
    Err(refuse(
        Some(signal),
        code,
        format!("{signal:?} needs the task in {}, but it is {}", names.join(" or "), task.state),
    ))
}

/// The updated task and the move that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub task: Task,
    pub mv: Move,
}

fn transition(task: &Task, signal: Signal, to: State, reason: impl Into<String>, now: Timestamp) -> Outcome {
    let mut next = task.clone();
    next.state = to;
    next.updated_at = now;
    Outcome { mv: Move { signal, from: task.state, to, reason: reason.into() }, task: next }
}

/// What a caller supplies to create a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: Id,
    pub repository: String,
    pub workflow: String,
    pub intent: String,
    #[serde(default = "default_scope")]
    pub scope: Scope,
    #[serde(default)]
    pub dependencies: Vec<Id>,
    #[serde(rename = "criterion", alias = "criteria")]
    pub criteria: Vec<Criterion>,
    #[serde(default)]
    pub environment: String,
    #[serde(default = "default_budget")]
    pub budget: Budget,
    #[serde(default)]
    pub integration_required: bool,
}

fn default_scope() -> Scope {
    Scope { paths: vec![], description: None }
}

fn default_budget() -> Budget {
    Budget { max_attempts: 3 }
}

pub fn create(spec: TaskSpec, workflow: &Workflow, now: Timestamp) -> Result<Task, Refusal> {
    let invalid = |m: String| refuse(None, RefusalCode::InvalidTask, m);
    if spec.criteria.is_empty() {
        return Err(invalid("a task needs at least one criterion; without one nothing can be verified".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &spec.criteria {
        if !seen.insert(c.id.as_str()) {
            return Err(invalid(format!("criterion id {} appears twice", c.id)));
        }
    }
    if spec.budget.max_attempts == 0 {
        return Err(invalid("budget.max_attempts must be at least 1".into()));
    }
    if spec.integration_required && workflow.integration == Integration::Never {
        return Err(invalid(format!("the {} workflow never integrates", workflow.name)));
    }
    if spec.dependencies.contains(&spec.id) {
        return Err(invalid("a task cannot depend on itself".into()));
    }
    Ok(Task {
        policy_digest: policy_digest(&spec.criteria),
        id: spec.id,
        repository: spec.repository,
        workflow: WorkflowRef { name: workflow.name.clone(), version: workflow.version },
        intent: spec.intent,
        scope: spec.scope,
        dependencies: spec.dependencies,
        criteria: spec.criteria,
        input_snapshot: None,
        environment: spec.environment,
        budget: spec.budget,
        integration_required: spec.integration_required,
        state: State::Pending,
        resume_point: None,
        blocked_reason: None,
        current_tree: None,
        lease_epoch: 0,
        current_attempt: None,
        attempts_used: 0,
        created_at: now,
        updated_at: now,
    })
}

/// G1, pending to ready: dependencies done and the input snapshot recorded.
pub fn ready(
    task: &Task,
    dependencies: &[(Id, State)],
    snapshot: Snapshot,
    now: Timestamp,
) -> Result<Outcome, Refusal> {
    require_state(task, Signal::G1, &[State::Pending])?;
    let not_done: Vec<String> = task
        .dependencies
        .iter()
        .filter(|d| !dependencies.iter().any(|(id, s)| id == *d && *s == State::Done))
        .map(|d| {
            let state = dependencies.iter().find(|(id, _)| id == d).map_or("missing".into(), |(_, s)| s.to_string());
            format!("{d} is {state}")
        })
        .collect();
    if !not_done.is_empty() {
        return Err(Refusal {
            signal: Some(Signal::G1),
            code: RefusalCode::DependenciesNotDone,
            message: "every dependency must be done first".into(),
            missing: not_done,
        });
    }
    let mut out = transition(task, Signal::G1, State::Ready, "dependencies done; input snapshot recorded", now);
    out.task.input_snapshot = Some(snapshot);
    Ok(out)
}

/// The result of asking to start an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The attempt may run with this epoch. For a worker, `outcome` moves the task to running.
    Allowed { epoch: u32, outcome: Option<Outcome>, fallbacks: Vec<(Fallback, String)> },
    /// A required capability is missing with no fallback; the task moves to blocked.
    Blocked(Outcome),
}

#[allow(clippy::too_many_arguments)]
fn check_start(
    task: &Task,
    signal: Signal,
    workflow: &Workflow,
    role: Role,
    mode: Mode,
    capabilities: &CapabilitySet,
    grant: &EffectiveGrant,
    now: Timestamp,
) -> Result<Result<Vec<(Fallback, String)>, Outcome>, Refusal> {
    let missing_classes = grants::missing_classes(grant, &workflow.role(role).action_classes);
    if !missing_classes.is_empty() {
        return Err(Refusal {
            signal: Some(signal),
            code: RefusalCode::GrantInsufficient,
            message: format!("the effective grant does not cover what a {role:?} needs"),
            missing: missing_classes.iter().map(|c| format!("{c:?}")).collect(),
        });
    }
    let check = capability::check(&workflow.requirements(role, mode), capabilities);
    if !check.ok() {
        let reason = format!("host is missing: {}", check.missing.join("; "));
        let mut out = transition(task, Signal::Block, State::Blocked, reason.clone(), now);
        out.task.resume_point = Some(task.state);
        out.task.blocked_reason = Some(reason);
        return Ok(Err(out));
    }
    Ok(Ok(check.fallbacks))
}

/// G2, ready to running: opens a worker attempt with lease epoch n+1.
pub fn start_worker(
    task: &Task,
    workflow: &Workflow,
    mode: Mode,
    capabilities: &CapabilitySet,
    grant: &EffectiveGrant,
    attempt_id: &str,
    now: Timestamp,
) -> Result<Start, Refusal> {
    require_state(task, Signal::G2, &[State::Ready])?;
    if task.attempts_used >= task.budget.max_attempts {
        return Err(refuse(
            Some(Signal::G2),
            RefusalCode::BudgetExhausted,
            format!("all {} attempts are used", task.budget.max_attempts),
        ));
    }
    match check_start(task, Signal::G2, workflow, Role::Worker, mode, capabilities, grant, now)? {
        Err(blocked) => Ok(Start::Blocked(blocked)),
        Ok(fallbacks) => {
            let epoch = task.lease_epoch + 1;
            let mut out = transition(
                task,
                Signal::G2,
                State::Running,
                format!("attempt {attempt_id} opened at epoch {epoch}"),
                now,
            );
            out.task.lease_epoch = epoch;
            out.task.attempts_used += 1;
            out.task.current_attempt = Some(attempt_id.to_string());
            Ok(Start::Allowed { epoch, outcome: Some(out), fallbacks })
        }
    }
}

/// Opens a verifier attempt. No state change; without an independent verifier
/// the task moves to blocked with its work kept.
pub fn start_verifier(
    task: &Task,
    workflow: &Workflow,
    mode: Mode,
    capabilities: &CapabilitySet,
    grant: &EffectiveGrant,
    now: Timestamp,
) -> Result<Start, Refusal> {
    require_state(task, Signal::G4, &[State::AwaitingVerification])?;
    match check_start(task, Signal::G4, workflow, Role::Verifier, mode, capabilities, grant, now)? {
        Err(mut blocked) => {
            let reason = format!("no independent verifier: {}", blocked.mv.reason);
            blocked.mv.reason = reason.clone();
            blocked.task.blocked_reason = Some(reason);
            Ok(Start::Blocked(blocked))
        }
        Ok(fallbacks) => Ok(Start::Allowed { epoch: task.lease_epoch, outcome: None, fallbacks }),
    }
}

#[allow(clippy::large_enum_variant)] // short-lived return value
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    Accepted(Outcome),
    /// Stored but never applied.
    Superseded {
        reason: String,
    },
}

/// G3, running to awaiting verification: only for the current attempt and epoch.
pub fn submit_result(
    task: &Task,
    attempt: &Attempt,
    epoch: u32,
    output_tree: &str,
    now: Timestamp,
) -> Result<Submission, Refusal> {
    if attempt.task_id != task.id {
        return Err(refuse(Some(Signal::G3), RefusalCode::WrongTask, "attempt belongs to another task"));
    }
    if attempt.role != Role::Worker {
        return Err(refuse(Some(Signal::G3), RefusalCode::WrongRole, "only a worker attempt submits results"));
    }
    let superseded = |reason: String| Ok(Submission::Superseded { reason });
    if task.current_attempt.as_deref() != Some(attempt.id.as_str()) {
        return superseded(format!(
            "attempt {} is not the current attempt ({})",
            attempt.id,
            task.current_attempt.as_deref().unwrap_or("none")
        ));
    }
    if epoch != task.lease_epoch || attempt.epoch != task.lease_epoch {
        return superseded(format!("epoch {epoch} is not the current lease epoch {}", task.lease_epoch));
    }
    if attempt.status != AttemptStatus::Running {
        return superseded(format!("attempt is {:?}, not running", attempt.status));
    }
    if task.state != State::Running {
        return superseded(format!("task is {}, not running", task.state));
    }
    let mut out = transition(
        task,
        Signal::G3,
        State::AwaitingVerification,
        format!("result accepted from attempt {} at epoch {epoch}", attempt.id),
        now,
    );
    out.task.current_tree = Some(output_tree.to_string());
    Ok(Submission::Accepted(out))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Claim,
    Assessment,
}

/// Who may record evidence. The role comes from the attempt, never the caller.
pub fn check_evidence_source(
    task: &Task,
    attempt: &Attempt,
    kind: EvidenceKind,
    criterion_id: &str,
) -> Result<(), Refusal> {
    if attempt.task_id != task.id {
        return Err(refuse(None, RefusalCode::WrongTask, "attempt belongs to another task"));
    }
    if task.state.is_terminal() {
        return Err(refuse(None, RefusalCode::Terminal, format!("task is {}", task.state)));
    }
    if !task.criteria.iter().any(|c| c.id == criterion_id) {
        return Err(refuse(None, RefusalCode::UnknownCriterion, format!("task has no criterion {criterion_id}")));
    }
    match kind {
        EvidenceKind::Claim => {
            if attempt.role != Role::Worker {
                return Err(refuse(
                    None,
                    RefusalCode::WrongRole,
                    "claims come from worker attempts; verifiers record assessments",
                ));
            }
            if !matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted) {
                return Err(refuse(None, RefusalCode::AttemptNotActive, format!("attempt is {:?}", attempt.status)));
            }
        }
        EvidenceKind::Assessment => {
            if attempt.role != Role::Verifier {
                return Err(refuse(None, RefusalCode::WrongRole, "assessments come from verifier attempts only"));
            }
            if attempt.status != AttemptStatus::Running {
                return Err(refuse(None, RefusalCode::AttemptNotActive, format!("attempt is {:?}", attempt.status)));
            }
        }
    }
    Ok(())
}

/// The automatic moves the evidence allows right now: G4, R1, R2 and G7.
/// Returns `None` when nothing should change.
pub fn advance(task: &Task, report: &EvidenceReport, now: Timestamp) -> Option<Outcome> {
    match task.state {
        State::AwaitingVerification if report.any_fail => {
            if task.attempts_used < task.budget.max_attempts {
                let mut out = transition(
                    task,
                    Signal::R1,
                    State::Ready,
                    "an independent check failed; rework with a fix brief",
                    now,
                );
                out.task.current_attempt = None;
                Some(out)
            } else {
                Some(transition(
                    task,
                    Signal::Fail,
                    State::Failed,
                    format!("an independent check failed and all {} attempts are used", task.budget.max_attempts),
                    now,
                ))
            }
        }
        State::AwaitingVerification if report.all_pass => Some(transition(
            task,
            Signal::G4,
            State::Verified,
            "every required criterion has current passing evidence",
            now,
        )),
        State::Verified | State::Integrating if !report.all_pass => Some(transition(
            task,
            Signal::R2,
            State::AwaitingVerification,
            "evidence went stale: the tree, checks, environment or policy changed",
            now,
        )),
        State::Verified if !task.integration_required => {
            Some(transition(task, Signal::G7, State::Done, "verified; the workflow needs no delivery", now))
        }
        _ => None,
    }
}

/// R3, running back to ready when the current attempt is cancelled or times
/// out, so a fresh attempt can start at the next epoch. Fails the task when the
/// attempt budget is spent.
pub fn retry(task: &Task, reason: &str, now: Timestamp) -> Result<Outcome, Refusal> {
    require_state(task, Signal::R3, &[State::Running])?;
    if task.attempts_used >= task.budget.max_attempts {
        return Ok(transition(
            task,
            Signal::Fail,
            State::Failed,
            format!("{reason}; all {} attempts are used", task.budget.max_attempts),
            now,
        ));
    }
    let mut out = transition(task, Signal::R3, State::Ready, reason, now);
    out.task.current_attempt = None;
    Ok(out)
}

/// G5, verified to integrating. Without landing authority the task is blocked
/// at verified, with the reason recorded.
pub fn begin_integration(
    task: &Task,
    report: &EvidenceReport,
    landing: LandingAuthority,
    now: Timestamp,
) -> Result<Outcome, Refusal> {
    require_state(task, Signal::G5, &[State::Verified])?;
    if !task.integration_required {
        return Err(refuse(Some(Signal::G5), RefusalCode::IntegrationNotAllowed, "this task does not integrate"));
    }
    if !report.all_pass {
        return Err(Refusal {
            signal: Some(Signal::G5),
            code: RefusalCode::EvidenceNotCurrent,
            message: "evidence is no longer current; advance the task to re-verify".into(),
            missing: report.missing(),
        });
    }
    if landing == LandingAuthority::None {
        let reason = "no landing authority is granted for this task";
        let mut out = transition(task, Signal::Block, State::Blocked, reason, now);
        out.task.resume_point = Some(State::Verified);
        out.task.blocked_reason = Some(reason.into());
        return Ok(out);
    }
    Ok(transition(task, Signal::G5, State::Integrating, format!("landing authorized ({landing:?})"), now))
}

/// What the forge reported for a pinned merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MergeReport {
    Merged {
        head_sha: String,
    },
    /// The forge refused, for example because the head moved.
    Refused {
        reason: String,
    },
}

/// G6, integrating to done: the forge merged exactly the verified head.
pub fn confirm_integration(
    task: &Task,
    operation: &Operation,
    report: &MergeReport,
    now: Timestamp,
) -> Result<Outcome, Refusal> {
    require_state(task, Signal::G6, &[State::Integrating])?;
    if operation.task_id != task.id {
        return Err(refuse(Some(Signal::G6), RefusalCode::WrongTask, "operation belongs to another task"));
    }
    match report {
        MergeReport::Refused { reason } => {
            Ok(transition(task, Signal::R2, State::AwaitingVerification, format!("merge refused: {reason}"), now))
        }
        MergeReport::Merged { head_sha } => {
            let expected = operation.intent.expected_head_sha.as_deref().unwrap_or("");
            let matches =
                expected.len() >= 7 && (head_sha.starts_with(expected) || expected.starts_with(head_sha.as_str()));
            if !matches {
                return Err(refuse(
                    Some(Signal::G6),
                    RefusalCode::HeadMismatch,
                    format!("forge merged {head_sha}, but the verified head was {expected}; reconcile by hand"),
                ));
            }
            Ok(transition(task, Signal::G6, State::Done, format!("merged at {head_sha}"), now))
        }
    }
}

/// Records that the code under evidence changed, for example after a rebase.
/// Not a move: the next `advance` finds the evidence stale and applies R2.
pub fn record_new_tree(task: &Task, tree: &str, now: Timestamp) -> Result<Task, Refusal> {
    require_state(task, Signal::R2, &[State::AwaitingVerification, State::Verified, State::Integrating])?;
    let mut next = task.clone();
    next.current_tree = Some(tree.to_string());
    next.updated_at = now;
    Ok(next)
}

pub fn block(task: &Task, reason: &str, now: Timestamp) -> Result<Outcome, Refusal> {
    if task.state.is_terminal() || task.state == State::Blocked {
        return Err(refuse(Some(Signal::Block), RefusalCode::WrongState, format!("task is {}", task.state)));
    }
    let mut out = transition(task, Signal::Block, State::Blocked, reason, now);
    out.task.resume_point = Some(task.state);
    out.task.blocked_reason = Some(reason.to_string());
    Ok(out)
}

/// Returns the task to the exact state it left.
pub fn unblock(task: &Task, now: Timestamp) -> Result<Outcome, Refusal> {
    require_state(task, Signal::Unblock, &[State::Blocked])?;
    let to = task.resume_point.unwrap_or(State::Pending);
    let mut out = transition(task, Signal::Unblock, to, "unblocked by the operator", now);
    out.task.resume_point = None;
    out.task.blocked_reason = None;
    Ok(out)
}

pub fn fail(task: &Task, reason: &str, now: Timestamp) -> Result<Outcome, Refusal> {
    if task.state.is_terminal() {
        return Err(refuse(Some(Signal::Fail), RefusalCode::Terminal, format!("task is {}", task.state)));
    }
    Ok(transition(task, Signal::Fail, State::Failed, reason, now))
}

pub fn cancel(task: &Task, reason: &str, now: Timestamp) -> Result<Outcome, Refusal> {
    if task.state.is_terminal() {
        return Err(refuse(Some(Signal::Cancel), RefusalCode::Terminal, format!("task is {}", task.state)));
    }
    Ok(transition(task, Signal::Cancel, State::Cancelled, reason, now))
}

/// One possible next move, for `interlock status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NextMove {
    pub signal: Signal,
    pub to: State,
    pub ready: bool,
    pub needs: Vec<String>,
}

pub fn next_moves(task: &Task, report: &EvidenceReport) -> Vec<NextMove> {
    let mv = |signal, to, ready, needs: Vec<String>| NextMove { signal, to, ready, needs };
    let budget_left = task.attempts_used < task.budget.max_attempts;
    match task.state {
        State::Pending => {
            vec![mv(Signal::G1, State::Ready, false, vec!["dependencies done and an input snapshot".into()])]
        }
        State::Ready => vec![mv(
            Signal::G2,
            State::Running,
            budget_left,
            if budget_left { vec![] } else { vec!["attempt budget is used up".into()] },
        )],
        State::Running => vec![
            mv(
                Signal::G3,
                State::AwaitingVerification,
                false,
                vec![format!(
                    "a result from attempt {} at epoch {}",
                    task.current_attempt.as_deref().unwrap_or("?"),
                    task.lease_epoch
                )],
            ),
            mv(Signal::R3, State::Ready, budget_left, vec!["cancel or time out the current attempt".into()]),
        ],
        State::AwaitingVerification if report.any_fail => vec![if budget_left {
            mv(Signal::R1, State::Ready, true, vec![])
        } else {
            mv(Signal::Fail, State::Failed, true, vec![])
        }],
        State::AwaitingVerification => vec![mv(Signal::G4, State::Verified, report.all_pass, report.missing())],
        State::Verified if !report.all_pass => {
            vec![mv(Signal::R2, State::AwaitingVerification, true, report.missing())]
        }
        State::Verified if task.integration_required => {
            vec![mv(
                Signal::G5,
                State::Integrating,
                false,
                vec!["landing authority and an integration operation".into()],
            )]
        }
        State::Verified => vec![mv(Signal::G7, State::Done, true, vec![])],
        State::Integrating if !report.all_pass => {
            vec![mv(Signal::R2, State::AwaitingVerification, true, report.missing())]
        }
        State::Integrating => {
            vec![mv(Signal::G6, State::Done, false, vec!["the forge to confirm a merge of the verified head".into()])]
        }
        State::Blocked => vec![mv(
            Signal::Unblock,
            task.resume_point.unwrap_or(State::Pending),
            false,
            vec![task.blocked_reason.clone().unwrap_or_else(|| "operator to unblock".into())],
        )],
        State::Done | State::Failed | State::Cancelled => vec![],
    }
}
