//! Sessions outside the run loop: where attempt tokens are kept, how a
//! recorded session is stopped, and how sessions whose supervisor is gone are
//! ended. `interlock run` uses these when it starts; `interlock task cancel`,
//! `retry` and `fail` use them when no supervisor is watching.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use chrono::{DateTime, Utc};
use interlock_adapter::Target;
use interlock_schema::{Attempt, AttemptStatus, EndReason, Handoff, Spent};
use interlock_store::Store;

use crate::run::{Result, RunError};

/// Where a checkout's attempt tokens are kept: a per-user directory outside
/// the repository, `$XDG_RUNTIME_DIR/interlock/<hash>` or
/// `/tmp/interlock-<uid>/<hash>`, where the hash names the store directory.
pub fn token_dir(store_dir: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(store_dir).unwrap_or_else(|_| store_dir.to_path_buf());
    let hash = interlock_core::digest::sha256_hex(canonical.display().to_string().as_bytes());
    let base = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()) {
        Some(d) => d.join("interlock"),
        None => std::env::temp_dir().join(format!("interlock-{}", nix::unistd::getuid())),
    };
    base.join(&hash[..16])
}

/// Creates `dir` and its parent readable only by this user.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for d in [dir.parent(), Some(dir)].into_iter().flatten() {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// The token files of one checkout. Tokens are bookkeeping, not security:
/// any process running as the same user can read them, and can read a running
/// session's token from its environment.
pub struct Tokens {
    dir: PathBuf,
}

impl Tokens {
    pub fn new(store_dir: &Path) -> Tokens {
        Tokens { dir: token_dir(store_dir) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, attempt_id: &str) -> PathBuf {
        self.dir.join(format!("{attempt_id}.token"))
    }

    pub fn save(&self, attempt_id: &str, token: &str) -> Result<()> {
        let path = self.path(attempt_id);
        let io = |e: std::io::Error| RunError::Other(format!("cannot write {}: {e}", path.display()));
        private_dir(&self.dir).map_err(io)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&path).map_err(io)?;
        std::io::Write::write_all(&mut f, token.as_bytes()).map_err(io)
    }

    pub fn load(&self, attempt_id: &str) -> Option<String> {
        std::fs::read_to_string(self.path(attempt_id)).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }

    pub fn forget(&self, attempt_id: &str) {
        let _ = std::fs::remove_file(self.path(attempt_id));
    }
}

pub fn target_of(h: &Handoff) -> Target {
    Target {
        pid: h.pid,
        pgid: h.pgid,
        process_start: h.process_start,
        started_at: h.started_at.into(),
        deadline: h.deadline.into(),
        transcript: PathBuf::from(&h.transcript),
    }
}

/// Whether the supervisor that started a session still runs (and is not this process).
pub fn owned_by_live_supervisor(h: &Handoff) -> bool {
    h.supervisor_pid != std::process::id() && interlock_adapter::alive(h.supervisor_pid, h.supervisor_start)
}

/// Stops a recorded session: its process group if the host still runs, then
/// anything left carrying its marker or in its group. Returns whether the host
/// was still running.
pub fn stop(attempt_id: &str, h: &Handoff) -> bool {
    let live = interlock_adapter::alive(h.pid, h.process_start);
    if live {
        interlock_adapter::attach(&target_of(h), &AtomicBool::new(true), |_| Default::default());
    }
    interlock_adapter::contain(attempt_id, Some(h.pgid), None);
    live
}

/// What a session that ended unseen used: its transcript's last write bounds
/// its wall-clock time, and a host that finished reported its cost there.
pub fn salvage_spent(h: &Handoff) -> Spent {
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

/// Why a session nobody will finish ended, from its attempt's status and
/// whether its host was still running when it was found.
pub fn unattended_end(attempt: &Attempt, h: &Handoff, was_live: bool, context: &str) -> (EndReason, String) {
    match attempt.status {
        AttemptStatus::Running if was_live => {
            (EndReason::Crash, format!("its supervisor was gone; {context} stopped the session (pid {})", h.pid))
        }
        AttemptStatus::Running => {
            (EndReason::Crash, format!("the session's process (pid {}) was gone when {context} found it", h.pid))
        }
        AttemptStatus::Submitted => {
            (EndReason::Completed, format!("its result was accepted; {context} recorded the end"))
        }
        status => {
            let status =
                serde_json::to_value(status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            let what = if was_live { "stopped the session" } else { "found the session already gone" };
            (EndReason::Cancelled, format!("the attempt was {status} while no supervisor ran; {context} {what}"))
        }
    }
}

/// Ends every session of `task_id` (or of every task) that has no recorded
/// end and whose supervisor is gone: stops what still runs, records why it
/// ended, and drops its token. For `interlock task cancel`, `retry` and
/// `fail`, which change the task while no supervisor may be watching.
pub fn end_unattended(
    store: &mut Store,
    store_dir: &Path,
    task_id: Option<&str>,
    context: &str,
) -> Result<Vec<String>> {
    let tokens = Tokens::new(store_dir);
    let mut ended = Vec::new();
    for attempt in store.unended_sessions()? {
        if task_id.is_some_and(|t| t != attempt.task_id) {
            continue;
        }
        let Some(h) = attempt.handoff.clone() else { continue };
        if owned_by_live_supervisor(&h) || attempt.status == AttemptStatus::Running {
            // A live supervisor finishes its own sessions; a running attempt is
            // the run loop's to re-attach.
            continue;
        }
        let was_live = stop(&attempt.id, &h);
        let (reason, detail) = unattended_end(&attempt, &h, was_live, context);
        store.reconcile_attempt(&attempt.id, reason, &detail, Some(salvage_spent(&h)), Utc::now())?;
        tokens.forget(&attempt.id);
        ended.push(attempt.id);
    }
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff(supervisor_pid: u32, supervisor_start: Option<u64>) -> Handoff {
        Handoff {
            host: "claude-code".into(),
            pid: 0,
            pgid: 0,
            process_start: None,
            started_at: Utc::now(),
            deadline: Utc::now(),
            budget_deadline: false,
            transcript: String::new(),
            host_session_id: None,
            supervisor_pid,
            supervisor_start,
            start_commit: None,
            env: vec![],
            effort: None,
            reattached_at: vec![],
        }
    }

    /// A restarted supervisor, or `task cancel`, must not take over a session
    /// whose own supervisor still runs; a reused pid is not that supervisor.
    #[test]
    fn a_session_whose_supervisor_still_runs_is_left_to_it() {
        let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = other.id();
        let start = interlock_adapter::process_start(pid);
        assert!(start.is_some());
        assert!(owned_by_live_supervisor(&handoff(pid, start)), "a live supervisor keeps its session");
        assert!(!owned_by_live_supervisor(&handoff(pid, start.map(|s| s + 1))), "same pid, another process");
        assert!(!owned_by_live_supervisor(&handoff(std::process::id(), None)), "this process is not another one");
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(!owned_by_live_supervisor(&handoff(pid, start)), "a dead supervisor owns nothing");
    }
}
