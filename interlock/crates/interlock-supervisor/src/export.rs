//! Pausing safely. An export is a `wip:` commit of an attempt's worktree
//! whose message carries a resume note built from records, kept on
//! interlock's own ref `interlock/wip/<task>`. Nothing here moves the user's
//! branches or touches their index. `resume` makes an export's tree the next
//! worker's starting point.

use std::path::{Path, PathBuf};

use interlock_core::grants::{HostPolicy, Profile};
use interlock_schema::{Attempt, Role, Timestamp};
use interlock_store::Store;
use serde::Serialize;

use crate::git;
use crate::run::{Result, RunError};

#[derive(Debug, Clone, Serialize)]
pub struct Export {
    pub task_id: String,
    pub commit: String,
    pub tree: String,
    pub base: String,
    /// interlock's ref that points at the commit.
    pub reference: String,
    /// The worktree the tree was read from, or `accepted output`.
    pub source: String,
    pub attempt_id: Option<String>,
    /// The resume note, also kept in the commit message.
    pub note: PathBuf,
}

/// interlock's ref for a task's exported work in progress.
pub fn wip_ref(task_id: &str) -> String {
    format!("refs/heads/interlock/wip/{task_id}")
}

/// Exports the newest worker worktree that still exists, or else the last
/// accepted output.
pub fn export(
    store: &Store,
    repo: &Path,
    dir: &Path,
    task_id: &str,
    profile: Profile,
    now: Timestamp,
) -> Result<Export> {
    let task = store.task(task_id)?;
    let attempts = store.attempts(task_id)?;
    let live = attempts.iter().rev().filter(|a| a.role == Role::Worker).find_map(|a| {
        let wt = PathBuf::from(a.worktree.as_deref()?);
        wt.join(".git").exists().then_some((a, wt))
    });
    match (live, &task.current_tree) {
        (Some((attempt, wt)), _) => {
            let tree = git::worktree_tree(&wt)?;
            write(store, repo, dir, task_id, Some(attempt), &tree, &wt.display().to_string(), profile, now)
        }
        (None, Some(tree)) => write(store, repo, dir, task_id, None, tree, "accepted output", profile, now),
        (None, None) => Err(RunError::Other(format!(
            "task {task_id} has no worker worktree and no accepted output to export; an interrupted worker's work is \
             exported to {} when it stops",
            wip_ref(task_id)
        ))),
    }
}

/// Exports one attempt's worktree, if it changed anything since the attempt
/// started. Used when a worker is cancelled or found orphaned.
pub fn salvage(
    store: &Store,
    repo: &Path,
    dir: &Path,
    attempt: &Attempt,
    wt: &Path,
    profile: Profile,
    now: Timestamp,
) -> Result<Option<Export>> {
    if !wt.join(".git").exists() {
        return Ok(None);
    }
    let tree = git::worktree_tree(wt)?;
    if git::tree_of(wt, "HEAD")? == tree {
        return Ok(None);
    }
    write(store, repo, dir, &attempt.task_id, Some(attempt), &tree, &wt.display().to_string(), profile, now).map(Some)
}

#[allow(clippy::too_many_arguments)]
fn write(
    store: &Store,
    repo: &Path,
    dir: &Path,
    task_id: &str,
    attempt: Option<&Attempt>,
    tree: &str,
    source: &str,
    profile: Profile,
    now: Timestamp,
) -> Result<Export> {
    let task = store.task(task_id)?;
    let base = task
        .input_snapshot
        .as_ref()
        .map(|s| s.base_commit.clone())
        .ok_or_else(|| RunError::Other(format!("task {task_id} has no input snapshot to export against")))?;
    let reference = wip_ref(task_id);
    let short = reference.trim_start_matches("refs/heads/");
    let brief = store.brief(task_id, Role::Worker, profile, &HostPolicy::open(), now)?.to_markdown();
    let from = match attempt {
        Some(a) => format!("attempt {} (epoch {}, {})", a.id, a.epoch, source),
        None => source.to_string(),
    };
    let note = format!(
        "# Resume note for {task_id}\n\n\
         Exported {} from {from}, on base {base}. The task was {}.\n\n\
         `git diff {base} {short}` shows the work in progress. To continue from it:\n\n\
         ```\ninterlock task resume {task_id}\ninterlock run {task_id} --host <host>\n```\n\n{brief}",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        task.state,
    );
    let first_line = task.intent.lines().next().unwrap_or_default();
    let commit = git::commit_tree(repo, tree, Some(&base), &format!("wip: {task_id}: {first_line}\n\n{note}"))?;
    git::update_interlock_ref(repo, &reference, &commit, &format!("interlock: export {task_id}"))?;
    let exports = dir.join("exports");
    let path = exports.join(format!("{task_id}.md"));
    std::fs::create_dir_all(&exports)
        .and_then(|_| std::fs::write(&path, &note))
        .map_err(|e| RunError::Other(format!("cannot write {}: {e}", path.display())))?;
    Ok(Export {
        task_id: task_id.to_string(),
        commit,
        tree: tree.to_string(),
        base,
        reference,
        source: source.to_string(),
        attempt_id: attempt.map(|a| a.id.clone()),
        note: path,
    })
}

/// Makes an export (by default the task's `interlock/wip/<task>`) the next
/// worker's starting point. Returns the commit and its tree.
pub fn resume(
    store: &mut Store,
    repo: &Path,
    task_id: &str,
    from: Option<&str>,
    now: Timestamp,
) -> Result<(String, String)> {
    let rev = from.map(str::to_string).unwrap_or_else(|| wip_ref(task_id));
    let (commit, tree) = git::resolve_commit(repo, &rev)?;
    store.resume_from(task_id, &tree, now)?;
    Ok((commit, tree))
}
