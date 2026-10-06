//! interlock runs criteria checks itself, so a pass is something interlock
//! saw rather than something an agent reported.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chrono::Utc;
use interlock_adapter::{CommandPlan, Exit, SessionSpec, SessionSummary};
use interlock_schema::{CheckRun, Criterion, RunTarget, ToolPolicy};
use interlock_store::{NewCheckRun, Store};

use crate::git;
use crate::run::{Result, RunError};

pub struct CheckRequest<'a> {
    pub task_id: &'a str,
    pub criterion_id: &'a str,
    pub target: RunTarget,
    /// The attempt's id and token; `None` for the operator or supervisor.
    pub attempt: Option<(String, String)>,
    /// For `Output`, the worktree whose tree is checked. For `Base`, any directory in the repository.
    pub dir: &'a Path,
    /// Where base worktrees and temporary output files go.
    pub scratch: &'a Path,
    pub timeout: Duration,
}

/// Runs one criterion's check and records what happened.
pub fn run_check(store: &mut Store, req: CheckRequest<'_>) -> Result<CheckRun> {
    run_check_cancellable(store, req, &AtomicBool::new(false))?
        .ok_or_else(|| RunError::Other("the check was cancelled".into()))
}

/// Like [`run_check`], but stops the check's process group when `cancel` is
/// set. A cancelled run is not recorded, so it never counts as a failure.
pub fn run_check_cancellable(
    store: &mut Store,
    req: CheckRequest<'_>,
    cancel: &AtomicBool,
) -> Result<Option<CheckRun>> {
    let task = store.task(req.task_id)?;
    let criterion = task
        .criteria
        .iter()
        .find(|c| c.id == req.criterion_id)
        .ok_or_else(|| RunError::Other(format!("task {} has no criterion {}", task.id, req.criterion_id)))?;
    let command = criterion
        .check
        .clone()
        .ok_or_else(|| RunError::Other(format!("criterion {} has no check to run", criterion.id)))?;
    std::fs::create_dir_all(req.scratch).map_err(|e| RunError::Other(e.to_string()))?;
    let id = uuid::Uuid::new_v4().simple().to_string();

    // Every check runs in a fresh checkout of exactly the tree it is recorded
    // against. Run in the agent's own worktree, files the tree leaves out
    // (ignored or generated ones, such as a planted `.pyc`) could change the
    // result while it still counted for that tree. Base runs always worked
    // this way, so a task's checks already have to run from a clean checkout.
    let base = task.input_snapshot.as_ref().map(|s| s.base_commit.clone());
    let (commit, tree, at) = match req.target {
        RunTarget::Output => {
            let tree = git::worktree_tree(req.dir)?;
            let commit = git::commit_tree(req.dir, &tree, base.as_deref(), "interlock: tree under check")?;
            // Run from the same place inside the tree as the caller's directory.
            let root = git::toplevel(req.dir)?;
            let within =
                req.dir.canonicalize().ok().and_then(|d| {
                    root.canonicalize().ok().and_then(|r| d.strip_prefix(&r).ok().map(Path::to_path_buf))
                });
            (commit, tree, within.unwrap_or_default())
        }
        RunTarget::Base => {
            let base = base.ok_or_else(|| RunError::Other("the task has no input snapshot yet".into()))?;
            let tree = git::tree_of(req.dir, &base)?;
            (base, tree, PathBuf::new())
        }
    };
    let wt = req.scratch.join(format!("check-{id}"));
    git::worktree_add(req.dir, &wt, &commit)?;
    let (workdir, cleanup) = (wt.join(at), Some(wt));

    let transcript = req.scratch.join(format!("check-{id}.out"));
    let plan = CommandPlan {
        program: "sh".into(),
        args: vec!["-c".into(), command],
        stdin: None,
        // The check runs with no attempt credentials of its own.
        env: vec![("INTERLOCK_TOKEN".into(), String::new()), ("INTERLOCK_ATTEMPT".into(), String::new())],
    };
    let spec = SessionSpec {
        workdir,
        prompt: String::new(),
        append_system: None,
        agent: None,
        tools: ToolPolicy::default(),
        model: None,
        max_turns: None,
        plugin_dir: None,
        extra_plugin_dirs: vec![],
        env: vec![],
        timeout: req.timeout,
        transcript: transcript.clone(),
        session_id: None,
        max_cost_usd: None,
        effort: None,
        clear_env: false,
    };
    let outcome = interlock_adapter::run(&plan, &spec, cancel, |_| SessionSummary::default());
    let stderr_path = transcript.with_extension("stderr.txt");
    let mut output = std::fs::read(&transcript).unwrap_or_default();
    output.extend(std::fs::read(&stderr_path).unwrap_or_default());
    let _ = std::fs::remove_file(&transcript);
    let _ = std::fs::remove_file(&stderr_path);
    if let Some(wt) = cleanup {
        let _ = git::worktree_remove(req.dir, &wt);
        let _ = std::fs::remove_dir_all(&wt);
    }
    if outcome.exit == Exit::Cancelled {
        return Ok(None);
    }

    let run = store.record_check_run(
        NewCheckRun {
            task_id: task.id.clone(),
            criterion_id: criterion.id.clone(),
            attempt: req.attempt,
            target: req.target,
            tree,
            exit_code: outcome.exit_code,
            timed_out: outcome.exit == Exit::TimedOut,
            duration_ms: outcome.duration_ms,
            output,
        },
        Utc::now(),
    )?;
    Ok(Some(run))
}

/// Files in the base tree that a criterion's check command names. A result
/// may not change them, so a worker cannot weaken the check it is judged by.
pub fn protected_paths(repo: &Path, base: &str, criteria: &[Criterion]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for command in criteria.iter().filter_map(|c| c.check.as_deref()) {
        for token in command.split_whitespace() {
            let token = token.trim_matches(|c| matches!(c, '"' | '\'' | ';' | '(' | ')'));
            let token = token.strip_prefix("./").unwrap_or(token);
            if token.is_empty() || token.starts_with('-') || token.contains('=') || out.iter().any(|p| p == token) {
                continue;
            }
            if git::object_type(repo, &format!("{base}:{token}")).as_deref() == Some("blob") {
                out.push(token.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use interlock_core::lifecycle::TaskSpec;
    use interlock_schema::{RunProducer, Snapshot};
    use std::process::Command;

    fn setup() -> (tempfile::TempDir, Store, String) {
        let repo = git::tests::repo_with(&[
            ("calc.py", "def add(a, b):\n    return a - b\n"),
            ("checks/add.sh", "python3 -c 'import calc; assert calc.add(2, 3) == 5'\n"),
            ("tests/__init__.py", ""),
        ]);
        let mut store = Store::open(&repo.path().join(".interlock/state.db")).unwrap();
        let spec: TaskSpec = serde_json::from_value(serde_json::json!({
            "id": "t", "repository": ".", "workflow": "bug-fix", "intent": "fix add",
            "criteria": [
                {"id": "repro", "statement": "s", "check": "sh ./checks/add.sh", "min_strength": "tested",
                 "producer": "independent", "baseline": "fails"},
                {"id": "unit", "statement": "s", "check": "python3 -m unittest discover -s tests", "min_strength": "tested",
                 "producer": "self", "baseline": "passes"}
            ]
        }))
        .unwrap();
        let task = store.create_task(spec, Utc::now()).unwrap();
        let base = git::head(repo.path()).unwrap();
        let protected = protected_paths(repo.path(), &base, &task.criteria);
        let snapshot = Snapshot {
            repository: ".".into(),
            base_commit: base.clone(),
            untracked_hash: None,
            protected_paths: protected,
        };
        store.ready("t", snapshot, Utc::now()).unwrap();
        (repo, store, base)
    }

    fn check(store: &mut Store, repo: &Path, criterion: &str, target: RunTarget, dir: &Path) -> CheckRun {
        run_check(
            store,
            CheckRequest {
                task_id: "t",
                criterion_id: criterion,
                target,
                attempt: None,
                dir,
                scratch: &repo.join(".interlock/scratch"),
                timeout: Duration::from_secs(60),
            },
        )
        .unwrap()
    }

    #[test]
    fn check_files_named_by_commands_are_protected() {
        let (_repo, store, _) = setup();
        let protected = store.task("t").unwrap().input_snapshot.unwrap().protected_paths;
        assert_eq!(protected, vec!["checks/add.sh"], "files only; flags and directories are not");
    }

    #[test]
    fn base_and_output_runs_record_what_happened() {
        let (repo, mut store, base) = setup();
        let base_run = check(&mut store, repo.path(), "repro", RunTarget::Base, repo.path());
        assert!(base_run.failed(), "the reproduction fails on the base: {}", base_run.output_tail);
        assert_eq!(base_run.tree, git::tree_of(repo.path(), &base).unwrap());
        assert_eq!(base_run.producer, RunProducer::Operator);
        assert!(base_run.output_tail.contains("AssertionError"));

        let empty = check(&mut store, repo.path(), "unit", RunTarget::Base, repo.path());
        assert_eq!(empty.vacuous.as_deref(), Some("unittest ran 0 tests"), "{}", empty.output_tail);

        std::fs::write(repo.path().join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        let fixed = check(&mut store, repo.path(), "repro", RunTarget::Output, repo.path());
        assert!(fixed.passed(), "{}", fixed.output_tail);
        let after = git::worktree_tree(repo.path()).unwrap();
        assert_eq!(fixed.tree, after);
        assert!(
            !repo
                .path()
                .join(".interlock/scratch")
                .read_dir()
                .unwrap()
                .any(|e| { e.unwrap().file_name().to_string_lossy().starts_with("check-") }),
            "check worktrees are cleaned up"
        );
    }

    #[test]
    fn files_the_tree_leaves_out_cannot_change_an_output_check() {
        // A verifier plants bytecode compiled from a fixed `calc.py` beside the
        // buggy source, with the source's size and mtime, so Python would load
        // the fix. The tree, which leaves bytecode out, still holds the bug.
        let (repo, mut store, _) = setup();
        let p = repo.path();
        let src = p.join("calc.py");
        let stamp = std::fs::metadata(&src).unwrap().modified().unwrap();
        std::fs::write(&src, "def add(a, b):\n    return a + b\n").unwrap();
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(stamp).unwrap();
        let compiled = Command::new("python3").args(["-m", "py_compile", "calc.py"]).current_dir(p).status().unwrap();
        assert!(compiled.success());
        std::fs::write(&src, "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(stamp).unwrap();
        let planted = Command::new("sh").args(["-c", "sh ./checks/add.sh"]).current_dir(p).status().unwrap();
        assert!(planted.success(), "in the live worktree the planted bytecode makes the check pass");

        let run = check(&mut store, p, "repro", RunTarget::Output, p);
        assert!(run.failed(), "interlock checks the tree, which holds the bug: {}", run.output_tail);
        assert_eq!(run.tree, git::worktree_tree(p).unwrap(), "and records it against that tree");
    }

    #[test]
    fn a_check_that_hangs_times_out() {
        let (repo, mut store, _) = setup();
        let mut task = store.task("t").unwrap();
        task.criteria[0].check = Some("sleep 30".into());
        // Rewrite the criterion through the store's own connection for this test only.
        store
            .connection()
            .execute("UPDATE tasks SET record = ?1 WHERE id = 't'", [serde_json::to_string(&task).unwrap()])
            .unwrap();
        let run = run_check(
            &mut store,
            CheckRequest {
                task_id: "t",
                criterion_id: "repro",
                target: RunTarget::Output,
                attempt: None,
                dir: repo.path(),
                scratch: &repo.path().join(".interlock/scratch"),
                timeout: Duration::from_millis(300),
            },
        )
        .unwrap();
        assert!(run.timed_out && !run.passed() && !run.failed());
    }
}
