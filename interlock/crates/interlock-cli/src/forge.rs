//! `interlock integrate run`, `interlock reconcile`, and the reconcile that
//! starts every `interlock run`. All of them hold the controller lock, so
//! only one process talks to the forge for a checkout at a time, and none of
//! them runs inside an attempt: they are the operator's.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, anyhow, bail};
use clap::Args;
use interlock_forge::deliver::parse_duration;
use interlock_forge::{DeliverConfig, DeliverError, GhForge, MergeMethod, inside_attempt};
use interlock_schema::State;
use interlock_store::Store;
use serde_json::json;

#[derive(Args)]
pub struct IntegrateRun {
    pub task: String,
    /// The git remote that is the GitHub repository.
    #[arg(long, env = "INTERLOCK_FORGE_REMOTE", default_value = "origin")]
    pub remote: String,
    /// The branch to merge into. Defaults to the remote's default branch.
    #[arg(long, env = "INTERLOCK_FORGE_BASE")]
    pub base: Option<String>,
    /// OWNER/REPO for gh, when the remote does not identify it.
    #[arg(long, env = "INTERLOCK_FORGE_REPO")]
    pub repo: Option<String>,
    /// merge, squash or rebase.
    #[arg(long, env = "INTERLOCK_FORGE_METHOD", default_value = "merge")]
    pub method: String,
    /// Arm auto-merge, pinned to the verified head, instead of waiting for checks.
    #[arg(long, env = "INTERLOCK_FORGE_AUTO_MERGE")]
    pub auto_merge: bool,
    /// How long to wait for checks and mergeability, for example 15m; 0 looks once.
    #[arg(long, env = "INTERLOCK_FORGE_WAIT", default_value = "15m")]
    pub wait: String,
    #[arg(long, env = "INTERLOCK_FORGE_POLL", default_value = "15s")]
    pub poll: String,
    /// A pull request that reports no checks is not ready until this long after the push.
    #[arg(long, env = "INTERLOCK_FORGE_CHECKS_SETTLE", default_value = "30s")]
    pub checks_settle: String,
    /// How long one gh or git call may take before its outcome counts as unknown.
    #[arg(long, env = "INTERLOCK_FORGE_CALL_TIMEOUT", default_value = "120s")]
    pub call_timeout: String,
}

/// Forge commands are the operator's. An agent inside an attempt may not run
/// them, whatever the hooks let through.
pub fn operator_only(command: &str) -> Result<()> {
    if inside_attempt() {
        bail!("`interlock {command}` is the operator's; it does not run inside an attempt (INTERLOCK_ATTEMPT is set)");
    }
    Ok(())
}

/// One controller per checkout: the same lock file `interlock run` takes.
struct ControllerLock(PathBuf);

impl ControllerLock {
    fn acquire(db: &Path) -> Result<ControllerLock> {
        let dir = db.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("supervisor.lock");
        if let Ok(pid) = std::fs::read_to_string(&path) {
            let pid = pid.trim();
            if !pid.is_empty() && pid != std::process::id().to_string() && Path::new(&format!("/proc/{pid}")).exists() {
                bail!("another controller (pid {pid}) is running; lock at {}", path.display());
            }
        }
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(ControllerLock(path))
    }
}

impl Drop for ControllerLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Store errors keep their own exit codes; everything else is a failure.
fn deliver_err(e: DeliverError) -> anyhow::Error {
    match e {
        DeliverError::Store(s) => anyhow::Error::from(s),
        other => anyhow!(other),
    }
}

fn repo_root() -> Result<PathBuf> {
    Ok(interlock_supervisor::git::toplevel(&std::env::current_dir()?)?)
}

/// G5, push, pull request, readiness, the pinned merge, and G6. Exits 0 when
/// the task is done and 5 otherwise, like `interlock run`.
pub fn integrate_run(db: &Path, args: &IntegrateRun) -> Result<ExitCode> {
    operator_only("integrate run")?;
    let repo = repo_root()?;
    let duration = |s: &str| parse_duration(s).ok_or_else(|| anyhow!("bad duration {s}; use forms like 90s or 15m"));
    let cfg = DeliverConfig {
        base: args.base.clone(),
        method: MergeMethod::parse(&args.method).ok_or_else(|| anyhow!("--method takes merge, squash or rebase"))?,
        auto_merge: args.auto_merge,
        wait: duration(&args.wait)?,
        poll: duration(&args.poll)?,
        checks_settle: duration(&args.checks_settle)?,
    };
    let mut forge = GhForge::from_env(&repo);
    forge.remote = args.remote.clone();
    forge.repo = args.repo.clone().or(forge.repo);
    forge.timeout = duration(&args.call_timeout)?;
    let _lock = ControllerLock::acquire(db)?;
    let mut store = Store::open(db)?;
    let delivery = interlock_forge::integrate(&mut store, &repo, &forge, &cfg, &args.task, &AtomicBool::new(false))
        .map_err(deliver_err)?;
    crate::print(&delivery)?;
    Ok(if delivery.final_state == State::Done { ExitCode::SUCCESS } else { ExitCode::from(5) })
}

/// Settles every operation nobody confirmed, for one task or all of them.
pub fn reconcile(db: &Path, task: Option<&str>) -> Result<()> {
    operator_only("reconcile")?;
    let repo = repo_root()?;
    let _lock = ControllerLock::acquire(db)?;
    let mut store = Store::open(db)?;
    let forge = GhForge::from_env(&repo);
    let reconciled = interlock_forge::reconcile(&mut store, &repo, &forge, task).map_err(deliver_err)?;
    let unknown =
        store.open_operations(task)?.iter().filter(|o| o.state == interlock_schema::OperationState::Unknown).count();
    crate::print(&json!({ "reconciled": reconciled, "still_unknown": unknown }))
}

/// The reconcile that starts every `interlock run`: whatever a previous
/// controller left open is settled from the forge before anything else.
/// Returns the operations it settled.
pub fn reconcile_on_start(db: &Path, repo: &Path) -> Result<Vec<String>> {
    operator_only("run")?;
    let _lock = ControllerLock::acquire(db)?;
    let mut store = Store::open(db)?;
    if store.open_operations(None)?.is_empty() && store.pinned_landings(None)?.is_empty() {
        return Ok(vec![]);
    }
    let forge = GhForge::from_env(repo);
    let reconciled = interlock_forge::reconcile(&mut store, repo, &forge, None).map_err(deliver_err)?;
    Ok(reconciled.into_iter().map(|r| r.operation).collect())
}
