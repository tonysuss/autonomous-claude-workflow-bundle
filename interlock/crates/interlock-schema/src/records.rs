use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Id = String;
pub type Timestamp = DateTime<Utc>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Ready,
    Running,
    AwaitingVerification,
    Verified,
    Integrating,
    Done,
    Blocked,
    Failed,
    Cancelled,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Ready => "ready",
            State::Running => "running",
            State::AwaitingVerification => "awaiting_verification",
            State::Verified => "verified",
            State::Integrating => "integrating",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence strength. `Blocked` and `Failed` never count as a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    Observed,
    Tested,
    Static,
    Blocked,
    Failed,
}

impl Strength {
    /// Rank of a passing strength; `None` for blocked and failed.
    pub fn pass_rank(self) -> Option<u8> {
        match self {
            Strength::Observed => Some(3),
            Strength::Tested => Some(2),
            Strength::Static => Some(1),
            Strength::Blocked | Strength::Failed => None,
        }
    }

    pub fn satisfies(self, min: MinStrength) -> bool {
        self.pass_rank().is_some_and(|r| r >= min.rank())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinStrength {
    Observed,
    Tested,
    Static,
}

impl MinStrength {
    pub fn rank(self) -> u8 {
        match self {
            MinStrength::Observed => 3,
            MinStrength::Tested => 2,
            MinStrength::Static => 1,
        }
    }
}

/// Who may produce evidence that satisfies a criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Producer {
    /// A worker's own claim is enough.
    #[serde(rename = "self")]
    SelfReport,
    /// Only a verifier's assessment counts.
    #[serde(rename = "independent")]
    Independent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Worker,
    Verifier,
    Reviewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Read,
    LocalReversible,
    ExternalReversible,
    Landing,
    Irreversible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingAuthority {
    None,
    Coordinator,
    Owner,
    Operator,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRef {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repository: String,
    pub base_commit: String,
    #[serde(default)]
    pub untracked_hash: Option<String>,
    /// Files the task's checks run, as they were at the snapshot. A result may not change them.
    #[serde(default)]
    pub protected_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRef {
    pub host: String,
    pub version: String,
}

/// Ties evidence to exactly what it checked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Currency {
    pub tree: String,
    pub check_version: String,
    pub environment: String,
    pub policy_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveGrant {
    pub action_classes: Vec<ActionClass>,
    pub tools: ToolPolicy,
    pub landing_authority: LandingAuthority,
    pub grant_ids: Vec<Id>,
}

fn default_check_version() -> String {
    "1".to_string()
}

/// What a criterion's check must do on the task's input snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Baseline {
    /// The check reproduces the problem: it must fail before the change.
    Fails,
    /// A regression guard: it must already pass before the change.
    Passes,
    #[default]
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    pub id: Id,
    pub statement: String,
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default = "default_check_version")]
    pub check_version: String,
    pub min_strength: MinStrength,
    pub producer: Producer,
    #[serde(default)]
    pub baseline: Baseline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub paths: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub max_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: Id,
    pub repository: String,
    pub workflow: WorkflowRef,
    pub intent: String,
    pub scope: Scope,
    pub dependencies: Vec<Id>,
    pub criteria: Vec<Criterion>,
    #[serde(default)]
    pub input_snapshot: Option<Snapshot>,
    pub environment: String,
    pub policy_digest: String,
    pub budget: Budget,
    pub integration_required: bool,
    pub state: State,
    #[serde(default)]
    pub resume_point: Option<State>,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub current_tree: Option<String>,
    pub lease_epoch: u32,
    #[serde(default)]
    pub current_attempt: Option<Id>,
    pub attempts_used: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Running,
    Submitted,
    Superseded,
    Cancelled,
    Failed,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: Id,
    pub task_id: Id,
    pub epoch: u32,
    pub role: Role,
    pub host: HostRef,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    pub effective_grant: EffectiveGrant,
    pub token_hash: String,
    pub status: AttemptStatus,
    pub started_at: Timestamp,
    #[serde(default)]
    pub ended_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Accepted,
    Superseded,
    /// Changed files outside the task's scope or the task's own checks.
    Rejected,
}

/// A worker's output (the `result` record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub id: Id,
    pub task_id: Id,
    pub attempt_id: Id,
    pub epoch: u32,
    pub output_tree: String,
    pub changed_paths: Vec<String>,
    pub summary: String,
    pub open_questions: Vec<String>,
    pub status: ResultStatus,
    #[serde(default)]
    pub superseded_reason: Option<String>,
    pub received_at: Timestamp,
}

/// A claim (from a worker) or an assessment (from a verifier). The two are
/// always stored apart; the policy reads both and decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: Id,
    pub task_id: Id,
    pub criterion_id: Id,
    pub attempt_id: Id,
    pub strength: Strength,
    pub currency: Currency,
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub recorded_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    #[serde(rename = "result.submitted")]
    ResultSubmitted,
    #[serde(rename = "claim.added")]
    ClaimAdded,
    #[serde(rename = "assessment.added")]
    AssessmentAdded,
    #[serde(rename = "attempt.failed")]
    AttemptFailed,
    #[serde(rename = "attempt.completed")]
    AttemptCompleted,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::ResultSubmitted => "result.submitted",
            EventKind::ClaimAdded => "claim.added",
            EventKind::AssessmentAdded => "assessment.added",
            EventKind::AttemptFailed => "attempt.failed",
            EventKind::AttemptCompleted => "attempt.completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: Id,
    pub task_id: Id,
    #[serde(default)]
    pub attempt_id: Option<Id>,
    #[serde(default)]
    pub epoch: Option<u32>,
    #[serde(rename = "type")]
    pub kind: EventKind,
    #[serde(default)]
    pub payload_ref: Option<String>,
    pub received_at: Timestamp,
    pub acknowledged: bool,
    #[serde(default)]
    pub outcome: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: Id,
    pub principal: String,
    pub task_scope: Vec<String>,
    pub action_classes: Vec<ActionClass>,
    pub tools: ToolPolicy,
    pub landing_authority: LandingAuthority,
    pub origin: String,
    pub created_at: Timestamp,
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    #[serde(default)]
    pub revoked_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    OpenPr,
    Merge,
    ArmAutoMerge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Planned,
    Started,
    Confirmed,
    Unknown,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationIntent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub id: Id,
    pub task_id: Id,
    pub kind: OperationKind,
    pub intent: OperationIntent,
    pub state: OperationState,
    #[serde(default)]
    pub outcome: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Who asked interlock to run a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunProducer {
    Worker,
    Verifier,
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTarget {
    /// The tree under verification.
    Output,
    /// The task's input snapshot.
    Base,
}

/// interlock's own execution of a criterion's check on one tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub id: Id,
    pub task_id: Id,
    pub criterion_id: Id,
    #[serde(default)]
    pub attempt_id: Option<Id>,
    pub producer: RunProducer,
    pub target: RunTarget,
    pub tree: String,
    pub check_version: String,
    pub environment: String,
    pub command: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The detector that found the check ran nothing, if one did.
    #[serde(default)]
    pub vacuous: Option<String>,
    pub duration_ms: u64,
    #[serde(default)]
    pub output_ref: Option<String>,
    pub output_tail: String,
    pub recorded_at: Timestamp,
}

impl CheckRun {
    /// Exited 0, finished in time, and actually checked something.
    pub fn passed(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && self.vacuous.is_none()
    }

    /// Ran to completion and reported failure.
    pub fn failed(&self) -> bool {
        !self.timed_out && self.exit_code.is_some_and(|c| c != 0) && self.vacuous.is_none()
    }
}
