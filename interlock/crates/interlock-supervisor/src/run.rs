//! The headless path: start worker and verifier sessions through a host
//! adapter, each in its own git worktree, and let the store's guards decide
//! every move. The supervisor holds tokens but never writes state directly.
//!
//! Sessions outlive the supervisor. Each session's handoff (process, group,
//! deadline, transcript, host session id) is recorded before the host starts,
//! so a restarted supervisor re-attaches to a session that is still running
//! and carries on as if it had never stopped, and ends one that is gone with
//! a synthetic failure report before retrying (R3).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use interlock_adapter::{Exit, Host, HostReport, Probe, SessionOutcome, SessionSpec, write_hooks_plugin};
use interlock_core::budget;
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::evidence;
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::Move;
use interlock_core::workflow::Mode;
use interlock_schema::{
    Attempt, AttemptEnd, AttemptStatus, Baseline, Binding, EndReason, Handoff, HostRef, ResultStatus, Role, RunTarget,
    Snapshot, Spent, State, Task,
};
use interlock_store::{SessionEnd, StartAttempt, Started, Store, SubmitResult};
use serde::Serialize;

use crate::config::{Config, PinStatus};
use crate::export::{self, Export};
use crate::lock::ControllerLock;
use crate::sessions::{self, Tokens};
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
    /// Sessions to run before giving up, across workers and verifiers. Zero
    /// runs the baseline checks and stops.
    pub max_sessions: u32,
    pub profile: Profile,
    /// The interlock binary the hooks and agents call.
    pub interlock_bin: PathBuf,
    /// Declared capabilities, instead of inspecting the host.
    pub capabilities: Option<CapabilitySet>,
    /// The reasoning effort for every session, for hosts that have one.
    pub effort: Option<String>,
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
    /// The session's cost in dollars, where the host reports it.
    pub cost_usd: Option<f64>,
    /// The session's premium requests, where the host reports them.
    pub premium_requests: Option<f64>,
    pub host_session_id: Option<String>,
    /// A restarted supervisor followed this session to its end.
    pub reattached: bool,
    /// Processes the session left behind that interlock stopped when it ended.
    pub stopped_strays: Vec<u32>,
    /// Where a stopped worker's unfinished work was exported.
    pub export: Option<Export>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub task_id: String,
    pub host: String,
    pub final_state: State,
    pub stopped_because: String,
    /// Attempts, of any task, whose sessions a previous supervisor left
    /// without a recorded end and which this run ended.
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

/// What opening an attempt came to.
#[allow(clippy::large_enum_variant)] // short-lived return value
enum Opened {
    Yes(Attempt, String),
    Blocked(Move),
    /// The run was interrupted before the attempt was opened.
    Interrupted,
}

pub struct Supervisor {
    pub repo: PathBuf,
    pub db: PathBuf,
    store: Store,
    host: Box<dyn Host>,
    probe: Probe,
    /// The host as inspected once in this run; every check reads this copy.
    inspected: Option<HostReport>,
    cfg: RunConfig,
    config: Config,
    /// This process adopts orphans its sessions leave (Linux).
    subreaper: bool,
    /// Set by a signal handler (or anyone) to stop the run: the running
    /// session is stopped, its attempt cancelled, and the task left to resume.
    pub cancel: Arc<AtomicBool>,
}

impl Supervisor {
    pub fn new(repo: PathBuf, db: PathBuf, host: Box<dyn Host>, probe: Probe, cfg: RunConfig) -> Result<Supervisor> {
        let store = Store::open(&db)?;
        Ok(Supervisor {
            repo,
            db,
            store,
            host,
            probe,
            inspected: None,
            cfg,
            config: Config::default(),
            subreaper: false,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    fn dir(&self) -> PathBuf {
        self.db.parent().map(Path::to_path_buf).unwrap_or_else(|| self.repo.join(".interlock"))
    }

    fn tokens(&self) -> Tokens {
        Tokens::new(&self.dir())
    }

    fn interrupted(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Inspects the host once per run: its version and flags do not change
    /// under a running supervisor, and each inspection runs the binary twice.
    fn inspect(&mut self) -> HostReport {
        let (host, probe) = (&self.host, &self.probe);
        self.inspected.get_or_insert_with(|| host.inspect(probe)).clone()
    }

    fn capabilities(&mut self) -> Result<(CapabilitySet, String)> {
        if let Some(c) = &self.cfg.capabilities {
            return Ok((c.clone(), "declared".into()));
        }
        let report: HostReport = self.inspect();
        if !report.installed {
            return Err(RunError::Other(format!("{} is not installed: {}", report.host, report.notes.join("; "))));
        }
        let value = serde_json::to_value(&report).map_err(|e| RunError::Other(e.to_string()))?;
        self.store.save_host_report(self.host.name(), &value, Utc::now())?;
        Ok((report.capability_set(), report.version.unwrap_or_default()))
    }

    /// Refuses an effort level the host cannot take, before anything starts.
    fn check_effort(&mut self) -> Result<()> {
        let Some(level) = self.cfg.effort.clone() else { return Ok(()) };
        let has_flag = match &self.cfg.capabilities {
            Some(c) => c.has(Capability::EffortSelection),
            None => self.inspect().capability_set().has(Capability::EffortSelection),
        };
        let name = self.host.name();
        if !has_flag || self.host.effort_levels().is_empty() {
            return Err(RunError::Other(format!(
                "{name} has no effort setting (its --help shows no effort flag), so --effort cannot be honored"
            )));
        }
        if !self.host.effort_levels().contains(&level.as_str()) {
            return Err(RunError::Other(format!(
                "{name} takes --effort {}, not {level}",
                self.host.effort_levels().join(", ")
            )));
        }
        Ok(())
    }

    /// Drives a task until it is done, blocked, failed, needs the operator,
    /// the session budget runs out, or the run is interrupted.
    pub fn run(&mut self, task_id: &str) -> Result<RunReport> {
        self.check_effort()?;
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| RunError::Other(e.to_string()))?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
        let _lock = ControllerLock::acquire(&dir)?;
        self.subreaper = interlock_adapter::become_subreaper();
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
        // Read only now, so a bad config never stands in the way of recovery.
        self.config = Config::load(&dir).map_err(RunError::Other)?;

        if let Some(pinned) = self.config.pin(self.host.name()).map(str::to_string) {
            let installed = self.inspect().version;
            let pin = PinStatus::of(Some(&pinned), installed.as_deref());
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
            if self.interrupted() {
                report.interrupted = true;
                let state = self.store.task(task_id)?.state;
                break format!(
                    "interrupted by a signal; any running session was stopped and the task can resume from {state}"
                );
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
                // The evidence may already decide, as after a moved-head R2: then no verifier session is needed.
                State::AwaitingVerification if !self.store.advance(task_id, Utc::now())?.is_empty() => {}
                State::Ready if !baseline_checked => {
                    let Some(runs) = self.baseline(task_id)? else { continue };
                    baseline_checked = true;
                    report.baseline = runs;
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
                State::Ready | State::AwaitingVerification if self.cfg.max_sessions == 0 => {
                    break "baseline only: --max-sessions 0 starts no session".to_string();
                }
                State::Ready | State::AwaitingVerification if sessions >= self.cfg.max_sessions => {
                    break format!("used all {} sessions this run allows", self.cfg.max_sessions);
                }
                State::Ready => {
                    sessions += 1;
                    if let Some(s) = self.worker_session(task_id, &plugin)? {
                        report.sessions.push(s);
                    }
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
                    if let Some(s) = self.verifier_session(task_id, &plugin)? {
                        report.sessions.push(s);
                    }
                }
                State::Running => {
                    self.store.retry(task_id, "found running without a live session", Utc::now())?;
                }
                // G7 when no delivery is needed; otherwise G5, the pinned merge and G6 through the forge.
                State::Verified | State::Integrating => {
                    if let Some(stop) = crate::delivery::step(&mut self.store, &self.repo, task_id, &self.cancel)? {
                        break stop;
                    }
                }
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

    /// Accounts for every session a previous supervisor left without a
    /// recorded end, in any task: a running session of this task is followed
    /// to its end; every other one is stopped if it still runs, its end is
    /// recorded with a synthetic report, its token dropped, and a worker's
    /// unfinished work salvaged. Then R3 for any task left running.
    fn restart_reconcile(&mut self, task_id: &str, report: &mut RunReport) -> Result<()> {
        let tokens = self.tokens();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for attempt in self.store.unended_sessions()? {
            let Some(h) = attempt.handoff.clone() else { continue };
            if sessions::owned_by_live_supervisor(&h) {
                if attempt.task_id == task_id && attempt.status == AttemptStatus::Running {
                    return Err(RunError::Other(format!(
                        "attempt {}'s session belongs to a supervisor that is still running (pid {}); \
                         not starting another",
                        attempt.id, h.supervisor_pid
                    )));
                }
                continue;
            }
            let live = interlock_adapter::alive(h.pid, h.process_start);
            if live
                && attempt.task_id == task_id
                && attempt.status == AttemptStatus::Running
                && let Some(token) = tokens.load(&attempt.id)
            {
                let s = self.reattach(task_id, &attempt, &token, &h)?;
                report.reattached.push(attempt.id.clone());
                report.sessions.push(s);
                continue;
            }
            let was_live = sessions::stop(&attempt.id, &h);
            let (reason, detail) = sessions::unattended_end(&attempt, &h, was_live, "a restarted supervisor");
            self.store.reconcile_attempt(
                &attempt.id,
                reason,
                &detail,
                Some(sessions::salvage_spent(&h)),
                Utc::now(),
            )?;
            tokens.forget(&attempt.id);
            report.reconciled.push(attempt.id.clone());
            touched.insert(attempt.task_id.clone());
            self.salvage_worktree(&attempt, h.start_commit.as_deref(), &mut report.exports);
        }
        // An attempt of this task opened just before the old supervisor died,
        // with no session started yet.
        for attempt in self.store.attempts(task_id)? {
            if attempt.status == AttemptStatus::Running && attempt.handoff.is_none() {
                let detail = "the supervisor stopped before the session started";
                self.store.reconcile_attempt(&attempt.id, EndReason::Crash, detail, None, Utc::now())?;
                tokens.forget(&attempt.id);
                report.reconciled.push(attempt.id.clone());
                touched.insert(task_id.to_string());
                if let Some(wt) = attempt.worktree.as_deref().filter(|_| !self.cfg.keep_worktrees) {
                    let _ = git::worktree_remove(&self.repo, Path::new(wt));
                }
            }
        }
        touched.insert(task_id.to_string());
        for t in touched {
            let running = self.store.attempts(&t)?.iter().any(|a| a.status == AttemptStatus::Running);
            if !running && self.store.task(&t)?.state == State::Running {
                self.store.retry(&t, "the worker's session was gone after a supervisor restart", Utc::now())?;
            }
        }
        Ok(())
    }

    /// Exports a reconciled worker's unfinished work, then removes its worktree.
    fn salvage_worktree(&self, attempt: &Attempt, start_commit: Option<&str>, exports: &mut Vec<Export>) {
        let Some(wt) = attempt.worktree.as_deref().map(PathBuf::from) else { return };
        if attempt.role == Role::Worker
            && let Some(start) = start_commit
            && let Ok(Some(e)) =
                export::salvage(&self.store, &self.repo, &self.dir(), attempt, &wt, start, self.cfg.profile, Utc::now())
        {
            exports.push(e);
        }
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, &wt);
        }
    }

    /// Follows a session a previous supervisor started, then finishes it
    /// exactly as that supervisor would have.
    fn reattach(&mut self, task_id: &str, attempt: &Attempt, token: &str, h: &Handoff) -> Result<SessionReport> {
        self.store.note_reattach(&attempt.id, token, Utc::now())?;
        let host =
            interlock_adapter::host(&h.host).ok_or_else(|| RunError::Other(format!("unknown host {}", h.host)))?;
        let watch = self.watch(task_id, &attempt.id);
        let outcome = interlock_adapter::attach(&sessions::target_of(h), &watch.stop, |l| host.summarize(l));
        drop(watch);
        // Orphans of a supervisor that died went to init, not here; its marker still finds them.
        let strays = interlock_adapter::contain(&attempt.id, Some(h.pgid), None);
        let wt = PathBuf::from(attempt.worktree.clone().unwrap_or_default());
        let ended =
            Ended { outcome, budget_deadline: h.budget_deadline, host_session_id: h.host_session_id.clone(), strays };
        let mut s = match attempt.role {
            Role::Worker => {
                let start = match &h.start_commit {
                    Some(c) => c.clone(),
                    None => self.store.task(task_id)?.input_snapshot.map(|s| s.base_commit).unwrap_or_default(),
                };
                self.finish_worker(task_id, attempt, token, &wt, &start, ended)?
            }
            Role::Verifier | Role::Reviewer => self.finish_verifier(task_id, attempt, token, &wt, ended)?,
        };
        s.reattached = true;
        Ok(s)
    }

    /// Runs each baseline check on the input snapshot, once per task, before
    /// any worker spends effort: a reproduction that already passes, or a
    /// regression guard that fails or tests nothing, makes the task unsound.
    /// `None` when the run was interrupted; nothing partial is kept.
    fn baseline(&mut self, task_id: &str) -> Result<Option<Vec<BaselineRun>>> {
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
                None => {
                    let cancel = self.cancel.clone();
                    let ran = checks::run_check_cancellable(
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
                        &cancel,
                    )?;
                    match ran {
                        Some(r) => r,
                        None => return Ok(None),
                    }
                }
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
        Ok(Some(out))
    }

    fn start(&mut self, task_id: &str, role: Role, worktree: &Path) -> Result<Opened> {
        let (capabilities, version) = self.capabilities()?;
        // Inspecting the host takes time; a signal in the meantime opens nothing.
        if self.interrupted() {
            return Ok(Opened::Interrupted);
        }
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
            Started::Yes { attempt, token, .. } => {
                // interlock launches this session itself, so it knows who holds the attempt.
                let attempt = self.store.bind_attempt(&attempt.id, Binding::interlock_launched())?;
                Opened::Yes(attempt, token)
            }
            Started::Blocked { moved } => Opened::Blocked(moved),
        })
    }

    /// One independent verifier session for a task awaiting verification,
    /// launched by interlock: for guided sessions on hosts that cannot name a
    /// verifier subagent to the hooks. Applies what the evidence then allows.
    pub fn verify(&mut self, task_id: &str) -> Result<RunReport> {
        self.check_effort()?;
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| RunError::Other(e.to_string()))?;
        let _lock = ControllerLock::acquire(&dir)?;
        self.subreaper = interlock_adapter::become_subreaper();
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
        self.config = Config::load(&dir).map_err(RunError::Other)?;
        let task = self.store.task(task_id)?;
        if task.state != State::AwaitingVerification {
            report.final_state = task.state;
            report.stopped_because = format!("the task is {}, not awaiting verification", task.state);
            return Ok(report);
        }
        if let Some(session) = self.verifier_session(task_id, &plugin)? {
            report.sessions.push(session);
        }
        self.store.advance(task_id, Utc::now())?;
        report.final_state = self.store.task(task_id)?.state;
        report.stopped_because = "the verifier session ended".into();
        Ok(report)
    }

    /// The session's environment: the allowlist (see `interlock_adapter::env`),
    /// plus interlock's own variables for this attempt and the session marker.
    fn env(&self, attempt_id: &str, token: &str, tree: Option<&str>) -> Vec<(String, String)> {
        let mut env =
            interlock_adapter::env::session_env(self.host.as_ref(), std::env::vars_os(), &self.config.env.pass);
        let parent_path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_default();
        env.retain(|(k, _)| k != "PATH");
        let bin_dir = self.cfg.interlock_bin.parent().map(|p| p.display().to_string()).unwrap_or_default();
        env.extend([
            ("PATH".to_string(), format!("{bin_dir}:{parent_path}")),
            ("INTERLOCK_DB".to_string(), self.db.display().to_string()),
            ("INTERLOCK_ATTEMPT".to_string(), attempt_id.to_string()),
            ("INTERLOCK_TOKEN".to_string(), token.to_string()),
            ("INTERLOCK_MODE".to_string(), "headless".to_string()),
            ("INTERLOCK_HOST".to_string(), self.host.name().to_string()),
            (interlock_adapter::SESSION_MARKER.to_string(), attempt_id.to_string()),
        ]);
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
            effort: self.cfg.effort.clone(),
            clear_env: true,
        }
    }

    /// Starts a session in three steps: spawn the session's process group
    /// held at its gate, record the handoff in its own transaction, and only
    /// then let the host start. When it ends, anything it left behind is stopped.
    fn launch(
        &mut self,
        task_id: &str,
        attempt: &Attempt,
        token: &str,
        spec: &SessionSpec,
        budget_deadline: bool,
        start_commit: &str,
    ) -> Ended {
        let failed = |reason: String| SessionOutcome {
            exit: Exit::Failed { reason },
            exit_code: None,
            signal: None,
            duration_ms: 0,
            summary: Default::default(),
            transcript: spec.transcript.clone(),
        };
        let ended = |outcome: SessionOutcome, strays: Vec<u32>| Ended {
            outcome,
            budget_deadline,
            host_session_id: spec.session_id.clone(),
            strays,
        };
        let plan = match self.host.plan(&self.probe, spec) {
            Ok(p) => p,
            Err(reason) => return ended(failed(format!("could not start the host: {reason}")), vec![]),
        };
        let mut spawned = match interlock_adapter::spawn(&plan, spec) {
            Ok(s) => s,
            Err(outcome) => return ended(*outcome, vec![]),
        };
        let now = Utc::now();
        let deadline = chrono::Duration::from_std(spec.timeout)
            .ok()
            .and_then(|d| now.checked_add_signed(d))
            .unwrap_or_else(|| now + chrono::Duration::days(365));
        let mut names: Vec<String> = plan.env.iter().map(|(k, _)| k.clone()).collect();
        names.sort();
        names.dedup();
        let handoff = Handoff {
            host: self.host.name().into(),
            pid: spawned.pid,
            pgid: spawned.pgid,
            process_start: spawned.process_start,
            started_at: now,
            deadline,
            budget_deadline,
            transcript: spec.transcript.display().to_string(),
            host_session_id: spec.session_id.clone(),
            supervisor_pid: std::process::id(),
            supervisor_start: interlock_adapter::process_start(std::process::id()),
            start_commit: Some(start_commit.to_string()),
            env: names,
            effort: spec.effort.clone(),
            reattached_at: vec![],
        };
        if let Err(e) = self.store.record_handoff(&attempt.id, token, handoff) {
            spawned.abandon();
            return ended(failed(format!("could not start the host: the handoff was not recorded: {e}")), vec![]);
        }
        // A signal while the handoff was written: the host never starts.
        if self.interrupted() {
            spawned.abandon();
            let mut out = failed(String::new());
            out.exit = Exit::Cancelled;
            return ended(out, vec![]);
        }
        if let Err(e) = spawned.release() {
            return ended(failed(format!("could not start the host: {e}")), vec![]);
        }
        let (pgid, leader_start) = (spawned.pgid, spawned.process_start);
        let watch = self.watch(task_id, &attempt.id);
        let host = &self.host;
        let outcome = spawned.wait(spec, &watch.stop, |l| host.summarize(l));
        drop(watch);
        let adopted = if self.subreaper { leader_start.or(Some(0)) } else { None };
        let strays = interlock_adapter::contain(&attempt.id, Some(pgid), adopted);
        ended(outcome, strays)
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
        let resumed = match &task.current_tree {
            Some(tree) => {
                let results = self.store.results(&task.id)?;
                !results.iter().any(|r| r.status == ResultStatus::Accepted && &r.output_tree == tree)
            }
            None => false,
        };
        let attempts = self.store.attempts(&task.id)?;
        if let Some(last) = attempts.iter().rev().find(|a| a.role == Role::Worker)
            && let Some(end) = &last.end
            && !matches!(end.reason, EndReason::Completed | EndReason::Rejected)
        {
            let detail = end.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default();
            let kept = if resumed { "" } else { " Its changes are not in this worktree." };
            out.push_str(&format!(
                "\n## The previous attempt\n\nAttempt {} ended without a result ({}{detail}).{kept}\n",
                last.id, end.reason
            ));
        }
        if resumed {
            out.push_str(&format!(
                "\n## Resuming\n\nThis worktree starts from exported work in progress (tree {}), not from an \
                 accepted result. Read what is there before you continue.\n",
                task.current_tree.as_deref().unwrap_or_default()
            ));
        }
        Ok(out)
    }

    fn worker_session(&mut self, task_id: &str, plugin: &Path) -> Result<Option<SessionReport>> {
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
        let context = self.worker_context(&task)?;
        let wt = self.dir().join("worktrees").join(format!("{task_id}-worker-{}", task.attempts_used + 1));
        let (attempt, token) = match self.start(task_id, Role::Worker, &wt)? {
            Opened::Yes(a, t) => (a, t),
            Opened::Blocked(moved) => return Ok(Some(blocked_report(Role::Worker, moved))),
            Opened::Interrupted => return Ok(None),
        };
        self.tokens().save(&attempt.id, &token)?;
        fresh_worktree(&self.repo, &wt, &start_commit)?;
        let brief = self.store.brief(task_id, Role::Worker, self.cfg.profile, &HostPolicy::open(), Utc::now())?;
        let env = self.env(&attempt.id, &token, None);
        let (timeout, budget_deadline, cost_left) = self.limits(&task)?;
        let prompt = format!("{}{context}", prompts::worker(&brief.to_markdown()));
        let spec = self.spec(&wt, prompt, prompts::WORKER_SYSTEM, &attempt, env, plugin, timeout, cost_left);
        let ended = self.launch(task_id, &attempt, &token, &spec, budget_deadline, &start_commit);
        self.finish_worker(task_id, &attempt, &token, &wt, &start_commit, ended).map(Some)
    }

    /// Everything after a worker's session ends: submit its output, or
    /// record why it ended and retry (R3), stop at a spent budget, and clean up.
    fn finish_worker(
        &mut self,
        task_id: &str,
        attempt: &Attempt,
        token: &str,
        wt: &Path,
        start_commit: &str,
        ended: Ended,
    ) -> Result<SessionReport> {
        let Ended { outcome, budget_deadline, host_session_id, strays } = ended;
        let (reason, detail) = ended_because(&outcome, budget_deadline, self.host.auth_failures());
        let spent = spent_of(&outcome);
        let mut moves = Vec::new();
        let mut exported = None;
        let base_tree = match self.store.task(task_id)?.input_snapshot {
            Some(s) => git::tree_of(&self.repo, &s.base_commit)?,
            None => git::tree_of(&self.repo, start_commit)?,
        };
        let tree = if outcome.exit == Exit::Completed { Some(git::worktree_tree(wt)) } else { None };
        match tree {
            Some(Ok(tree)) => {
                // Scope is judged on everything the output changes relative to the input.
                let changed = git::changed_paths(&self.repo, &base_tree, &tree)?;
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
                        start_commit,
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
        self.tokens().forget(&attempt.id);
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, wt);
        }
        let (reason, detail) = self.recorded_end(&attempt.id, reason, detail);
        let ended = Ended { outcome, budget_deadline, host_session_id, strays };
        Ok(session_report(Role::Worker, attempt, ended, moves, reason, detail, exported))
    }

    fn verifier_session(&mut self, task_id: &str, plugin: &Path) -> Result<Option<SessionReport>> {
        let task = self.store.task(task_id)?;
        let tree = task.current_tree.clone().ok_or_else(|| RunError::Other("no output tree to verify".into()))?;
        let base = task.input_snapshot.as_ref().map(|s| s.base_commit.clone());
        let commit =
            git::commit_tree(&self.repo, &tree, base.as_deref(), &format!("interlock: {task_id} output to verify"))?;
        let n = self.store.attempts(task_id)?.iter().filter(|a| a.role == Role::Verifier).count() + 1;
        let wt = self.dir().join("worktrees").join(format!("{task_id}-verifier-{n}"));
        let (attempt, token) = match self.start(task_id, Role::Verifier, &wt)? {
            Opened::Yes(a, t) => (a, t),
            Opened::Blocked(moved) => return Ok(Some(blocked_report(Role::Verifier, moved))),
            Opened::Interrupted => return Ok(None),
        };
        self.tokens().save(&attempt.id, &token)?;
        fresh_worktree(&self.repo, &wt, &commit)?;
        let brief = self.store.brief(task_id, Role::Verifier, self.cfg.profile, &HostPolicy::open(), Utc::now())?;
        let env = self.env(&attempt.id, &token, Some(&tree));
        let (timeout, budget_deadline, cost_left) = self.limits(&task)?;
        let prompt =
            format!("{}{}", prompts::verifier(&brief.to_markdown()), prompts::verifier_requirements(&task.criteria));
        let spec = self.spec(&wt, prompt, prompts::VERIFIER_SYSTEM, &attempt, env, plugin, timeout, cost_left);
        let ended = self.launch(task_id, &attempt, &token, &spec, budget_deadline, &commit);
        self.finish_verifier(task_id, &attempt, &token, &wt, ended).map(Some)
    }

    fn finish_verifier(
        &mut self,
        task_id: &str,
        attempt: &Attempt,
        token: &str,
        wt: &Path,
        ended: Ended,
    ) -> Result<SessionReport> {
        let (reason, detail) = ended_because(&ended.outcome, ended.budget_deadline, self.host.auth_failures());
        let status = match reason {
            EndReason::Completed => AttemptStatus::Completed,
            EndReason::Cancelled => AttemptStatus::Cancelled,
            _ => AttemptStatus::Failed,
        };
        self.end(attempt, token, Some(status), done(reason, detail.clone()), spent_of(&ended.outcome))?;
        let mut moves: Vec<Move> = self.stop_at_budget(task_id, reason)?.into_iter().collect();
        if moves.is_empty() && !self.store.task(task_id)?.state.is_terminal() {
            moves = self.store.advance(task_id, Utc::now())?;
        }
        self.tokens().forget(&attempt.id);
        if !self.cfg.keep_worktrees {
            let _ = git::worktree_remove(&self.repo, wt);
        }
        let (reason, detail) = self.recorded_end(&attempt.id, reason, detail);
        Ok(session_report(Role::Verifier, attempt, ended, moves, reason, detail, None))
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

/// How a session came to an end, and what it left behind.
struct Ended {
    outcome: SessionOutcome,
    /// The session's deadline came from the task's wall-clock budget.
    budget_deadline: bool,
    host_session_id: Option<String>,
    strays: Vec<u32>,
}

fn done(reason: EndReason, detail: Option<String>) -> AttemptEnd {
    AttemptEnd { reason, detail, synthetic: false }
}

/// The session's end reason; a timeout set by the task's wall-clock budget
/// counts as the budget running out.
fn ended_because(outcome: &SessionOutcome, budget_deadline: bool, auth: &[&str]) -> (EndReason, Option<String>) {
    match interlock_adapter::classify(outcome, auth) {
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
    ended: Ended,
    moves: Vec<Move>,
    reason: EndReason,
    detail: Option<String>,
    export: Option<Export>,
) -> SessionReport {
    let Ended { outcome, host_session_id, strays, .. } = ended;
    let spent = spent_of(&outcome);
    SessionReport {
        role,
        attempt_id: attempt.id.clone(),
        epoch: attempt.epoch,
        cost_usd: spent.cost_usd,
        premium_requests: spent.premium_requests,
        spent,
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
        stopped_strays: strays,
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
        cost_usd: None,
        premium_requests: None,
        host_session_id: None,
        reattached: false,
        stopped_strays: vec![],
        export: None,
    }
}
