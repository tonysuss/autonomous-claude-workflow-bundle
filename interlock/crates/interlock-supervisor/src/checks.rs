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
    /// For `Output`, the worktree to check. For `Base`, any directory in the repository.
    pub dir: &'a Path,
    /// Where base worktrees and temporary output files go.
    pub scratch: &'a Path,
    pub timeout: Duration,
}

/// Runs one criterion's check and records what happened.
pub fn run_check(store: &mut Store, req: CheckRequest<'_>) -> Result<CheckRun> {
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

    let (workdir, tree, cleanup): (PathBuf, String, Option<PathBuf>) = match req.target {
        RunTarget::Output => (req.dir.to_path_buf(), git::worktree_tree(req.dir)?, None),
        RunTarget::Base => {
            let base = task
                .input_snapshot
                .as_ref()
                .map(|s| s.base_commit.clone())
                .ok_or_else(|| RunError::Other("the task has no input snapshot yet".into()))?;
            let wt = req.scratch.join(format!("base-{id}"));
            git::worktree_add(req.dir, &wt, &base)?;
            let tree = git::tree_of(req.dir, &base)?;
            (wt.clone(), tree, Some(wt))
        }
    };

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
        tools: ToolPolicy::default(),
        model: None,
        max_turns: None,
        plugin_dir: None,
        env: vec![],
        timeout: req.timeout,
        transcript: transcript.clone(),
    };
    let outcome = interlock_adapter::run(&plan, &spec, &AtomicBool::new(false), |_| SessionSummary::default());
    let stderr_path = transcript.with_extension("stderr.txt");
    let mut output = std::fs::read(&transcript).unwrap_or_default();
    output.extend(std::fs::read(&stderr_path).unwrap_or_default());
    let _ = std::fs::remove_file(&transcript);
    let _ = std::fs::remove_file(&stderr_path);
    if let Some(wt) = cleanup {
        let _ = git::worktree_remove(req.dir, &wt);
        let _ = std::fs::remove_dir_all(&wt);
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
    Ok(run)
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
        assert_eq!(fixed.tree, git::worktree_tree(repo.path()).unwrap());
        assert!(
            !repo
                .path()
                .join(".interlock/scratch")
                .read_dir()
                .unwrap()
                .any(|e| { e.unwrap().file_name().to_string_lossy().starts_with("base-") }),
            "base worktrees are cleaned up"
        );
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
