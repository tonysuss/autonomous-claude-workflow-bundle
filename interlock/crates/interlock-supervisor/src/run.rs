//! The headless path: start worker and verifier sessions through a host
//! adapter, each in its own git worktree, and let the store's guards decide
//! every move. The supervisor holds tokens but never writes state directly.
//!
//! Sessions outlive the supervisor. Each session's handoff (process, group,
//! deadline, transcript, host session id) is recorded before the host starts,
//! so a restarted supervisor re-attaches to a session that is still running
//! and carries on as if it had never stopped, and ends one that is gone with
//! a synthetic failure report before retrying (R3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use interlock_adapter::{Exit, Host, HostReport, Probe, SessionOutcome, SessionSpec, Target, write_hooks_plugin};
use interlock_core::budget;
use interlock_core::capability::CapabilitySet;
use interlock_core::evidence;
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::Move;
use interlock_core::workflow::Mode;
use interlock_schema::{
    Attempt, AttemptEnd, AttemptStatus, Baseline, EndReason, Handoff, HostRef, ResultStatus, Role, RunTarget, Snapshot,
    Spent, State, Task,
};
use interlock_store::{SessionEnd, StartAttempt, Started, Store, SubmitResult};
use serde::Serialize;

use crate::config::{Config, PinStatus};
use crate::export::{self, Export};
use crate::{checks, git, prompts};

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
    /// Why the session ended; `None` when no session started.
    pub ended_because: Option<EndReason>,
    pub end_detail: Option<String>,
    pub spent: Spent,
    pub host_session_id: Option<String>,
    /// A restarted supervisor followed this session to its end.
    pub reattached: bool,
    /// Where a stopped worker's unfinished work was exported.
    pub export: Option<Export>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub task_id: String,
    pub host: String,
    pub final_state: State,
    pub stopped_because: String,
    /// Attempts a previous supervisor left running whose sessions were gone.
    pub reconciled: Vec<String>,
    /// Attempts whose sessions were still running and were followed to their end.
    pub reattached: Vec<String>,
    /// A signal stopped the run; the task can resume.
    pub interrupted: bool,
    /// interlock's runs of each baseline check on the input snapshot.
    pub baseline: Vec<BaselineRun>,
    pub sessions: Vec<SessionReport>,
    /// The host's pinned version, when `.interlock/config.toml` pins one.
    pub pin: Option<PinStatus>,
    /// What the task's sessions have used so far, across every run.
    pub spent: Spent,
    /// Work salvaged from orphaned workers.
    pub exports: Vec<Export>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BaselineRun {
    pub criterion_id: String,
    pub run_id: String,
    pub exit_code: Option<i32>,
    pub vacuous: Option<String>,
    pub problem: Option<String>,
}

/// One controller per repository checkout. The lock file is created
/// atomically; a holder that is gone (checked through /proc) is replaced.
struct Lock(PathBuf);

impl Lock {
    fn acquire(path: PathBuf) -> Result<Lock> {
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    std::io::Write::write_all(&mut f, std::process::id().to_string().as_bytes())
                        .map_err(|e| RunError::Other(format!("cannot write {}: {e}", path.display())))?;
                    return Ok(Lock(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let holder = std::fs::read_to_string(&path).unwrap_or_default();
                    let live = holder.trim().parse::<u32>().is_ok_and(|pid| interlock_adapter::alive(pid, None));
                    if live {
                        return Err(RunError::Other(format!(
                            "another supervisor (pid {}) is running; lock at {}",
                            holder.trim(),
                            path.display()
                        )));
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(RunError::Other(format!("cannot create {}: {e}", path.display()))),
            }
        }
        Err(RunError::Other(format!("another supervisor took the lock at {} first", path.display())))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Stops a session when the run is interrupted, the task is cancelled or
/// fails, or someone else ends the attempt, for example with
/// `interlock task cancel` from another terminal.
struct Watch {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const WATCH_EVERY: Duration = Duration::from_millis(250);

pub struct Supervisor {
    pub repo: PathBuf,
    pub db: PathBuf,
    store: Store,
    host: Box<dyn Host>,
    probe: Probe,
    cfg: RunConfig,
    /// Set by a signal handler (or anyone) to stop the run: the running
    /// session is stopped, its attempt cancelled, and the task left to resume.
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
    /// the session budget runs out, or the run is interrupted.
    pub fn run(&mut self, task_id: &str) -> Result<RunReport> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| RunError::Other(e.to_string()))?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
        let _lock = Lock::acquire(dir.join("supervisor.lock"))?;
        let config = Config::load(&dir).map_err(RunError::Other)?;
        let plugin = dir.join("plugin");
        write_hooks_plugin(&plugin, &self.cfg.interlock_bin).map_err(|e| RunError::Other(e.to_string()))?;

        let mut report = RunReport {
            task_id: task_id.into(),
            host: self.host.name().into(),
            final_state: self.store.task(task_id)?.state,
            stopped_because: String::new(),
            reconciled: vec![],
            reattached: vec![],
            interrupted: false,
            baseline: vec![],
            sessions: vec![],
            pin: None,
            spent: Spent::default(),
            exports: vec![],
        };

        self.restart_reconcile(task_id, &mut report)?;

        if let Some(pinned) = config.pin(self.host.name()) {
            let installed = self.host.inspect(&self.probe).version;
            let pin = PinStatus::of(Some(pinned), installed.as_deref());
            let state = self.store.task(task_id)?.state;
            if let Some(why) = pin.refusal(self.host.name())
                && !state.is_terminal()
                && state != State::Blocked
            {
                self.store.block(task_id, &why, Utc::now())?;
            }
            report.pin = Some(pin);
        }

        let mut verifier_runs: HashMap<String, u32> = HashMap::new();
        let mut baseline_checked = false;
        let mut sessions = 0;
        let stopped = loop {
            if self.cancel.load(Ordering::SeqCst) {
                report.interrupted = true;
                let state = self.store.task(task_id)?.state;
                break format!("interrupted; the running session was stopped and the task can resume from {state}");
            }
            let mut task = self.store.task(task_id)?;
            if matches!(task.state, State::Ready | State::AwaitingVerification)
                && self.store.enforce_budget(task_id, Utc::now())?.is_some()
            {
                task = self.store.task(task_id)?;
            }
            match task.state {
                State::Pending => {
                    let base_commit = git::head(&self.repo)?;
                    let snapshot = Snapshot {
                        repository: self.repo.display().to_string(),
                        protected_paths: checks::protected_paths(&self.repo, &base_commit, &task.criteria),
                        base_commit,
                        untracked_hash: None,
                    };
                    if let Err(e) = self.store.ready(task_id, snapshot, Utc::now()) {
                        break format!("cannot start: {e}");
                    }
                }
                State::Ready | State::AwaitingVerification if sessions >= self.cfg.max_sessions => {
                    break format!("used all {} sessions this run allows", self.cfg.max_sessions);
                }
                State::Ready if !baseline_checked => {
                    baseline_checked = true;
                    report.baseline = self.baseline(task_id)?;
                    let problems: Vec<String> = report
                        .baseline
                        .iter()
                        .filter_map(|b| b.problem.as_ref().map(|p| format!("{}: {p}", b.criterion_id)))
                        .collect();
                    if !problems.is_empty() {
                        self.store.block(
                            task_id,
                            &format!("fix the task's checks first. {}", problems.join("; ")),
                            Utc::now(),
                        )?;
                    }
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
                State::Failed => {
                    let why = self.store.transitions(task_id)?.last().map(|t| t.reason.clone()).unwrap_or_default();
                    break if why.is_empty() { "failed".to_string() } else { format!("failed: {why}") };
                }
                State::Cancelled => break "cancelled".to_string(),
            }
        };
        report.stopped_because = stopped;
        report.final_state = self.store.task(task_id)?.state;
        report.spent = self.store.spent(task_id)?;
        Ok(report)
    }

    /// Whatever a previous controller left running: follow a session that is
    /// still alive to its end; end one that is gone with a synthetic failure
    /// report and salvage its worktree. Then R3 if the task is still running.
    fn restart_reconcile(&mut self, task_id: &str, report: &mut RunReport) -> Result<()> {
        let running: Vec<Attempt> =
            self.store.attempts(task_id)?.into_iter().filter(|a| a.status == AttemptStatus::Running).collect();
        for attempt in running {
            let id = attempt.id.clone();
            let handoff = attempt.handoff.clone();
            let gone = match &handoff {
                Some(h) if interlock_adapter::alive(h.pid, h.process_start) => match self.load_token(&id) {
                    Some(token) => {
                        let s = self.reattach(task_id, &attempt, &token, h)?;
                        report.reattached.push(id.clone());
                        report.sessions.push(s);
                        None
                    }
                    None => {
                        // A session that cannot report is stopped rather than left to run unseen.
                        interlock_adapter::attach(&target_of(h), &AtomicBool::new(true), |_| Default::default());
                        Some(format!(
                            "the session (pid {}) was running, but its credentials were lost; stopped it",
                            h.pid
                        ))
                    }
                },
                Some(h) => {
                    Some(format!("the session's process (pid {}) was gone when the supervisor restarted", h.pid))
                }
                None => Some("the supervisor stopped before the session started".to_string()),
            };
            let Some(detail) = gone else { continue };
            let spent = handoff.as_ref().map(salvage_spent);
            self.store.reconcile_attempt(&id, EndReason::Crash, &detail, spent, Utc::now())?;
            self.forget_token(&id);
            report.reconciled.push(id);
            if let Some(wt) = attempt.worktree.as_deref().map(PathBuf::from) {
                if attempt.role == Role::Worker
                    && let Ok(Some(e)) = export::salvage(
                        &self.store,
                        &self.repo,
                        &self.dir(),
                        &attempt,
                        &wt,
                        self.cfg.profile,
                        Utc::now(),
                    )
                {
                    report.exports.push(e);
                }
                if !self.cfg.keep_worktrees {
                    let _ = git::worktree_remove(&self.repo, &wt);
                }
            }
        }
        if self.store.task(task_id)?.state == State::Running {
            self.store.retry(task_id, "the worker's session was gone after a supervisor restart", Utc::now())?;
        }
        Ok(())
    }

    /// Follows a session a previous supervisor started, then finishes it
    /// exactly as that supervisor would have.
    fn reattach(&mut self, task_id: &str, attempt: &Attempt, token: &str, h: &Handoff) -> Result<SessionReport> {
        self.store.note_reattach(&attempt.id, token, Utc::now())?;
        let host =
            interlock_adapter::host(&h.host).ok_or_else(|| RunError::Other(format!("unknown host {}", h.host)))?;
        let watch = self.watch(task_id, &attempt.id);
        let outcome = interlock_adapter::attach(&target_of(h), &watch.stop, |l| host.summarize(l));
        drop(watch);
        let wt = PathBuf::from(attempt.worktree.clone().unwrap_or_default());
        let session = h.host_session_id.clone();
        let mut s = match attempt.role {
            Role::Worker => {
                let task = self.store.task(task_id)?;
                let base = task
                    .input_snapshot
                    .as_ref()
                    .map(|s| s.base_commit.clone())
                    .ok_or_else(|| RunError::Other("no snapshot".into()))?;
                let base_tree = git::tree_of(&self.repo, &base)?;
                self.finish_worker(task_id, attempt, token, &wt, &base_tree, outcome, h.budget_deadline, session)?
            }
            Role::Verifier | Role::Reviewer => {
                self.finish_verifier(task_id, attempt, token, &wt, outcome, h.budget_deadline, session)?
            }
        };
        s.reattached = true;
        Ok(s)
    }

    /// Runs each baseline check on the input snapshot, once per task, before
    /// any worker spends effort: a reproduction that already passes, or a
    /// regression guard that fails or tests nothing, makes the task unsound.
    fn baseline(&mut self, task_id: &str) -> Result<Vec<BaselineRun>> {
        let task = self.store.task(task_id)?;
        let existing = self.store.check_runs(task_id)?;
        let scratch = self.dir().join("scratch");
        let mut out = Vec::new();
        for c in task.criteria.iter().filter(|c| c.check.is_some() && c.baseline != Baseline::Any) {
            let done = existing
                .iter()
                .find(|r| r.criterion_id == c.id && r.target == RunTarget::Base && r.check_version == c.check_version);
            let run = match done {
                Some(r) => r.clone(),
                None => checks::run_check(
                    &mut self.store,
                    checks::CheckRequest {
                        task_id,
                        criterion_id: &c.id,
                        target: RunTarget::Base,
                        attempt: None,
                        dir: &self.repo,
                        scratch: &scratch,
                        timeout: self.cfg.timeout,
                    },
                )?,
            };
            let runs = self.store.check_runs(task_id)?;
            out.push(BaselineRun {
                criterion_id: c.id.clone(),
                run_id: run.id.clone(),
                exit_code: run.exit_code,
                vacuous: run.vacuous.clone(),
                problem: evidence::baseline_problem(&task, c, &runs),
            });
        }
        Ok(out)
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

    /// The session's limits: the configured timeout, or what is left of the
    /// task's wall-clock budget if that is less (`true` when the budget sets
    /// the deadline), and what is left of its cost budget.
    fn limits(&self, task: &Task) -> Result<(Duration, bool, Option<f64>)> {
        let spent = self.store.spent(&task.id)?;
        let cost_left = budget::cost_left_usd(&task.budget, &spent);
        Ok(match budget::wall_left_ms(&task.budget, &spent).map(Duration::from_millis) {
            Some(left) if left < self.cfg.timeout => (left, true, cost_left),
            _ => (self.cfg.timeout, false, cost_left),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn spec(
        &self,
        workdir: &Path,
        prompt: String,
        system: &str,
        attempt: &interlock_schema::Attempt,
        env: Vec<(String, String)>,
        plugin: &Path,
        timeout: Duration,
        max_cost_usd: Option<f64>,
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
            timeout,
            transcript: self.dir().join("transcripts").join(format!("{}.jsonl", attempt.id)),
            session_id: Some(uuid::Uuid::new_v4().to_string()),
            max_cost_usd,
        }
    }

    /// Starts a session in three steps: spawn the session's process group
    /// held at its gate, record the handoff in its own transaction, and only
    /// then let the host start.
    fn launch(
        &mut self,
        task_id: &str,
        attempt: &Attempt,
        token: &str,
        spec: &SessionSpec,
        budget_deadline: bool,
    ) -> SessionOutcome {
        let failed = |reason: String| SessionOutcome {
            exit: Exit::Failed { reason },
            exit_code: None,
            signal: None,
            duration_ms: 0,
            summary: Default::default(),
            transcript: spec.transcript.clone(),
        };
        let plan = match self.host.plan(&self.probe, spec) {
            Ok(p) => p,
            Err(reason) => return failed(format!("could not start the host: {reason}")),
        };
        let mut spawned = match interlock_adapter::spawn(&plan, spec) {
            Ok(s) => s,
            Err(outcome) => return *outcome,
        };
        let now = Utc::now();
        let handoff = Handoff {
            host: self.host.name().into(),
            pid: spawned.pid,
            pgid: spawned.pgid,
            process_start: spawned.process_start,
            started_at: now,
            deadline: now + chrono::Duration::from_std(spec.timeout).unwrap_or(chrono::Duration::MAX),
            budget_deadline,
            transcript: spec.transcript.display().to_string(),
            host_session_id: spec.session_id.clone(),
            supervisor_pid: std::process::id(),
            reattached_at: vec![],
        };
        if let Err(e) = self.store.record_handoff(&attempt.id, token, handoff) {
            spawned.abandon();
            return failed(format!("could not start the host: the handoff was not recorded: {e}"));
        }
        if let Err(e) = spawned.release() {
            return failed(format!("could not start the host: {e}"));
        }
        let watch = self.watch(task_id, &attempt.id);
        let host = &self.host;
        spawned.wait(spec, &watch.stop, |l| host.summarize(l))
    }

    fn watch(&self, task_id: &str, attempt_id: &str) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (cancel, db) = (self.cancel.clone(), self.db.clone());
        let (task, attempt) = (task_id.to_string(), attempt_id.to_string());
        let (s, d) = (stop.clone(), done.clone());
        let thread = std::thread::spawn(move || {
            let store = Store::open(&db).ok();
            let mut checked = Instant::now();
            while !d.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    s.store(true, Ordering::SeqCst);
                    return;
                }
                if checked.elapsed() >= WATCH_EVERY
                    && let Some(store) = &store
                {
                    checked = Instant::now();
                    let task_over = store.task(&task).is_ok_and(|t| t.state.is_terminal());
                    let attempt_over = store.attempt(&attempt).is_ok_and(|a| !a.status.is_open());
                    if task_over || attempt_over {
                        s.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Watch { stop, done, thread: Some(thread) }
    }

    fn token_path(&self, attempt_id: &str) -> PathBuf {
        self.dir().join("sessions").join(format!("{attempt_id}.token"))
    }

    /// Keeps an attempt's token beside the store, readable only by this
    /// user, so a restarted supervisor can finish the attempt. The store
    /// itself keeps only its hash.
    fn save_token(&self, attempt_id: &str, token: &str) -> Result<()> {
        let path = self.token_path(attempt_id);
        let io = |e: std::io::Error| RunError::Other(format!("cannot write {}: {e}", path.display()));
        std::fs::create_dir_all(path.parent().expect("token path has a parent")).map_err(io)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&path).map_err(io)?;
        std::io::Write::write_all(&mut f, token.as_bytes()).map_err(io)
    }

    fn load_token(&self, attempt_id: &str) -> Option<String> {
        std::fs::read_to_string(self.token_path(attempt_id))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    fn forget_token(&self, attempt_id: &str) {
        let _ = std::fs::remove_file(self.token_path(attempt_id));
    }

    fn end(
        &mut self,
        attempt: &Attempt,
        token: &str,
        status: Option<AttemptStatus>,
        end: AttemptEnd,
        spent: Spent,
    ) -> Result<()> {
        self.store.end_session(&attempt.id, token, SessionEnd { status, end, spent: Some(spent) }, Utc::now())?;
        Ok(())
    }

    /// What the worker should know beyond the brief: how the last attempt
    /// ended, and whether this worktree holds exported work in progress.
    fn worker_context(&self, task: &Task) -> Result<String> {
        let mut out = String::new();
        let attempts = self.store.attempts(&task.id)?;
        if let Some(last) = attempts.iter().rev().find(|a| a.role == Role::Worker)
            && let Some(end) = &last.end
            && !matches!(end.reason, EndReason::Completed | EndReason::Rejected)
        {
            let detail = end.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default();
            out.push_str(&format!(
                "\n## The previous attempt\n\nAttempt {} ended without a result ({}{detail}). Its changes are not in \
                 this worktree.\n",
                last.id, end.reason
            ));
        }
        if let Some(tree) = &task.current_tree {
            let accepted = self.store.results(&task.id)?;
            if !accepted.iter().any(|r| r.status == ResultStatus::Accepted && &r.output_tree == tree) {
                out.push_str(&format!(
                    "\n## Resuming\n\nThis worktree starts from exported work in progress (tree {tree}), not from an \
                     accepted result. Read what is there before you continue.\n"
                ));
            }
        }
        Ok(out)
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
        let base_tree = git::tree_of(&self.repo, &base)?;
        let context = self.worker_context(&task)?;
        let wt = self.dir().join("worktrees").join(format!("{task_id}-worker-{}", task.attempts_used + 1));
        let (attempt, token) = match self.start(task_id, Role::Worker, &wt)? {
            Ok(v) => v,
            Err(moved) => return Ok(blocked_report(Role::Worker, moved)),
        };
        self.save_token(&attempt.id, &token)?;
        fresh_worktree(&self.repo, &wt, &start_commit)?;
        let brief = self.store.brief(task_id, Role::Worker, self.cfg.profile, &HostPolicy::open(), Utc::now())?;
        let env = self.env(&attempt.id, &token, None);
        let (timeout, budget_deadline, cost_left) = self.limits(&task)?;
        let prompt = format!("{}{context}", prompts::worker(&brief.to_markdown()));
        let spec = self.spec(&wt, prompt, prompts::WORKER_SYSTEM, &attempt, env, plugin, timeout, cost_left);
        let outcome = self.launch(task_id, &attempt, &token, &spec, budget_deadline);
        self.finish_worker(task_id, &attempt, &token, &wt, &base_tree, outcome, budget_deadline, spec.session_id)
    }

    /// Everything after a worker's session ends: submit its output, or
    /// record why it ended and retry (R3), stop at a spent budget, and clean up.
    #[allow(clippy::too_many_arguments)]
    fn finish_worker(
        &mut self,
        task_id: &str,
        attempt: &Attempt,
        token: &str,
        wt: &Path,
        base_tree: &str,
        outcome: SessionOutcome,
        budget_deadline: bool,
        host_session_id: Option<String>,
    ) -> Result<SessionReport> {
        let (reason, detail) = ended_because(&outcome, budget_deadline);
        let spent = spent_of(&outcome);
        let mut moves = Vec::new();
        let mut exported = None;
        let tree = if outcome.exit == Exit::Completed { Some(git::worktree_tree(wt)) } else { None };
        match tree {
            Some(Ok(tree)) => {
                // Scope is judged on everything the output changes relative to the input.
                let changed = git::changed_paths(&self.repo, base_tree, &tree)?;
                let applied = self.store.submit_result(
                    SubmitResult {
                        attempt_id: attempt.id.clone(),
                        token: token.to_string(),
                        epoch: attempt.epoch,
                        output_tree: tree,
                        changed_paths: changed,
                        summary: outcome.summary.final_text.clone().unwrap_or_else(|| "(no summary)".into()),
                        open_questions: vec![],
                        event_id: Some(format!("{}:result", attempt.id)),
                    },
                    Utc::now(),
                )?;
                moves.extend(applied.outcome.moved.clone());
                let why = applied.outcome.result.superseded_reason.clone().unwrap_or_default();
                match applied.outcome.result.status {
                    ResultStatus::Accepted => {
                        self.end(attempt, token, None, done(EndReason::Completed, None), spent)?
                    }
                    ResultStatus::Rejected => {
                        let note = format!("result rejected: {why}");
                        let end = done(EndReason::Rejected, Some(note.clone()));
                        self.end(attempt, token, Some(AttemptStatus::Failed), end, spent)?;
                        if self.store.task(task_id)?.state == State::Running {
                            moves.push(self.store.retry(task_id, &note, Utc::now())?);
                        }
                    }
                    ResultStatus::Superseded => {
                        let end = done(EndReason::Completed, Some(format!("result superseded: {why}")));
                        self.end(attempt, token, Some(AttemptStatus::Completed), end, spent)?;
                    }
                }
            }
            tree => {
                let (reason, detail) = match tree {
                    Some(Err(e)) => (EndReason::Crash, Some(format!("the worktree could not be read: {e}"))),
                    _ => (reason, detail.clone()),
                };
                let status =
                    if reason == EndReason::Cancelled { AttemptStatus::Cancelled } else { AttemptStatus::Failed };
                self.end(attempt, token, Some(status), done(reason, detail.clone()), spent)?;
                if reason == EndReason::Cancelled {
                    exported = export::salvage(
                        &self.store,
                        &self.repo,
                        &self.dir(),
                        attempt,
                        wt,
                        self.cfg.profile,
                        Utc::now(),
                    )
                    .ok()
                    .flatten();
                }
                let budget = self.stop_at_budget(task_id, reason)?;
                let has_budget_move = budget.is_some();
                moves.extend(budget);
                if !has_budget_move && self.store.task(task_id)?.state == State::Running {
                    let why = detail.map(|d| format!(": {d}")).unwrap_or_default();
                    moves.push(self.store.retry(task_id, &format!("session ended ({reason}{why})"), Utc::now())?);
                }
            }
        }
        moves.extend(self.store.enforce_budget(task_id, Utc::now())?);
        self.forget_token(&attempt.id);
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, wt);
        }
        let (reason, detail) = self.recorded_end(&attempt.id, reason, detail);
        Ok(session_report(Role::Worker, attempt, outcome, moves, reason, detail, host_session_id, exported))
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
        self.save_token(&attempt.id, &token)?;
        fresh_worktree(&self.repo, &wt, &commit)?;
        let brief = self.store.brief(task_id, Role::Verifier, self.cfg.profile, &HostPolicy::open(), Utc::now())?;
        let env = self.env(&attempt.id, &token, Some(&tree));
        let (timeout, budget_deadline, cost_left) = self.limits(&task)?;
        let prompt = prompts::verifier(&brief.to_markdown());
        let spec = self.spec(&wt, prompt, prompts::VERIFIER_SYSTEM, &attempt, env, plugin, timeout, cost_left);
        let outcome = self.launch(task_id, &attempt, &token, &spec, budget_deadline);
        self.finish_verifier(task_id, &attempt, &token, &wt, outcome, budget_deadline, spec.session_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_verifier(
        &mut self,
        task_id: &str,
        attempt: &Attempt,
        token: &str,
        wt: &Path,
        outcome: SessionOutcome,
        budget_deadline: bool,
        host_session_id: Option<String>,
    ) -> Result<SessionReport> {
        let (reason, detail) = ended_because(&outcome, budget_deadline);
        let status = match reason {
            EndReason::Completed => AttemptStatus::Completed,
            EndReason::Cancelled => AttemptStatus::Cancelled,
            _ => AttemptStatus::Failed,
        };
        self.end(attempt, token, Some(status), done(reason, detail.clone()), spent_of(&outcome))?;
        let mut moves: Vec<Move> = self.stop_at_budget(task_id, reason)?.into_iter().collect();
        if moves.is_empty() && !self.store.task(task_id)?.state.is_terminal() {
            moves = self.store.advance(task_id, Utc::now())?;
        }
        self.forget_token(&attempt.id);
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, wt);
        }
        let (reason, detail) = self.recorded_end(&attempt.id, reason, detail);
        Ok(session_report(Role::Verifier, attempt, outcome, moves, reason, detail, host_session_id, None))
    }

    /// Fails the task when its budget is spent. A session the budget itself
    /// stopped always fails the task, whatever the totals say.
    fn stop_at_budget(&mut self, task_id: &str, reason: EndReason) -> Result<Option<Move>> {
        if let Some(mv) = self.store.enforce_budget(task_id, Utc::now())? {
            return Ok(Some(mv));
        }
        if reason == EndReason::BudgetExhausted && !self.store.task(task_id)?.state.is_terminal() {
            let why = "budget exhausted: the session was stopped at the task's budget";
            return Ok(Some(self.store.stop(task_id, false, why, Utc::now())?));
        }
        Ok(None)
    }

    /// The end the store kept for an attempt: the first one recorded stands.
    fn recorded_end(&self, attempt_id: &str, reason: EndReason, detail: Option<String>) -> (EndReason, Option<String>) {
        match self.store.attempt(attempt_id).ok().and_then(|a| a.end) {
            Some(end) => (end.reason, end.detail),
            None => (reason, detail),
        }
    }
}

fn done(reason: EndReason, detail: Option<String>) -> AttemptEnd {
    AttemptEnd { reason, detail, synthetic: false }
}

/// The session's end reason; a timeout set by the task's wall-clock budget
/// counts as the budget running out.
fn ended_because(outcome: &SessionOutcome, budget_deadline: bool) -> (EndReason, Option<String>) {
    match interlock_adapter::classify(outcome) {
        (EndReason::Timeout, _) if budget_deadline => {
            (EndReason::BudgetExhausted, Some("the task's wall-clock budget ran out during the session".into()))
        }
        other => other,
    }
}

fn spent_of(outcome: &SessionOutcome) -> Spent {
    Spent {
        wall_ms: outcome.duration_ms,
        cost_usd: outcome.summary.cost_usd,
        premium_requests: outcome.summary.premium_requests,
        turns: outcome.summary.turns,
    }
}

fn target_of(h: &Handoff) -> Target {
    Target {
        pid: h.pid,
        pgid: h.pgid,
        process_start: h.process_start,
        started_at: h.started_at.into(),
        deadline: h.deadline.into(),
        transcript: PathBuf::from(&h.transcript),
    }
}

/// What a session that died unseen used: its transcript's last write bounds
/// its wall-clock time, and a host that finished reported its cost there.
fn salvage_spent(h: &Handoff) -> Spent {
    let path = Path::new(&h.transcript);
    let last_write =
        std::fs::metadata(path).and_then(|m| m.modified()).map(DateTime::<Utc>::from).unwrap_or(h.started_at);
    let lines: Vec<String> = std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect();
    let summary = interlock_adapter::host(&h.host).map(|host| host.summarize(&lines)).unwrap_or_default();
    Spent {
        wall_ms: (last_write - h.started_at).num_milliseconds().max(0) as u64,
        cost_usd: summary.cost_usd,
        premium_requests: summary.premium_requests,
        turns: summary.turns,
    }
}

/// Replaces anything a crashed run left at `path` with a clean worktree.
fn fresh_worktree(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    let _ = git::worktree_remove(repo, path);
    let _ = std::fs::remove_dir_all(path);
    git::worktree_add(repo, path, commit)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn session_report(
    role: Role,
    attempt: &interlock_schema::Attempt,
    outcome: SessionOutcome,
    moves: Vec<Move>,
    reason: EndReason,
    detail: Option<String>,
    host_session_id: Option<String>,
    export: Option<Export>,
) -> SessionReport {
    SessionReport {
        role,
        attempt_id: attempt.id.clone(),
        epoch: attempt.epoch,
        spent: spent_of(&outcome),
        exit: outcome.exit,
        duration_ms: outcome.duration_ms,
        host_session_id: host_session_id.or(outcome.summary.session_id),
        summary: outcome.summary.final_text,
        denials: outcome.summary.denials,
        transcript: outcome.transcript,
        moves,
        ended_because: Some(reason),
        end_detail: detail,
        reattached: false,
        export,
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
        ended_because: None,
        end_detail: None,
        spent: Spent::default(),
        host_session_id: None,
        reattached: false,
        export: None,
    }
}
