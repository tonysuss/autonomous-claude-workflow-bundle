//! Delivery to a forge. GitHub is reached through `gh` and `git`, behind the
//! small `Forge` trait so tests can stand in for it.
//!
//! interlock lands only the verified head: a commit of the task's verified
//! tree on its snapshot base, pushed to `interlock/<task>` and merged with
//! `gh pr merge --match-head-commit`. Every call that changes the forge is
//! preceded by an operation row, and a restarted controller reconciles any
//! operation whose outcome it never heard.

pub mod deliver;
pub mod gh;
pub mod git;
#[doc(hidden)]
pub mod testing;

use interlock_core::delivery::{Landed, PrState, PullRequest, same_sha};
use interlock_schema::Timestamp;
use serde::{Deserialize, Serialize};

pub use deliver::{DeliverConfig, DeliverError, Delivery, Reconciled, integrate, reconcile};
pub use gh::GhForge;

/// Whether this process runs inside an interlock attempt. There, the forge
/// settings in the environment are ignored: an agent must not point interlock
/// at another `gh`, remote or repository.
pub fn inside_attempt() -> bool {
    std::env::var("INTERLOCK_ATTEMPT").is_ok_and(|v| !v.is_empty())
}

/// An environment setting for the forge, ignored inside an attempt.
pub(crate) fn forge_var(key: &str) -> Option<String> {
    if inside_attempt() {
        return None;
    }
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    /// The command could not run or gave no answer in time. Whatever it was
    /// asked to do may or may not have happened.
    #[error("{0}")]
    Unavailable(String),
    /// The command ran and reported failure.
    #[error("{message}")]
    Failed { code: i32, message: String },
    /// The command answered with something interlock cannot read.
    #[error("unexpected answer: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn flag(self) -> &'static str {
        match self {
            MergeMethod::Merge => "--merge",
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
        }
    }

    pub fn parse(s: &str) -> Option<MergeMethod> {
        match s {
            "merge" => Some(MergeMethod::Merge),
            "squash" => Some(MergeMethod::Squash),
            "rebase" => Some(MergeMethod::Rebase),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Pending,
    Failed,
    Skipped,
}

/// One CI check or commit status on a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
}

/// What the forge shows about one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrView {
    pub number: u64,
    pub url: String,
    pub state: PrState,
    pub draft: bool,
    pub head_branch: String,
    /// The head commit; for a merged pull request, the head it merged.
    pub head: String,
    pub base_branch: String,
    /// The base branch's current commit, when the forge reports it.
    pub base: Option<String>,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: Option<String>,
    /// GitHub's mergeStateStatus: CLEAN, BLOCKED, BEHIND, DIRTY, UNSTABLE, ...
    pub merge_state: Option<String>,
    pub checks: Vec<Check>,
    pub merge_commit: Option<String>,
    pub merged_at: Option<Timestamp>,
    pub auto_merge: bool,
}

impl PrView {
    /// The host-neutral view the core's reconcile rules read. Where a merge
    /// landed is added separately, from git.
    pub fn observed(&self) -> PullRequest {
        PullRequest {
            number: self.number,
            state: self.state,
            head: self.head.clone(),
            auto_merge: self.auto_merge,
            base_branch: Some(self.base_branch.clone()).filter(|b| !b.is_empty()),
            merged_at: self.merged_at,
            landed: None,
        }
    }
}

/// A pull request to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPr {
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

/// The forge, as interlock uses it. Every method that changes the forge is
/// called only after an operation row for it is committed.
pub trait Forge {
    fn name(&self) -> &str;

    /// The branch pull requests target when none is configured.
    fn default_branch(&self) -> Result<String, ForgeError>;

    /// The commit `branch` holds on the forge, or `None` if it does not exist.
    fn branch_head(&self, branch: &str) -> Result<Option<String>, ForgeError>;

    /// Pushes `commit` to `branch`, only if the branch holds `lease` now
    /// (`None`: only if it does not exist yet).
    fn push(&self, commit: &str, branch: &str, lease: Option<&str>) -> Result<(), ForgeError>;

    /// Fetches `branch` into the local repository at `into` and returns its commit.
    fn fetch(&self, branch: &str, into: &str) -> Result<String, ForgeError>;

    /// The newest pull request whose head is `branch`, open ones first, in any state.
    fn find_pr(&self, branch: &str) -> Result<Option<PrView>, ForgeError>;

    fn view_pr(&self, number: u64) -> Result<PrView, ForgeError>;

    /// Opens a pull request and returns its number.
    fn create_pr(&self, pr: &NewPr) -> Result<u64, ForgeError>;

    /// Merges pull request `number` only if its head is still `head`. With
    /// `auto`, arms auto-merge pinned to the same head instead.
    fn merge(&self, number: u64, head: &str, method: MergeMethod, auto: bool) -> Result<(), ForgeError>;

    /// Turns auto-merge off on pull request `number`.
    fn disarm(&self, number: u64) -> Result<(), ForgeError>;

    /// Where merge commit `merge` landed, read with git rather than the
    /// forge's API: fetches `base_branch` into `into`, then reads whether it
    /// contains the merge, the merge's first parent and its tree.
    fn landed(&self, merge: &str, base_branch: &str, into: &str) -> Result<Landed, ForgeError>;
}

/// Whether a pull request may be merged at the verified head now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "readiness", content = "detail", rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    /// Not yet: checks are running, or the forge is still computing mergeability.
    Waiting(String),
    Merged {
        head: String,
    },
    /// The head is not the verified head.
    HeadMoved {
        found: String,
    },
    /// The base branch is no longer the commit the work was verified on.
    BaseMoved {
        found: String,
    },
    /// The forge will not merge it as it stands; the operator decides.
    Refused(String),
}

/// The readiness observer: reads checks, mergeability, and whether the head
/// and base are still the ones the evidence is about.
pub fn readiness(pr: &PrView, head: &str, base_branch: &str, base: &str) -> Readiness {
    let n = pr.number;
    match pr.state {
        PrState::Merged => return Readiness::Merged { head: pr.head.clone() },
        PrState::Closed => return Readiness::Refused(format!("pull request #{n} was closed without merging")),
        PrState::Open => {}
    }
    if !same_sha(&pr.head, head) {
        return Readiness::HeadMoved { found: pr.head.clone() };
    }
    if pr.base_branch != base_branch {
        return Readiness::Refused(format!(
            "pull request #{n} targets {}, but the evidence is for {base_branch}",
            if pr.base_branch.is_empty() { "an unknown branch" } else { &pr.base_branch }
        ));
    }
    if let Some(found) = pr.base.as_deref().filter(|b| !same_sha(b, base)) {
        return Readiness::BaseMoved { found: found.to_string() };
    }
    let named = |status| pr.checks.iter().filter(|c| c.status == status).map(|c| c.name.as_str()).collect::<Vec<_>>();
    let failed = named(CheckStatus::Failed);
    if !failed.is_empty() {
        return Readiness::Refused(format!("checks failed on pull request #{n}: {}", failed.join(", ")));
    }
    let mergeable = pr.mergeable.as_deref().unwrap_or("UNKNOWN");
    let merge_state = pr.merge_state.as_deref().unwrap_or("UNKNOWN");
    if mergeable == "CONFLICTING" || merge_state == "DIRTY" {
        return Readiness::Refused(format!("pull request #{n} conflicts with {}", pr.base_branch));
    }
    let pending = named(CheckStatus::Pending);
    if !pending.is_empty() {
        return Readiness::Waiting(format!("checks pending on pull request #{n}: {}", pending.join(", ")));
    }
    if pr.draft {
        return Readiness::Waiting(format!("pull request #{n} is a draft"));
    }
    match (mergeable, merge_state) {
        ("UNKNOWN", _) | (_, "UNKNOWN") => {
            Readiness::Waiting(format!("the forge is still working out whether #{n} can merge"))
        }
        (_, "BLOCKED") => Readiness::Waiting(format!(
            "pull request #{n} is blocked by branch protection, for example a required review"
        )),
        (_, "BEHIND") => Readiness::Waiting(format!("pull request #{n} must be brought up to date first")),
        _ => Readiness::Ready,
    }
}
