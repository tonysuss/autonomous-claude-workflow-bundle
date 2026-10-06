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

/// The branch interlock pushes a task's verified head to. Characters git does
/// not allow there are escaped with `%`, which no task id contains, so two
/// tasks never share a branch (`task_for_branch` reverses it).
pub fn branch_for(task_id: &str) -> String {
    let chars: Vec<char> = task_id.chars().collect();
    let mut name = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let next_or_prev_dot = chars.get(i + 1) == Some(&'.') || (i > 0 && chars[i - 1] == '.');
        match c {
            ':' => name.push_str("%3A"),
            '%' => name.push_str("%25"),
            '.' if i + 1 == chars.len() || next_or_prev_dot => name.push_str("%2E"),
            c => name.push(c),
        }
    }
    if name.ends_with(".lock") {
        let at = name.len() - ".lock".len();
        name.replace_range(at..at + 1, "%2E");
    }
    format!("interlock/{name}")
}

/// The task id whose branch `branch_for` named `branch`.
pub fn task_for_branch(branch: &str) -> Option<String> {
    let mut rest = branch.strip_prefix("interlock/")?;
    let mut out = String::new();
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        out.push(match rest.get(i + 1..i + 3)? {
            "3A" => ':',
            "2E" => '.',
            "25" => '%',
            _ => return None,
        });
        rest = &rest[i + 3..];
    }
    out.push_str(rest);
    Some(out)
}

/// Object ids compare by prefix, so abbreviated ids match full ones.
pub fn same_sha(a: &str, b: &str) -> bool {
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

pub fn action_class(kind: OperationKind) -> ActionClass {
    match kind {
        OperationKind::OpenPr | OperationKind::DisarmAutoMerge => ActionClass::ExternalReversible,
        OperationKind::Merge | OperationKind::ArmAutoMerge => ActionClass::Landing,
    }
}

/// Whether the operation is the one whose confirmation lands the task (G6).
pub fn lands(kind: OperationKind) -> bool {
    action_class(kind) == ActionClass::Landing
}

/// Whether interlock merges itself under this landing authority. With
/// `operator`, interlock opens the pull request pinned at the verified head
/// and the operator merges it; with `coordinator` or `owner`, interlock merges.
pub fn interlock_merges(landing: LandingAuthority) -> bool {
    matches!(landing, LandingAuthority::Coordinator | LandingAuthority::Owner)
}

/// The core's own check before interlock makes a forge call: the task is
/// integrating, the operation is planned and belongs to it, and landing
/// authority is still granted. Every operation exists only to land the task,
/// so each one needs that authority at the moment of the call, not just at G5.
/// Only a coordinator or owner lets interlock merge or arm auto-merge itself.
/// Withdrawing an armed auto-merge needs no authority.
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
    if op.kind == OperationKind::DisarmAutoMerge {
        return Ok(());
    }
    if landing == LandingAuthority::None {
        let class = action_class(op.kind);
        return Err(refuse(
            RefusalCode::GrantInsufficient,
            format!("landing authority is no longer granted for this task; refusing the {class:?} call for {}", op.id),
        ));
    }
    if lands(op.kind) && !interlock_merges(landing) {
        return Err(refuse(
            RefusalCode::IntegrationNotAllowed,
            format!(
                "landing authority is {landing:?}: the operator merges, so interlock will not call the forge for {}",
                op.id
            ),
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

/// Where a merged pull request landed, as git (not the forge's API) sees the
/// base branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
    /// The base branch, fetched with git, contains the merge commit.
    pub on_base: bool,
    /// The merge commit's first parent: the base it merged onto.
    #[serde(default)]
    pub onto: Option<String>,
    /// The tree the merge left on the base branch.
    #[serde(default)]
    pub tree: Option<String>,
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
    /// The branch it targets.
    #[serde(default)]
    pub base_branch: Option<String>,
    #[serde(default)]
    pub merged_at: Option<Timestamp>,
    /// For a merged pull request: where the merge landed.
    #[serde(default)]
    pub landed: Option<Landed>,
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
    Land {
        report: MergeReport,
        /// When the forge merged, if it says; landing authority must have held then.
        #[serde(default)]
        merged_at: Option<Timestamp>,
    },
    /// An operation that does not land took effect.
    Confirmed,
    /// The operation had no effect; it may be planned again.
    Failed { reason: String },
    /// The forge still holds the request, for example an armed auto-merge.
    Pending { reason: String },
    /// The forge did something interlock cannot accept, or refused outright;
    /// the operator decides. `took_effect`: the forge did act (it merged), so
    /// the operation is recorded as confirmed while the task is blocked.
    Block {
        reason: String,
        #[serde(default)]
        took_effect: bool,
    },
    /// The forge could not say; the task is blocked until it can.
    Unknown { reason: String },
}

impl Verdict {
    pub fn block(reason: impl Into<String>) -> Verdict {
        Verdict::Block { reason: reason.into(), took_effect: false }
    }

    fn landed_badly(reason: String) -> Verdict {
        Verdict::Block { reason, took_effect: true }
    }
}

/// What a look at the forge decides for one operation. A G6 needs the forge
/// to show a merge of the pinned head, into the pinned branch, that git sees
/// on that branch, with the verified base as its first parent and the
/// verified tree as its result. For an operation still planned, nothing was
/// asked of the forge, so only a landing someone else made, or a moved head,
/// changes anything.
pub fn reconcile(op: &Operation, seen: &Observation, task: &Task) -> Verdict {
    let verdict = judge(op, seen, task);
    if op.state == OperationState::Planned {
        return match verdict {
            Verdict::Failed { .. } | Verdict::Unknown { .. } | Verdict::Confirmed => {
                Verdict::Pending { reason: "nothing was asked of the forge yet".into() }
            }
            v => v,
        };
    }
    verdict
}

fn judge(op: &Operation, seen: &Observation, task: &Task) -> Verdict {
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
    let retargeted = match (op.intent.base.as_deref(), pr.base_branch.as_deref()) {
        (Some(want), Some(got)) if want != got => Some((want, got)),
        _ => None,
    };
    match (op.kind, pr.state) {
        (OperationKind::OpenPr, PrState::Open | PrState::Merged) if at_head => Verdict::Confirmed,
        (OperationKind::OpenPr, _) => Verdict::Failed {
            reason: format!("pull request #{n} is {:?} at {}, not open at {expected}", pr.state, short(&pr.head)),
        },
        (OperationKind::DisarmAutoMerge, PrState::Open) if !pr.auto_merge => Verdict::Confirmed,
        (OperationKind::DisarmAutoMerge, PrState::Closed) => Verdict::Confirmed,
        (OperationKind::DisarmAutoMerge, PrState::Open) => {
            Verdict::Failed { reason: format!("auto-merge is still armed on pull request #{n}") }
        }
        (OperationKind::DisarmAutoMerge, PrState::Merged) => {
            Verdict::Failed { reason: format!("pull request #{n} merged before auto-merge was disarmed") }
        }
        (_, PrState::Merged) => landed(op, pr, task, retargeted),
        (_, PrState::Open) if !at_head => Verdict::Land {
            report: MergeReport::Refused {
                reason: format!(
                    "the head of pull request #{n} moved to {} before the merge was confirmed; it was pinned to {}",
                    short(&pr.head),
                    short(expected)
                ),
            },
            merged_at: None,
        },
        (_, PrState::Open) if retargeted.is_some() => {
            let (want, got) = retargeted.unwrap_or_default();
            Verdict::block(format!("pull request #{n} now targets {got}, not {want}"))
        }
        (_, PrState::Open) if pr.auto_merge => {
            Verdict::Pending { reason: format!("pull request #{n} is queued to merge at the verified head") }
        }
        (_, PrState::Open) => Verdict::Failed {
            reason: format!("pull request #{n} is still open at the verified head; it was not merged"),
        },
        (_, PrState::Closed) => Verdict::block(format!("pull request #{n} was closed without merging")),
    }
}

/// A merge the forge reports, checked against what the evidence was for.
fn landed(op: &Operation, pr: &PullRequest, task: &Task, retargeted: Option<(&str, &str)>) -> Verdict {
    let n = pr.number;
    let expected = op.intent.expected_head_sha.as_deref().unwrap_or("");
    if !same_sha(&pr.head, expected) {
        return Verdict::landed_badly(format!(
            "pull request #{n} was merged at {}, but interlock pinned {expected}; reconcile by hand",
            pr.head
        ));
    }
    if let Some((want, got)) = retargeted {
        return Verdict::landed_badly(format!(
            "pull request #{n} was merged into {got}, not {want}, which the evidence was for; reconcile by hand"
        ));
    }
    let base_branch = pr.base_branch.as_deref().unwrap_or("its base branch");
    let Some(landed) = &pr.landed else {
        return Verdict::Unknown {
            reason: format!("the forge reports #{n} merged, but where it landed could not be read"),
        };
    };
    if !landed.on_base {
        return Verdict::Unknown {
            reason: format!(
                "the forge reports #{n} merged, but {base_branch} as git sees it does not contain the merge"
            ),
        };
    }
    let base = task.input_snapshot.as_ref().map(|s| s.base_commit.as_str()).unwrap_or("");
    let onto = landed.onto.as_deref().unwrap_or("");
    if !same_sha(onto, base) {
        return Verdict::landed_badly(format!(
            "landed on a base nobody verified: pull request #{n} merged onto {onto}, but the evidence is for {base}"
        ));
    }
    let tree = task.current_tree.as_deref().unwrap_or("");
    let left = landed.tree.as_deref().unwrap_or("");
    if !same_sha(left, tree) {
        return Verdict::landed_badly(format!(
            "landed a tree nobody verified: pull request #{n} left {left} on {base_branch}, but the evidence covers {tree}"
        ));
    }
    Verdict::Land { report: MergeReport::Merged { head_sha: pr.head.clone() }, merged_at: pr.merged_at }
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
