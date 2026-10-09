//! Local git plumbing for delivery. It writes objects and refs under
//! `refs/interlock/`, and never moves a branch or touches the index.

use std::path::Path;
use std::process::Command;

use interlock_schema::Task;

#[derive(Debug, thiserror::Error)]
#[error("git {args}: {message}")]
pub struct GitError {
    pub args: String,
    pub message: String,
}

fn git(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<std::process::Output, GitError> {
    Command::new("git")
        .args(args)
        .current_dir(repo)
        .envs(env.iter().copied())
        .output()
        .map_err(|e| GitError { args: args.join(" "), message: e.to_string() })
}

fn git_ok(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, GitError> {
    let out = git(repo, args, env)?;
    if !out.status.success() {
        return Err(GitError { args: args.join(" "), message: String::from_utf8_lossy(&out.stderr).trim().into() });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The commit interlock lands for a task: its verified tree, with the
/// snapshot base as the only parent. Author, committer and date are fixed
/// (the date is the task's creation time) and the commit is never signed, so
/// the same tree and base always give the same id: a restarted controller
/// rebuilds exactly the head it pinned. No ref is written here.
pub fn verified_head(repo: &Path, task: &Task) -> Result<String, GitError> {
    let missing =
        |what: &str| GitError { args: "commit-tree".into(), message: format!("task {} has no {what}", task.id) };
    let tree = task.current_tree.as_deref().ok_or_else(|| missing("verified tree"))?;
    let base = task.input_snapshot.as_ref().map(|s| s.base_commit.as_str()).ok_or_else(|| missing("snapshot"))?;
    let date = format!("@{} +0000", task.created_at.timestamp());
    let subject = task.intent.lines().next().unwrap_or_default();
    let message = format!(
        "{subject}\n\nVerified by interlock. Landed only at this commit.\n\nInterlock-Task: {}\nInterlock-Tree: {tree}\n",
        task.id
    );
    let env = [
        ("GIT_AUTHOR_NAME", "interlock"),
        ("GIT_AUTHOR_EMAIL", "interlock@localhost"),
        ("GIT_COMMITTER_NAME", "interlock"),
        ("GIT_COMMITTER_EMAIL", "interlock@localhost"),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    git_ok(repo, &["commit-tree", "--no-gpg-sign", tree, "-p", base, "-m", &message], &env)
}

/// Where interlock keeps a task's refs. Not a branch.
pub fn ref_prefix(task_id: &str) -> String {
    format!("refs/{}", interlock_core::delivery::branch_for(task_id))
}

/// Keeps the pinned head from garbage collection while the task integrates,
/// under `refs/interlock/<task>/head`. Written only after G5.
pub fn keep_head(repo: &Path, task_id: &str, head: &str) -> Result<(), GitError> {
    git_ok(repo, &["update-ref", &format!("{}/head", ref_prefix(task_id)), head], &[]).map(|_| ())
}

/// Deletes every ref interlock keeps for a task.
pub fn forget(repo: &Path, task_id: &str) -> Result<(), GitError> {
    // A pattern matches refs below it up to a slash, so `refs/interlock/a` never matches `refs/interlock/ab/head`.
    let refs = git_ok(repo, &["for-each-ref", "--format=%(refname)", &ref_prefix(task_id)], &[])?;
    for r in refs.lines().filter(|l| !l.is_empty()) {
        git_ok(repo, &["update-ref", "-d", r], &[])?;
    }
    Ok(())
}

/// The tree that merging `head` into `base` gives, or the conflicted paths.
pub fn merge_tree(repo: &Path, base: &str, head: &str) -> Result<Result<String, Vec<String>>, GitError> {
    let args = ["merge-tree", "--write-tree", "--name-only", "--no-messages", base, head];
    let out = git(repo, &args, &[])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let tree = lines.next().unwrap_or_default().trim().to_string();
    match out.status.code() {
        Some(0) => Ok(Ok(tree)),
        Some(1) => Ok(Err(lines.map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())),
        _ => Err(GitError { args: args.join(" "), message: String::from_utf8_lossy(&out.stderr).trim().into() }),
    }
}

pub fn tree_of(repo: &Path, commit: &str) -> Result<String, GitError> {
    git_ok(repo, &["rev-parse", &format!("{commit}^{{tree}}")], &[])
}
