//! The headless path: start worker and verifier sessions through a host
//! adapter, each in its own git worktree, and let the store's guards decide
//! every move. The supervisor holds tokens but never writes state directly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chrono::Utc;
use interlock_adapter::{Exit, Host, HostReport, Probe, SessionSpec, write_hooks_plugin};
use interlock_core::capability::CapabilitySet;
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::Move;
use interlock_core::workflow::Mode;
use interlock_schema::{HostRef, Role, Snapshot, State};
use interlock_store::{StartAttempt, Started, Store, SubmitResult};
use serde::Serialize;

use crate::{git, prompts};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Store(#[from] interlock_store::StoreError),
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, RunError>;

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub model: Option<String>,
    pub timeout: Duration,
    pub max_turns: Option<u32>,
    pub keep_worktrees: bool,
    /// Sessions to run before giving up, across workers and verifiers.
    pub max_sessions: u32,
    pub profile: Profile,
    /// The interlock binary the hooks and agents call.
    pub interlock_bin: PathBuf,
    /// Declared capabilities, instead of inspecting the host.
    pub capabilities: Option<CapabilitySet>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub role: Role,
    pub attempt_id: String,
    pub epoch: u32,
    pub exit: Exit,
    pub duration_ms: u64,
    pub summary: Option<String>,
    pub denials: u64,
    pub transcript: PathBuf,
    pub moves: Vec<Move>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub task_id: String,
    pub host: String,
    pub final_state: State,
    pub stopped_because: String,
    pub reconciled: Vec<String>,
    pub sessions: Vec<SessionReport>,
}

/// One controller per repository checkout. A live holder is detected through
/// /proc, so on systems without it a stale-looking lock is taken over.
struct Lock(PathBuf);

impl Lock {
    fn acquire(path: PathBuf) -> Result<Lock> {
        if let Ok(pid) = std::fs::read_to_string(&path) {
            let pid = pid.trim();
            if !pid.is_empty() && pid != std::process::id().to_string() && Path::new(&format!("/proc/{pid}")).exists() {
                return Err(RunError::Other(format!(
                    "another supervisor (pid {pid}) is running; lock at {}",
                    path.display()
                )));
            }
        }
        std::fs::write(&path, std::process::id().to_string())
            .map_err(|e| RunError::Other(format!("cannot write {}: {e}", path.display())))?;
        Ok(Lock(path))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub struct Supervisor {
    pub repo: PathBuf,
    pub db: PathBuf,
    store: Store,
    host: Box<dyn Host>,
    probe: Probe,
    cfg: RunConfig,
    pub cancel: Arc<AtomicBool>,
}

impl Supervisor {
    pub fn new(repo: PathBuf, db: PathBuf, host: Box<dyn Host>, probe: Probe, cfg: RunConfig) -> Result<Supervisor> {
        let store = Store::open(&db)?;
        Ok(Supervisor { repo, db, store, host, probe, cfg, cancel: Arc::new(AtomicBool::new(false)) })
    }

    fn dir(&self) -> PathBuf {
        self.db.parent().map(Path::to_path_buf).unwrap_or_else(|| self.repo.join(".interlock"))
    }

    fn capabilities(&mut self) -> Result<(CapabilitySet, String)> {
        if let Some(c) = &self.cfg.capabilities {
            return Ok((c.clone(), "declared".into()));
        }
        let report: HostReport = self.host.inspect(&self.probe);
        if !report.installed {
            return Err(RunError::Other(format!("{} is not installed: {}", report.host, report.notes.join("; "))));
        }
        let value = serde_json::to_value(&report).map_err(|e| RunError::Other(e.to_string()))?;
        self.store.save_host_report(self.host.name(), &value, Utc::now())?;
        Ok((report.capability_set(), report.version.unwrap_or_default()))
    }

    /// Drives a task until it is done, blocked, failed, needs the operator,
    /// or the session budget runs out.
    pub fn run(&mut self, task_id: &str) -> Result<RunReport> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| RunError::Other(e.to_string()))?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
        let _lock = Lock::acquire(dir.join("supervisor.lock"))?;
        let plugin = dir.join("plugin");
        write_hooks_plugin(&plugin, &self.cfg.interlock_bin).map_err(|e| RunError::Other(e.to_string()))?;

        let mut report = RunReport {
            task_id: task_id.into(),
            host: self.host.name().into(),
            final_state: self.store.task(task_id)?.state,
            stopped_because: String::new(),
            reconciled: vec![],
            sessions: vec![],
        };

        // Restart reconcile: whatever a previous controller left running is gone.
        let ended =
            self.store.reconcile_running(task_id, "the supervisor restarted; the session is gone", Utc::now())?;
        report.reconciled = ended.iter().map(|a| a.id.clone()).collect();
        if self.store.task(task_id)?.state == State::Running {
            self.store.retry(task_id, "reconciled after a supervisor restart", Utc::now())?;
        }

        let mut verifier_runs: HashMap<String, u32> = HashMap::new();
        let mut sessions = 0;
        let stopped = loop {
            if self.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                break "cancelled".to_string();
            }
            let task = self.store.task(task_id)?;
            match task.state {
                State::Pending => {
                    let snapshot = Snapshot {
                        repository: self.repo.display().to_string(),
                        base_commit: git::head(&self.repo)?,
                        untracked_hash: None,
                    };
                    if let Err(e) = self.store.ready(task_id, snapshot, Utc::now()) {
                        break format!("cannot start: {e}");
                    }
                }
                State::Ready | State::AwaitingVerification if sessions >= self.cfg.max_sessions => {
                    break format!("used all {} sessions this run allows", self.cfg.max_sessions);
                }
                State::Ready => {
                    sessions += 1;
                    let s = self.worker_session(task_id, &plugin)?;
                    report.sessions.push(s);
                }
                State::AwaitingVerification => {
                    let tree = task.current_tree.clone().unwrap_or_default();
                    let runs = verifier_runs.entry(tree).or_default();
                    if *runs >= 2 {
                        self.store.block(
                            task_id,
                            "two verifier sessions ended without decisive evidence",
                            Utc::now(),
                        )?;
                        continue;
                    }
                    *runs += 1;
                    sessions += 1;
                    let s = self.verifier_session(task_id, &plugin)?;
                    report.sessions.push(s);
                }
                State::Running => {
                    self.store.retry(task_id, "found running without a live session", Utc::now())?;
                }
                State::Verified => {
                    if self.store.advance(task_id, Utc::now())?.is_empty() {
                        break "verified; landing needs the operator (interlock integrate begin)".to_string();
                    }
                }
                State::Integrating => break "integrating; waiting for the forge".to_string(),
                State::Blocked => break format!("blocked: {}", task.blocked_reason.unwrap_or_default()),
                State::Done => break "done".to_string(),
                State::Failed => break "failed".to_string(),
                State::Cancelled => break "cancelled".to_string(),
            }
        };
        report.stopped_because = stopped;
        report.final_state = self.store.task(task_id)?.state;
        Ok(report)
    }

    fn start(
        &mut self,
        task_id: &str,
        role: Role,
        worktree: &Path,
    ) -> Result<std::result::Result<(interlock_schema::Attempt, String), Move>> {
        let (capabilities, version) = self.capabilities()?;
        let started = self.store.start_attempt(
            StartAttempt {
                task_id: task_id.into(),
                role,
                mode: Mode::Headless,
                host: HostRef { host: self.host.name().into(), version },
                capabilities,
                profile: self.cfg.profile,
                host_policy: HostPolicy::open(),
                agent: Some(format!("interlock-{}", if role == Role::Worker { "worker" } else { "verifier" })),
                model: self.cfg.model.clone(),
                worktree: Some(worktree.display().to_string()),
            },
            Utc::now(),
        )?;
        Ok(match started {
            Started::Yes { attempt, token, .. } => Ok((attempt, token)),
            Started::Blocked { moved } => Err(moved),
        })
    }

    fn env(&self, attempt_id: &str, token: &str, tree: Option<&str>) -> Vec<(String, String)> {
        let bin_dir = self.cfg.interlock_bin.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let path = format!("{bin_dir}:{}", std::env::var("PATH").unwrap_or_default());
        let mut env = vec![
            ("INTERLOCK_DB".to_string(), self.db.display().to_string()),
            ("INTERLOCK_ATTEMPT".to_string(), attempt_id.to_string()),
            ("INTERLOCK_TOKEN".to_string(), token.to_string()),
            ("INTERLOCK_MODE".to_string(), "headless".to_string()),
            ("INTERLOCK_HOST".to_string(), self.host.name().to_string()),
            ("PATH".to_string(), path),
        ];
        if let Some(t) = tree {
            env.push(("INTERLOCK_TREE".to_string(), t.to_string()));
        }
        env
    }

    fn spec(
        &self,
        workdir: &Path,
        prompt: String,
        system: &str,
        attempt: &interlock_schema::Attempt,
        env: Vec<(String, String)>,
        plugin: &Path,
    ) -> SessionSpec {
        SessionSpec {
            workdir: workdir.to_path_buf(),
            prompt,
            append_system: Some(system.to_string()),
            tools: attempt.effective_grant.tools.clone(),
            model: self.cfg.model.clone(),
            max_turns: self.cfg.max_turns,
            plugin_dir: Some(plugin.to_path_buf()),
            env,
            timeout: self.cfg.timeout,
            transcript: self.dir().join("transcripts").join(format!("{}.jsonl", attempt.id)),
        }
    }

    fn worker_session(&mut self, task_id: &str, plugin: &Path) -> Result<SessionReport> {
        let task = self.store.task(task_id)?;
        let base = task
            .input_snapshot
            .as_ref()
            .map(|s| s.base_commit.clone())
            .ok_or_else(|| RunError::Other("no snapshot".into()))?;
        // Rework continues from the last accepted output, so earlier work is kept.
        let start_commit = match &task.current_tree {
            Some(tree) => {
                git::commit_tree(&self.repo, tree, Some(&base), &format!("interlock: {task_id} previous output"))?
            }
            None => base.clone(),
        };
        let start_tree = git::tree_of(&self.repo, &start_commit)?;
        let wt = self.dir().join("worktrees").join(format!("{task_id}-worker-{}", task.attempts_used + 1));
        let (attempt, token) = match self.start(task_id, Role::Worker, &wt)? {
            Ok(v) => v,
            Err(moved) => return Ok(blocked_report(Role::Worker, moved)),
        };
        fresh_worktree(&self.repo, &wt, &start_commit)?;
        let brief = self.store.brief(task_id, Role::Worker, self.cfg.profile, &HostPolicy::open(), Utc::now())?;
        let env = self.env(&attempt.id, &token, None);
        let spec = self.spec(&wt, prompts::worker(&brief.to_markdown()), prompts::WORKER_SYSTEM, &attempt, env, plugin);
        let outcome = self.host.run_session(&self.probe, &spec, &self.cancel);

        let mut moves = Vec::new();
        if outcome.exit == Exit::Completed {
            let tree = git::worktree_tree(&wt)?;
            let changed = git::changed_paths(&self.repo, &start_tree, &tree)?;
            let applied = self.store.submit_result(
                SubmitResult {
                    attempt_id: attempt.id.clone(),
                    token: token.clone(),
                    epoch: attempt.epoch,
                    output_tree: tree,
                    changed_paths: changed,
                    summary: outcome.summary.final_text.clone().unwrap_or_else(|| "(no summary)".into()),
                    open_questions: vec![],
                    event_id: Some(format!("{}:result", attempt.id)),
                },
                Utc::now(),
            )?;
            moves.extend(applied.outcome.moved);
        } else {
            let note = format!("session ended: {:?}", outcome.exit);
            self.store.end_attempt(&attempt.id, &token, true, Some(note.clone()), Utc::now())?;
            moves.push(self.store.retry(task_id, &note, Utc::now())?);
        }
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, &wt);
        }
        Ok(session_report(Role::Worker, &attempt, outcome, moves))
    }

    fn verifier_session(&mut self, task_id: &str, plugin: &Path) -> Result<SessionReport> {
        let task = self.store.task(task_id)?;
        let tree = task.current_tree.clone().ok_or_else(|| RunError::Other("no output tree to verify".into()))?;
        let base = task.input_snapshot.as_ref().map(|s| s.base_commit.clone());
        let commit =
            git::commit_tree(&self.repo, &tree, base.as_deref(), &format!("interlock: {task_id} output to verify"))?;
        let n = self.store.attempts(task_id)?.iter().filter(|a| a.role == Role::Verifier).count() + 1;
        let wt = self.dir().join("worktrees").join(format!("{task_id}-verifier-{n}"));
        let (attempt, token) = match self.start(task_id, Role::Verifier, &wt)? {
            Ok(v) => v,
            Err(moved) => return Ok(blocked_report(Role::Verifier, moved)),
        };
        fresh_worktree(&self.repo, &wt, &commit)?;
        let brief = self.store.brief(task_id, Role::Verifier, self.cfg.profile, &HostPolicy::open(), Utc::now())?;
        let env = self.env(&attempt.id, &token, Some(&tree));
        let spec =
            self.spec(&wt, prompts::verifier(&brief.to_markdown()), prompts::VERIFIER_SYSTEM, &attempt, env, plugin);
        let outcome = self.host.run_session(&self.probe, &spec, &self.cancel);
        let failed = outcome.exit != Exit::Completed;
        self.store.end_attempt(
            &attempt.id,
            &token,
            failed,
            Some(format!("session ended: {:?}", outcome.exit)),
            Utc::now(),
        )?;
        let moves = self.store.advance(task_id, Utc::now())?;
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, &wt);
        }
        Ok(session_report(Role::Verifier, &attempt, outcome, moves))
    }
}

/// Replaces anything a crashed run left at `path` with a clean worktree.
fn fresh_worktree(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    let _ = git::worktree_remove(repo, path);
    let _ = std::fs::remove_dir_all(path);
    git::worktree_add(repo, path, commit)?;
    Ok(())
}

fn session_report(
    role: Role,
    attempt: &interlock_schema::Attempt,
    outcome: interlock_adapter::SessionOutcome,
    moves: Vec<Move>,
) -> SessionReport {
    SessionReport {
        role,
        attempt_id: attempt.id.clone(),
        epoch: attempt.epoch,
        exit: outcome.exit,
        duration_ms: outcome.duration_ms,
        summary: outcome.summary.final_text,
        denials: outcome.summary.denials,
        transcript: outcome.transcript,
        moves,
    }
}

fn blocked_report(role: Role, moved: Move) -> SessionReport {
    SessionReport {
        role,
        attempt_id: String::new(),
        epoch: 0,
        exit: Exit::Failed { reason: moved.reason.clone() },
        duration_ms: 0,
        summary: None,
        denials: 0,
        transcript: PathBuf::new(),
        moves: vec![moved],
    }
}
