//! Delivery rules: what each forge operation needs before interlock makes the
//! call, and what the forge's answer means for an operation whose outcome is
//! not yet known. Pure, like the rest of the core: callers observe the forge
//! and pass in what they saw.

use interlock_schema::{
    ActionClass, LandingAuthority, Operation, OperationKind, OperationState, State, Task, Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::lifecycle::{self, MergeReport, Outcome, Refusal, RefusalCode, Signal};

/// How a task's blocked reason starts while one of its operations has an
/// unknown outcome. Reconciling that operation lifts the block.
pub const UNKNOWN_OUTCOME: &str = "the outcome of a forge operation is unknown";

/// The branch interlock pushes a task's verified head to. Characters git
/// does not allow in branch names are replaced.
pub fn branch_for(task_id: &str) -> String {
    let mut name = task_id.replace(':', "-");
    while name.contains("..") {
        name = name.replace("..", "-");
    }
    let mut name = name.trim_end_matches('.').to_string();
    if name.ends_with(".lock") {
        name.push('-');
    }
    format!("interlock/{name}")
}

/// Object ids compare by prefix, so abbreviated ids match full ones.
pub fn same_sha(a: &str, b: &str) -> bool {
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

pub fn action_class(kind: OperationKind) -> ActionClass {
    match kind {
        OperationKind::OpenPr => ActionClass::ExternalReversible,
        OperationKind::Merge | OperationKind::ArmAutoMerge => ActionClass::Landing,
    }
}

/// Whether the operation is the one whose confirmation lands the task (G6).
pub fn lands(kind: OperationKind) -> bool {
    action_class(kind) == ActionClass::Landing
}

/// The core's own check before interlock makes a forge call: the task is
/// integrating, the operation is planned and belongs to it, and landing
/// authority is still granted. Every operation exists only to land the task,
/// so each one needs that authority at the moment of the call, not just at G5.
pub fn authorize(task: &Task, op: &Operation, landing: LandingAuthority) -> Result<(), Refusal> {
    let refuse = |code, message: String| Refusal { signal: None, code, message, missing: vec![] };
    if op.task_id != task.id {
        return Err(refuse(RefusalCode::WrongTask, format!("operation {} belongs to another task", op.id)));
    }
    if task.state != State::Integrating {
        return Err(refuse(
            RefusalCode::WrongState,
            format!("forge calls need the task integrating; it is {}", task.state),
        ));
    }
    if op.state != OperationState::Planned {
        return Err(refuse(RefusalCode::WrongState, format!("operation {} is {:?}, not planned", op.id, op.state)));
    }
    if landing == LandingAuthority::None {
        let class = action_class(op.kind);
        return Err(refuse(
            RefusalCode::GrantInsufficient,
            format!("landing authority is no longer granted for this task; refusing the {class:?} call for {}", op.id),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

/// What the forge shows about a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub state: PrState,
    /// The head commit, or for a merged pull request the head it merged.
    pub head: String,
    /// Auto-merge or a merge queue holds it.
    pub auto_merge: bool,
}

/// One look at the forge on behalf of an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "seen", content = "detail", rename_all = "snake_case")]
pub enum Observation {
    Pr(PullRequest),
    /// The forge answered that there is no pull request for the branch.
    NoPr,
    /// The forge could not be asked or did not answer.
    Unreachable(String),
}

/// What an observation means for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// The landing was confirmed or refused: G6, or R2.
    Land { report: MergeReport },
    /// An operation that does not land took effect.
    Confirmed,
    /// The operation had no effect; it may be planned again.
    Failed { reason: String },
    /// The forge still holds the request, for example an armed auto-merge.
    Pending { reason: String },
    /// The forge did something interlock cannot accept; the operator decides.
    Block { reason: String },
    /// The forge could not say; the task is blocked until it can.
    Unknown { reason: String },
}

/// What a look at the forge decides for an operation that was started, or
/// whose outcome was unknown.
pub fn reconcile(op: &Operation, seen: &Observation) -> Verdict {
    let expected = op.intent.expected_head_sha.as_deref().unwrap_or("");
    let pr = match seen {
        Observation::Unreachable(why) => {
            return Verdict::Unknown { reason: format!("the forge did not answer: {why}") };
        }
        Observation::NoPr if op.kind == OperationKind::OpenPr => {
            return Verdict::Failed { reason: "no pull request exists for the branch".into() };
        }
        Observation::NoPr => {
            let n = op.intent.pull_request.map_or("?".into(), |n| n.to_string());
            return Verdict::Unknown { reason: format!("the forge does not show pull request #{n}") };
        }
        Observation::Pr(pr) => pr,
    };
    let at_head = same_sha(&pr.head, expected);
    let n = pr.number;
    match (op.kind, pr.state) {
        (OperationKind::OpenPr, PrState::Open | PrState::Merged) if at_head => Verdict::Confirmed,
        (OperationKind::OpenPr, _) => Verdict::Failed {
            reason: format!("pull request #{n} is {:?} at {}, not open at {expected}", pr.state, short(&pr.head)),
        },
        (_, PrState::Merged) if at_head => Verdict::Land { report: MergeReport::Merged { head_sha: pr.head.clone() } },
        (_, PrState::Merged) => Verdict::Block {
            reason: format!(
                "pull request #{n} was merged at {}, but the verified head was {}; reconcile by hand",
                pr.head, expected
            ),
        },
        (_, PrState::Open) if !at_head => Verdict::Land {
            report: MergeReport::Refused {
                reason: format!(
                    "the head of pull request #{n} moved to {} before the merge was confirmed; it was pinned to {}",
                    short(&pr.head),
                    short(expected)
                ),
            },
        },
        (_, PrState::Open) if pr.auto_merge => {
            Verdict::Pending { reason: format!("pull request #{n} is queued to merge at the verified head") }
        }
        (_, PrState::Open) => Verdict::Failed {
            reason: format!("pull request #{n} is still open at the verified head; it was not merged"),
        },
        (_, PrState::Closed) => Verdict::Block { reason: format!("pull request #{n} was closed without merging") },
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The base moved before the merge: the task now builds on `base`, and `tree`
/// is what would land there. Evidence about the old tree stops counting, so
/// the next move is R2.
pub fn rebase(task: &Task, base: &str, tree: &str, now: Timestamp) -> Result<Task, Refusal> {
    let mut next = lifecycle::record_new_tree(task, tree, now)?;
    let snapshot = next.input_snapshot.as_mut().ok_or_else(|| Refusal {
        signal: Some(Signal::R2),
        code: RefusalCode::InvalidTask,
        message: "the task has no input snapshot to rebase".into(),
        missing: vec![],
    })?;
    snapshot.base_commit = base.to_string();
    Ok(next)
}

/// A task blocked only because an operation's outcome was unknown resumes
/// where it stopped once the forge has answered.
pub fn resume(task: &Task, now: Timestamp) -> Option<Outcome> {
    let blocked_on_forge = task.blocked_reason.as_deref().is_some_and(|r| r.starts_with(UNKNOWN_OUTCOME));
    if task.state != State::Blocked || !blocked_on_forge {
        return None;
    }
    let mut out = lifecycle::unblock(task, now).ok()?;
    out.mv.reason = "the forge answered; resuming".into();
    Some(out)
}
