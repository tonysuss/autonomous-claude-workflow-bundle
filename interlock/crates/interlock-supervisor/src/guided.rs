//! Guidance mode: a person drives a host session and the generated skills call
//! interlock at each step. These helpers give that session what the supervisor
//! gives a headless one: a fresh worktree per attempt, and a way for the hooks
//! to find the attempt they govern when the session's environment names none.

use std::path::{Path, PathBuf};

use interlock_schema::{Attempt, AttemptStatus, Role};
use interlock_store::Store;
use serde::{Deserialize, Serialize};

use crate::git;
use crate::hook::HookContext;
use crate::run::{Result, RunError};

/// The file, next to the store, that names the attempt a guided session is in.
pub const ACTIVE_FILE: &str = "guided-attempt.json";

/// The attempt a guided session most recently opened in this checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    pub attempt_id: String,
    pub task_id: String,
    pub role: Role,
    pub worktree: Option<String>,
}

fn active_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join(ACTIVE_FILE)
}

/// Records `attempt` as the one the hooks govern. One guided attempt per checkout.
pub fn record_active(db: &Path, attempt: &Attempt) -> std::io::Result<()> {
    let active = Active {
        attempt_id: attempt.id.clone(),
        task_id: attempt.task_id.clone(),
        role: attempt.role,
        worktree: attempt.worktree.clone(),
    };
    std::fs::write(active_path(db), serde_json::to_string_pretty(&active)?)
}

/// The recorded attempt, while it is still open on a task that is still active.
pub fn active_attempt(db: &Path) -> Option<String> {
    let text = std::fs::read_to_string(active_path(db)).ok()?;
    let active: Active = serde_json::from_str(&text).ok()?;
    let store = Store::open(db).ok()?;
    let attempt = store.attempt(&active.attempt_id).ok()?;
    let task = store.task(&attempt.task_id).ok()?;
    let open = matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted);
    (open && !task.state.is_terminal()).then_some(attempt.id)
}

/// The hook context for a session. In interactive mode, a session whose
/// environment names no attempt is governed by the recorded guided attempt.
pub fn hook_context(db: PathBuf) -> HookContext {
    let mut ctx = HookContext::from_env(db);
    if ctx.attempt_id.is_none() && !ctx.headless {
        ctx.attempt_id = active_attempt(&ctx.db);
    }
    ctx
}

/// Creates a fresh worktree for the next `role` attempt on a task, under the
/// store's directory, and returns its path. Workers start from the input
/// snapshot, or from the last accepted output when reworking; verifiers and
/// reviewers start from the output under verification. No branch moves.
pub fn prepare_worktree(store: &Store, repo: &Path, db: &Path, task_id: &str, role: Role) -> Result<PathBuf> {
    let task = store.task(task_id)?;
    let base =
        task.input_snapshot.as_ref().map(|s| s.base_commit.clone()).ok_or_else(|| {
            RunError::Other(format!("task {task_id} has no input snapshot; run `interlock task ready`"))
        })?;
    let (name, commit) = match role {
        Role::Worker => {
            let commit = match &task.current_tree {
                Some(tree) => {
                    git::commit_tree(repo, tree, Some(&base), &format!("interlock: {task_id} previous output"))?
                }
                None => base,
            };
            (format!("{task_id}-worker-{}", task.attempts_used + 1), commit)
        }
        Role::Verifier | Role::Reviewer => {
            let tree = task
                .current_tree
                .clone()
                .ok_or_else(|| RunError::Other(format!("task {task_id} has no output to verify yet")))?;
            let n = store.attempts(task_id)?.iter().filter(|a| a.role == role).count() + 1;
            let role_name = if role == Role::Verifier { "verifier" } else { "reviewer" };
            let commit =
                git::commit_tree(repo, &tree, Some(&base), &format!("interlock: {task_id} output to {role_name}"))?;
            (format!("{task_id}-{role_name}-{n}"), commit)
        }
    };
    let dir = db.parent().unwrap_or(Path::new(".interlock")).join("worktrees").join(name);
    let _ = git::worktree_remove(repo, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    git::worktree_add(repo, &dir, &commit)?;
    Ok(dir)
}

/// Removes a worktree made by `prepare_worktree`, for an attempt that did not open.
pub fn discard_worktree(repo: &Path, dir: &Path) {
    let _ = git::worktree_remove(repo, dir);
    let _ = std::fs::remove_dir_all(dir);
}

/// The paths a result changes relative to the task's input snapshot, read from
/// the output tree itself, so a guided worker cannot leave a file off the list.
pub fn changed_paths(store: &Store, repo: &Path, task_id: &str, tree: &str) -> Result<Vec<String>> {
    let task = store.task(task_id)?;
    let base = task
        .input_snapshot
        .as_ref()
        .map(|s| s.base_commit.clone())
        .ok_or_else(|| RunError::Other(format!("task {task_id} has no input snapshot")))?;
    let base_tree = git::tree_of(repo, &base)?;
    Ok(git::changed_paths(repo, &base_tree, tree)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use interlock_core::capability::{Capability, CapabilitySet};
    use interlock_core::grants::{HostPolicy, Profile};
    use interlock_core::lifecycle::TaskSpec;
    use interlock_core::workflow::Mode;
    use interlock_schema::{HostRef, Snapshot};
    use interlock_store::{StartAttempt, Started, SubmitResult};
    use serde_json::json;

    fn caps() -> CapabilitySet {
        [Capability::ToolRestriction, Capability::PerCallPolicy, Capability::CustomAgents].into_iter().collect()
    }

    fn start(store: &mut Store, role: Role, worktree: &Path) -> (Attempt, String) {
        let started = store
            .start_attempt(
                StartAttempt {
                    task_id: "t".into(),
                    role,
                    mode: Mode::Interactive,
                    host: HostRef { host: "copilot".into(), version: "1".into() },
                    capabilities: caps(),
                    profile: Profile::Conservative,
                    host_policy: HostPolicy::open(),
                    agent: None,
                    model: None,
                    worktree: Some(worktree.display().to_string()),
                },
                Utc::now(),
            )
            .unwrap();
        let Started::Yes { attempt, token, .. } = started else { panic!("blocked") };
        (attempt, token)
    }

    #[test]
    fn worktrees_follow_the_task_and_the_hooks_find_the_open_attempt() {
        let repo = git::tests::repo_with(&[("calc.py", "def add(a, b):\n    return a - b\n")]);
        let db = repo.path().join(".interlock/state.db");
        let mut store = Store::open(&db).unwrap();
        let spec: TaskSpec = serde_json::from_value(json!({
            "id": "t", "repository": ".", "workflow": "bug-fix", "intent": "fix add",
            "criteria": [{"id": "c", "statement": "s", "min_strength": "tested", "producer": "self"}]
        }))
        .unwrap();
        store.create_task(spec, Utc::now()).unwrap();
        assert!(prepare_worktree(&store, repo.path(), &db, "t", Role::Worker).is_err(), "no snapshot yet");
        let base = git::head(repo.path()).unwrap();
        let snapshot =
            Snapshot { repository: ".".into(), base_commit: base, untracked_hash: None, protected_paths: vec![] };
        store.ready("t", snapshot, Utc::now()).unwrap();

        let wt = prepare_worktree(&store, repo.path(), &db, "t", Role::Worker).unwrap();
        assert!(wt.ends_with(".interlock/worktrees/t-worker-1") && wt.join("calc.py").exists());
        assert_eq!(active_attempt(&db), None, "nothing recorded yet");
        let (attempt, token) = start(&mut store, Role::Worker, &wt);
        record_active(&db, &attempt).unwrap();
        assert_eq!(active_attempt(&db).as_deref(), Some(attempt.id.as_str()));

        std::fs::write(wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        let tree = git::worktree_tree(&wt).unwrap();
        assert_eq!(changed_paths(&store, repo.path(), "t", &tree).unwrap(), vec!["calc.py"]);
        store
            .submit_result(
                SubmitResult {
                    attempt_id: attempt.id.clone(),
                    token,
                    epoch: attempt.epoch,
                    output_tree: tree.clone(),
                    changed_paths: vec!["calc.py".into()],
                    summary: "fixed".into(),
                    open_questions: vec![],
                    event_id: None,
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(active_attempt(&db).as_deref(), Some(attempt.id.as_str()), "a submitted attempt stays governed");

        let vwt = prepare_worktree(&store, repo.path(), &db, "t", Role::Verifier).unwrap();
        assert!(vwt.ends_with("t-verifier-1"));
        assert_eq!(git::worktree_tree(&vwt).unwrap(), tree, "the verifier sees exactly the output");
        let (verifier, vtoken) = start(&mut store, Role::Verifier, &vwt);
        record_active(&db, &verifier).unwrap();
        assert_eq!(active_attempt(&db).as_deref(), Some(verifier.id.as_str()));
        store.end_attempt(&verifier.id, &vtoken, false, None, Utc::now()).unwrap();
        assert_eq!(active_attempt(&db), None, "an ended attempt governs nothing");
        discard_worktree(repo.path(), &vwt);
        assert!(!vwt.exists());
    }
}
