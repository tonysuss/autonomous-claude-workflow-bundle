//! Running one headless session. Each host turns a host-neutral
//! `SessionSpec` into a `CommandPlan`; one runner executes any plan with a
//! timeout and cancellation. The host writes its event stream straight to a
//! transcript file, so a session can outlive the supervisor that started it.
//!
//! A session starts in two steps so the supervisor can record its handoff
//! first: `spawn` starts the session's process group with a gate that holds
//! the host back, and `release` lets it run. A supervisor that dies before
//! releasing the gate leaves no host running. `attach` waits for a session
//! that another supervisor started.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use interlock_schema::{EndReason, ToolPolicy};
use serde::{Deserialize, Serialize};

use crate::procinfo;

/// What to run, in host-neutral terms.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// The worktree the agent works in.
    pub workdir: PathBuf,
    pub prompt: String,
    /// Role instructions appended to the host's system prompt, where supported.
    pub append_system: Option<String>,
    /// interlock's agent for the session's role, such as `interlock-worker`.
    /// A host that runs custom agents headless runs the session as this
    /// agent, kept in the hooks plugin, with `append_system` as its
    /// instructions. `None` where the host has no custom agents.
    pub agent: Option<String>,
    /// Host-neutral tool policy; the host translates it.
    pub tools: ToolPolicy,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    /// A Claude-format plugin carrying interlock's hooks. Both hosts load it.
    pub plugin_dir: Option<PathBuf>,
    /// Further plugins loaded alongside it, such as the generated skills.
    pub extra_plugin_dirs: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// Where the raw output stream is written, one event per line.
    pub transcript: PathBuf,
    /// The host's own id for the new session, so it is known before the session starts.
    pub session_id: Option<String>,
    /// A cost cap in US dollars for hosts that enforce one themselves.
    pub max_cost_usd: Option<f64>,
    /// The reasoning effort, for hosts with an effort setting.
    pub effort: Option<String>,
    /// Start from an empty environment, so the session gets exactly `env`
    /// (see [`crate::env`]) rather than inheriting interlock's.
    pub clear_env: bool,
}

/// A concrete command for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub env: Vec<(String, String)>,
}

/// What a host's output stream said about the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub final_text: Option<String>,
    /// The host reported the session as failed.
    pub is_error: bool,
    pub session_id: Option<String>,
    pub turns: Option<u64>,
    pub cost_usd: Option<f64>,
    /// Copilot CLI's `usage.premiumRequests`.
    #[serde(default)]
    pub premium_requests: Option<f64>,
    /// The host's own label for why it stopped, such as Claude Code's `budget_exhausted`.
    #[serde(default)]
    pub stop_reason: Option<String>,
    pub denials: u64,
    pub events: u64,
    /// The model the host reported using, where it says.
    #[serde(default)]
    pub model: Option<String>,
    /// The host reported the session's end: its final `result` event arrived.
    #[serde(default)]
    pub finished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "exit", rename_all = "snake_case")]
pub enum Exit {
    Completed,
    Failed { reason: String },
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOutcome {
    #[serde(flatten)]
    pub exit: Exit,
    pub exit_code: Option<i32>,
    /// The signal that killed the host, when one did and interlock did not send it.
    #[serde(default)]
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub summary: SessionSummary,
    pub transcript: PathBuf,
}

/// The gate: wait for one line, `go`, on stdin, then become the host with
/// its real stdin. End of input without it (the supervisor died) exits 97.
const GATE: &str = r#"IFS= read -r go || exit 97; [ "$go" = go ] || exit 97; f=$1; shift; exec "$@" < "$f""#;
const NEVER_RELEASED: i32 = 97;

/// How long a stopped session gets to exit after SIGTERM before SIGKILL.
const GRACE: Duration = Duration::from_secs(3);

fn failed_to_start(plan: &CommandPlan, spec: &SessionSpec, why: impl std::fmt::Display) -> SessionOutcome {
    SessionOutcome {
        exit: Exit::Failed { reason: format!("could not start {}: {why}", plan.program.display()) },
        exit_code: None,
        signal: None,
        duration_ms: 0,
        summary: SessionSummary::default(),
        transcript: spec.transcript.clone(),
    }
}

/// A session's process group, held at its gate until `release`.
pub struct Spawned {
    child: Child,
    gate: Option<ChildStdin>,
    released: bool,
    started: Instant,
    pub pid: u32,
    /// The session runs in its own process group, led by `pid`.
    pub pgid: u32,
    pub process_start: Option<u64>,
}

/// Starts a session's process group without starting the host.
pub fn spawn(plan: &CommandPlan, spec: &SessionSpec) -> Result<Spawned, Box<SessionOutcome>> {
    let fail = |why: String| Box::new(failed_to_start(plan, spec, why));
    if let Some(dir) = spec.transcript.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail(format!("cannot create {}: {e}", dir.display())))?;
    }
    if plan.program.components().count() > 1 && !plan.program.exists() {
        return Err(fail("no such file".into()));
    }
    let stdout = std::fs::File::create(&spec.transcript).map_err(|e| fail(e.to_string()))?;
    let stderr = std::fs::File::create(stderr_path(&spec.transcript)).map_err(|e| fail(e.to_string()))?;
    let input = match &plan.stdin {
        Some(text) => {
            let path = spec.transcript.with_extension("stdin.txt");
            std::fs::write(&path, text).map_err(|e| fail(e.to_string()))?;
            path
        }
        None => PathBuf::from("/dev/null"),
    };
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", GATE, "interlock-session"]).arg(&input).arg(&plan.program).args(&plan.args);
    if spec.clear_env {
        cmd.env_clear();
    }
    cmd.current_dir(&spec.workdir)
        .envs(plan.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr);
    // Its own process group, so a timeout or cancel stops every subprocess the
    // host started, and a terminal's Ctrl-C reaches the supervisor, not the host.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| fail(e.to_string()))?;
    let pid = child.id();
    Ok(Spawned {
        gate: child.stdin.take(),
        released: false,
        child,
        started: Instant::now(),
        pid,
        pgid: pid,
        process_start: process_start(pid),
    })
}

impl Spawned {
    /// Lets the host start. Call it once the handoff is recorded.
    pub fn release(&mut self) -> std::io::Result<()> {
        match self.gate.take() {
            Some(mut gate) => {
                self.released = true;
                gate.write_all(b"go\n")
            }
            None => Ok(()),
        }
    }

    /// Closes the gate without releasing it: the host never starts.
    pub fn abandon(mut self) {
        self.gate.take();
        let _ = self.child.wait();
    }

    /// Waits for the session to end, stopping its process group on cancel
    /// or timeout, then reads the transcript.
    pub fn wait(
        mut self,
        spec: &SessionSpec,
        cancel: &AtomicBool,
        summarize: impl Fn(&[String]) -> SessionSummary,
    ) -> SessionOutcome {
        self.gate.take();
        let child = &mut self.child;
        let (exit, code, signal) = loop {
            match child.try_wait() {
                Ok(Some(status)) => break (Exit::Completed, status.code(), signal_of(status)),
                Ok(None) => {}
                Err(e) => break (Exit::Failed { reason: e.to_string() }, None, None),
            }
            if cancel.load(Ordering::SeqCst) {
                stop_child(child, self.pgid);
                break (Exit::Cancelled, None, None);
            }
            if self.started.elapsed() >= spec.timeout {
                stop_child(child, self.pgid);
                break (Exit::TimedOut, None, None);
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        // Anything the host left behind in its group goes with it.
        signal_group(self.pgid, nix::sys::signal::Signal::SIGKILL);
        // 97 is the gate's own exit only when the host was never released;
        // any code from a host that ran is the host's.
        let code = if !self.released && code == Some(NEVER_RELEASED) { None } else { code };
        finish(exit, code, signal, self.started.elapsed(), &spec.transcript, summarize)
    }
}

/// A session process another supervisor started, from its handoff.
#[derive(Debug, Clone)]
pub struct Target {
    pub pid: u32,
    pub pgid: u32,
    pub process_start: Option<u64>,
    pub started_at: SystemTime,
    pub deadline: SystemTime,
    pub transcript: PathBuf,
}

/// Waits for a session this process did not start, as a restarted
/// supervisor does. Its exit code is not observable, so the transcript
/// decides whether it completed.
pub fn attach(target: &Target, cancel: &AtomicBool, summarize: impl Fn(&[String]) -> SessionSummary) -> SessionOutcome {
    let still = || alive(target.pid, target.process_start);
    let exit = loop {
        if !still() {
            break Exit::Completed;
        }
        if cancel.load(Ordering::SeqCst) {
            stop_group(target.pgid, still);
            break Exit::Cancelled;
        }
        if SystemTime::now() >= target.deadline {
            stop_group(target.pgid, still);
            break Exit::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    signal_group(target.pgid, nix::sys::signal::Signal::SIGKILL);
    let took = SystemTime::now().duration_since(target.started_at).unwrap_or_default();
    let mut out = finish(exit, None, None, took, &target.transcript, summarize);
    // Its exit status is not observable from here: a host that ended without
    // its final report died, and is classified as a crash.
    if matches!(out.exit, Exit::Failed { .. }) && !out.summary.finished {
        out.exit = Exit::Failed { reason: "the host ended without reporting a result".into() };
    }
    out
}

fn finish(
    exit: Exit,
    code: Option<i32>,
    signal: Option<i32>,
    took: Duration,
    transcript: &Path,
    summarize: impl Fn(&[String]) -> SessionSummary,
) -> SessionOutcome {
    let lines = read_lines(transcript);
    let summary = summarize(&lines);
    let exit = match exit {
        Exit::Completed if signal.is_some() => {
            Exit::Failed { reason: format!("the host was killed by signal {}", signal.unwrap_or_default()) }
        }
        Exit::Completed if code.is_some_and(|c| c != 0) => {
            Exit::Failed { reason: format!("the host exited with code {}", code.unwrap_or(-1)) }
        }
        Exit::Completed if summary.is_error => Exit::Failed { reason: "the host reported an error".into() },
        other => other,
    };
    SessionOutcome {
        exit,
        exit_code: code,
        signal,
        duration_ms: took.as_millis() as u64,
        summary,
        transcript: transcript.to_path_buf(),
    }
}

/// Runs a plan to completion, timeout or cancellation, in one step.
pub fn run(
    plan: &CommandPlan,
    spec: &SessionSpec,
    cancel: &AtomicBool,
    summarize: impl Fn(&[String]) -> SessionSummary,
) -> SessionOutcome {
    match spawn(plan, spec) {
        Ok(mut s) => {
            let _ = s.release();
            s.wait(spec, cancel, summarize)
        }
        Err(outcome) => *outcome,
    }
}

pub fn stderr_path(transcript: &Path) -> PathBuf {
    transcript.with_extension("stderr.txt")
}

fn read_lines(path: &Path) -> Vec<String> {
    match std::fs::File::open(path) {
        Ok(f) => BufReader::new(f).lines().map_while(Result::ok).collect(),
        Err(_) => vec![],
    }
}

fn tail(path: &Path, bytes: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(bytes)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(not(unix))]
fn signal_of(_: ExitStatus) -> Option<i32> {
    None
}

fn signal_group(pgid: u32, signal: nix::sys::signal::Signal) {
    // Group 0 is the caller's own, and 1 is init's: never a session's.
    if pgid > 1 {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid as i32), signal);
    }
}

/// Asks the group to stop, then kills it after a grace period.
fn stop_group(pgid: u32, mut still_running: impl FnMut() -> bool) {
    signal_group(pgid, nix::sys::signal::Signal::SIGTERM);
    let asked = Instant::now();
    while still_running() && asked.elapsed() < GRACE {
        std::thread::sleep(Duration::from_millis(50));
    }
    signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
}

fn stop_child(child: &mut Child, pgid: u32) {
    stop_group(pgid, || matches!(child.try_wait(), Ok(None)));
    let _ = child.kill();
    let _ = child.wait();
}

/// The kernel's start time for a process, which tells it apart from a later
/// process that reuses its pid. `None` where processes cannot be read.
pub fn process_start(pid: u32) -> Option<u64> {
    procinfo::stat(pid).map(|s| s.start)
}

/// Whether a process is still running: present, not a zombie, and the same
/// process that was recorded when `process_start` is known.
pub fn alive(pid: u32, process_start: Option<u64>) -> bool {
    match procinfo::stat(pid) {
        Some(s) => !s.zombie && process_start.is_none_or(|start| start == s.start),
        None if procinfo::SUPPORTED => false,
        None => match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
            Ok(()) => true,
            Err(e) => e == nix::errno::Errno::EPERM,
        },
    }
}

/// Host stop reasons that mean the host's own cost cap was reached.
const BUDGET_STOPS: &[&str] = &["budget_exhausted", "error_max_budget_usd"];

/// Why a session ended, from its outcome and what the host wrote.
/// `auth_failures` are the host's own sign-in error phrases
/// ([`crate::Host::auth_failures`]). Running out of the task's wall-clock
/// budget looks like a timeout here; the supervisor, which set the
/// deadline, tells the two apart.
pub fn classify(outcome: &SessionOutcome, auth_failures: &[&str]) -> (EndReason, Option<String>) {
    let reason = match &outcome.exit {
        Exit::Completed => return (EndReason::Completed, None),
        Exit::TimedOut => {
            return (
                EndReason::Timeout,
                Some(format!("stopped after {}s without finishing", outcome.duration_ms / 1000)),
            );
        }
        Exit::Cancelled => return (EndReason::Cancelled, None),
        Exit::Failed { reason } => reason,
    };
    if reason.starts_with("could not start") {
        return (EndReason::HostError, Some(reason.clone()));
    }
    // Killed by a signal interlock did not send, or (re-attached, so with no
    // exit status to read) gone without its final report.
    if outcome.signal.is_some() || (outcome.exit_code.is_none() && !outcome.summary.finished) {
        return (EndReason::Crash, Some(reason.clone()));
    }
    let s = &outcome.summary;
    if s.stop_reason.as_deref().is_some_and(|r| BUDGET_STOPS.contains(&r)) {
        return (EndReason::BudgetExhausted, Some("the host stopped at its cost cap".into()));
    }
    let said = format!(
        "{}\n{}\n{}",
        s.final_text.as_deref().unwrap_or_default(),
        tail(&stderr_path(&outcome.transcript), 16 * 1024),
        tail(&outcome.transcript, 4 * 1024)
    )
    .to_lowercase();
    if let Some(hit) = auth_failures.iter().find(|p| said.contains(*p)) {
        return (EndReason::AuthFailure, Some(format!("{reason}; the host said \"{hit}\"")));
    }
    let detail = match &s.stop_reason {
        Some(r) => format!("{reason} ({r})"),
        None => reason.clone(),
    };
    (EndReason::HostError, Some(detail))
}

/// The variable that marks every process of a session, so a process that
/// leaves the session's process group can still be found.
pub const SESSION_MARKER: &str = "INTERLOCK_SESSION";

/// Makes this process adopt its orphaned descendants (Linux), so a session's
/// processes that escape their group and lose their parent come back here.
/// macOS has no such setting: there an orphan goes to launchd, and only its
/// marker ties it to its session.
#[cfg(target_os = "linux")]
pub fn become_subreaper() -> bool {
    nix::sys::prctl::set_child_subreaper(true).is_ok()
}

#[cfg(not(target_os = "linux"))]
pub fn become_subreaper() -> bool {
    false
}

fn pids() -> Vec<u32> {
    procinfo::pids()
}

/// A process's parent and process group.
fn parent_and_group(pid: u32) -> Option<(u32, u32)> {
    procinfo::stat(pid).map(|s| (s.parent, s.group))
}

fn carries_marker(pid: u32, session: &str) -> bool {
    procinfo::started_with(pid, SESSION_MARKER, Some(session))
}

/// Whether this process runs inside a session interlock launched: its own
/// environment names an attempt or a session, or an ancestor's carries the
/// session marker. An agent can clear its own environment, not its
/// ancestors', and a process that left its session was killed with it.
pub fn in_launched_session() -> bool {
    let own = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    if own("INTERLOCK_ATTEMPT") || own("INTERLOCK_TOKEN") || own(SESSION_MARKER) {
        return true;
    }
    let marked = |pid: u32| procinfo::started_with(pid, SESSION_MARKER, None);
    let mut pid = std::process::id();
    for _ in 0..128 {
        match parent_and_group(pid) {
            Some((parent, _)) if parent > 1 => {
                if marked(parent) {
                    return true;
                }
                pid = parent;
            }
            _ => break,
        }
    }
    false
}

/// Processes, other than this one, still running with the session's marker.
pub fn marked(session: &str) -> Vec<u32> {
    let me = std::process::id();
    pids().into_iter().filter(|p| *p != me && alive(*p, None) && carries_marker(*p, session)).collect()
}

fn reap(pid: u32) {
    use nix::sys::wait::{WaitPidFlag, waitpid};
    let _ = waitpid(nix::unistd::Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG));
}

/// Kills what is left of a session: every process carrying its marker, the
/// members of its process group `pgid` when one of them carries the marker
/// (a bare group id may have been reused), and, with `adopted_since`, every
/// child of this process started at or after that kernel start time, which in
/// a subreaper are orphans the session left behind. Repeats until nothing is
/// left, since killing a parent orphans its children. Returns the pids killed.
///
/// Pass `adopted_since` only from a supervisor that has no children of its
/// own running at that moment, with the session leader's start time.
pub fn contain(session: &str, pgid: Option<u32>, adopted_since: Option<u64>) -> Vec<u32> {
    let me = std::process::id();
    let mut killed: Vec<u32> = Vec::new();
    for _ in 0..50 {
        let all: Vec<(u32, u32, u32)> = pids()
            .into_iter()
            .filter(|p| *p != me)
            .filter_map(|p| parent_and_group(p).map(|(parent, group)| (p, parent, group)))
            .collect();
        let mut targets: Vec<u32> = Vec::new();
        for &(p, parent, _) in &all {
            if let Some(since) = adopted_since
                && parent == me
                && process_start(p).is_none_or(|t| t >= since)
            {
                if alive(p, None) {
                    targets.push(p);
                } else {
                    reap(p);
                }
            } else if alive(p, None) && carries_marker(p, session) {
                targets.push(p);
            }
        }
        if let Some(g) = pgid {
            let members: Vec<u32> =
                all.iter().filter(|(p, _, group)| *group == g && alive(*p, None)).map(|m| m.0).collect();
            if members.iter().any(|p| carries_marker(*p, session)) {
                targets.extend(members);
            }
        }
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            break;
        }
        for p in &targets {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(*p as i32), nix::sys::signal::Signal::SIGKILL);
            if !killed.contains(p) {
                killed.push(*p);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        if adopted_since.is_some() {
            for p in &targets {
                reap(*p);
            }
        }
    }
    killed
}

/// Quotes one argument for a POSIX shell command string, as hook commands are.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The name of interlock's hooks plugin. Copilot names the plugin's agents
/// `interlock-hooks:<agent>`.
pub const HOOKS_PLUGIN: &str = "interlock-hooks";

/// Writes interlock's hooks as a Claude-format plugin, which both Claude Code
/// and Copilot CLI load with `--plugin-dir`.
pub fn write_hooks_plugin(dir: &std::path::Path, interlock_bin: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join(".claude-plugin"))?;
    std::fs::create_dir_all(dir.join("hooks"))?;
    let manifest = serde_json::json!({
        "name": HOOKS_PLUGIN,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "interlock per-call policy and stop guard"
    });
    std::fs::write(dir.join(".claude-plugin/plugin.json"), serde_json::to_string_pretty(&manifest)?)?;
    let bin = shell_quote(&interlock_bin.display().to_string());
    let hook = |event: &str| serde_json::json!([{ "type": "command", "command": format!("{bin} hook {event}"), "timeout": 60 }]);
    let hooks = serde_json::json!({
        "hooks": {
            "PreToolUse": [{ "matcher": "*", "hooks": hook("pre-tool-use") }],
            "Stop": [{ "hooks": hook("stop") }],
        }
    });
    std::fs::write(dir.join("hooks/hooks.json"), serde_json::to_string_pretty(&hooks)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dir: &std::path::Path, timeout_ms: u64) -> SessionSpec {
        SessionSpec {
            workdir: dir.to_path_buf(),
            prompt: "hi".into(),
            append_system: None,
            agent: None,
            tools: ToolPolicy::default(),
            model: None,
            max_turns: None,
            plugin_dir: None,
            extra_plugin_dirs: vec![],
            env: vec![],
            timeout: Duration::from_millis(timeout_ms),
            transcript: dir.join("t.jsonl"),
            session_id: None,
            max_cost_usd: None,
            effort: None,
            clear_env: false,
        }
    }

    const CLAUDE_AUTH: &[&str] = crate::claude_code::AUTH_FAILURES;

    /// Runs its arguments in a new session, as setsid(1) does where there is
    /// one: macOS has none.
    const SETSID: &str = "/usr/bin/perl -MPOSIX=setsid -e 'setsid(); exec @ARGV or die'";

    fn sh(script: &str) -> CommandPlan {
        CommandPlan { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], stdin: None, env: vec![] }
    }

    fn count(lines: &[String]) -> SessionSummary {
        SessionSummary { events: lines.len() as u64, final_text: lines.last().cloned(), ..Default::default() }
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn completes_and_keeps_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&sh("echo a; echo b"), &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::Completed);
        assert_eq!(out.summary.events, 2);
        assert_eq!(std::fs::read_to_string(dir.path().join("t.jsonl")).unwrap(), "a\nb\n");
    }

    #[test]
    fn a_non_zero_exit_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&sh("exit 3"), &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert!(matches!(out.exit, Exit::Failed { .. }));
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::HostError);
    }

    #[test]
    fn times_out_and_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&sh("sleep 5"), &spec(dir.path(), 200), &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::TimedOut);
        assert!(out.duration_ms < 3000);
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::Timeout);
        let out = run(&sh("sleep 5"), &spec(dir.path(), 5000), &AtomicBool::new(true), count);
        assert_eq!(out.exit, Exit::Cancelled);
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::Cancelled);
    }

    #[test]
    fn feeds_the_prompt_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = sh("cat");
        plan.stdin = Some("from stdin\n".into());
        let out = run(&plan, &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert_eq!(out.summary.final_text.as_deref(), Some("from stdin"));
    }

    #[test]
    fn the_gate_holds_the_host_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let plan = sh(&format!("echo $$ > {}", marker.display()));
        let s = spec(dir.path(), 5000);
        let mut spawned = spawn(&plan, &s).ok().unwrap();
        assert!(alive(spawned.pid, spawned.process_start));
        assert_eq!(spawned.pgid, spawned.pid);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "the host ran before its handoff could be recorded");
        spawned.release().unwrap();
        let pid = spawned.pid;
        let out = spawned.wait(&s, &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::Completed);
        let ran_as: u32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        assert_eq!(ran_as, pid, "the host replaces the gate, so the recorded pid is the host's");
    }

    #[test]
    fn an_abandoned_gate_never_starts_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let spawned = spawn(&sh(&format!("touch {}", marker.display())), &spec(dir.path(), 5000)).ok().unwrap();
        let pid = spawned.pid;
        spawned.abandon();
        assert!(!alive(pid, None));
        assert!(!marker.exists());
    }

    #[test]
    fn a_missing_host_binary_is_a_host_error() {
        let dir = tempfile::tempdir().unwrap();
        let plan = CommandPlan { program: "/no/such/copilot".into(), args: vec![], stdin: None, env: vec![] };
        let out = run(&plan, &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert!(matches!(&out.exit, Exit::Failed { reason } if reason.starts_with("could not start")));
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::HostError);
    }

    #[test]
    fn a_host_killed_by_a_signal_crashed() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&sh("kill -9 $$"), &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert_eq!(out.signal, Some(9));
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::Crash);
    }

    #[test]
    fn sign_in_failures_and_host_cost_caps_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &sh("echo 'Error: Not logged in. Please run /login' >&2; exit 1"),
            &spec(dir.path(), 5000),
            &AtomicBool::new(false),
            count,
        );
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::AuthFailure);
        // Only the host's own sign-in errors count: a tool's 401 is not the host failing to sign in.
        let out = run(
            &sh("echo 'curl: HTTP 401 Unauthorized' >&2; exit 1"),
            &spec(dir.path(), 5000),
            &AtomicBool::new(false),
            count,
        );
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::HostError);
        let copilot = crate::copilot::AUTH_FAILURES;
        let out = run(
            &sh("echo 'Error: No authentication information found.' >&2; exit 1"),
            &spec(dir.path(), 5000),
            &AtomicBool::new(false),
            count,
        );
        assert_eq!(classify(&out, copilot).0, EndReason::AuthFailure, "Copilot 1.0.91's message, captured");
        let capped = |lines: &[String]| SessionSummary {
            is_error: true,
            stop_reason: Some("budget_exhausted".into()),
            ..count(lines)
        };
        let out = run(&sh("echo '{}'; exit 1"), &spec(dir.path(), 5000), &AtomicBool::new(false), capped);
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::BudgetExhausted);
    }

    #[test]
    fn stopping_a_session_stops_its_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child.pid");
        // The host starts a background child that would otherwise outlive it.
        let plan = sh(&format!("sleep 30 & echo $! > {}; wait", pidfile.display()));
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let s = spec(dir.path(), 20_000);
        let handle = std::thread::spawn(move || run(&plan, &s, &flag, count));
        wait_until("the background child exists", || pidfile.exists());
        let child: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(alive(child, None));
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap().exit, Exit::Cancelled);
        wait_until("the background child is gone", || !alive(child, None));
    }

    #[test]
    fn attach_follows_a_session_another_supervisor_started() {
        let dir = tempfile::tempdir().unwrap();
        let s = spec(dir.path(), 20_000);
        // The first supervisor starts the session and "dies": it never waits on it.
        let mut first = spawn(&sh("sleep 1; echo '{\"done\":true}'"), &s).ok().unwrap();
        first.release().unwrap();
        let target = Target {
            pid: first.pid,
            pgid: first.pgid,
            process_start: first.process_start,
            started_at: SystemTime::now(),
            deadline: SystemTime::now() + Duration::from_secs(20),
            transcript: s.transcript.clone(),
        };
        let out = attach(&target, &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::Completed);
        assert_eq!(out.exit_code, None, "a process it did not start has no exit code to read");
        assert_eq!(out.summary.final_text.as_deref(), Some("{\"done\":true}"));
        drop(first);

        // Re-attached sessions still honor cancellation and the original deadline.
        let mut second = spawn(&sh("sleep 30"), &s).ok().unwrap();
        second.release().unwrap();
        let mut target = Target { pid: second.pid, pgid: second.pgid, process_start: second.process_start, ..target };
        target.deadline = SystemTime::now() + Duration::from_millis(300);
        assert_eq!(attach(&target, &AtomicBool::new(false), count).exit, Exit::TimedOut);
        assert!(!alive(second.pid, second.process_start));
        let mut third = spawn(&sh("sleep 30"), &s).ok().unwrap();
        third.release().unwrap();
        target = Target { pid: third.pid, pgid: third.pgid, process_start: third.process_start, ..target };
        target.deadline = SystemTime::now() + Duration::from_secs(20);
        assert_eq!(attach(&target, &AtomicBool::new(true), count).exit, Exit::Cancelled);
        assert!(!alive(third.pid, third.process_start));
    }

    #[test]
    fn a_host_that_exits_97_is_not_mistaken_for_an_unreleased_gate() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&sh("exit 97"), &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert_eq!(out.exit_code, Some(97));
        assert!(matches!(&out.exit, Exit::Failed { reason } if reason.contains("code 97")), "{:?}", out.exit);
    }

    #[test]
    fn a_cleared_environment_holds_only_what_the_plan_gives() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = spec(dir.path(), 5000);
        s.clear_env = true;
        let mut plan = sh("/usr/bin/env");
        plan.env = vec![("ONLY_THIS".into(), "1".into())];
        let out = run(&plan, &s, &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::Completed);
        let seen = std::fs::read_to_string(&s.transcript).unwrap();
        assert!(seen.lines().any(|l| l == "ONLY_THIS=1"), "{seen}");
        assert!(std::env::var_os("HOME").is_some() && !seen.contains("HOME="), "{seen}");
    }

    #[test]
    fn a_process_that_leaves_the_group_is_found_by_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("stray.pid");
        // The host starts a process in a new session, outside its group, and exits.
        let mut plan = sh(&format!("{SETSID} sleep 30 </dev/null >/dev/null 2>&1 & echo $! > {}", pidfile.display()));
        let marker = format!("att-marker-{}", std::process::id());
        plan.env = vec![(SESSION_MARKER.into(), marker.clone())];
        let out = run(&plan, &spec(dir.path(), 5000), &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::Completed);
        wait_until("the stray's pid is written", || pidfile.exists());
        let stray: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(alive(stray, None), "it left the session's process group, so the group kill missed it");
        assert_eq!(marked(&marker), vec![stray]);
        assert_eq!(contain(&marker, None, None), vec![stray]);
        wait_until("the stray is gone", || !alive(stray, None));
        assert!(contain(&marker, None, None).is_empty());
    }

    #[test]
    fn a_reattached_host_that_dies_without_its_report_crashed() {
        let dir = tempfile::tempdir().unwrap();
        let s = spec(dir.path(), 20_000);
        let target = |sp: &Spawned| Target {
            pid: sp.pid,
            pgid: sp.pgid,
            process_start: sp.process_start,
            started_at: SystemTime::now(),
            deadline: SystemTime::now() + Duration::from_secs(20),
            transcript: s.transcript.clone(),
        };
        let unfinished = |l: &[String]| SessionSummary { is_error: true, finished: false, ..count(l) };
        let mut died = spawn(&sh("sleep 0.3; kill -9 $$"), &s).ok().unwrap();
        died.release().unwrap();
        let out = attach(&target(&died), &AtomicBool::new(false), unfinished);
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::Crash);
        // A host that reported its own end is judged by that report.
        let reported = |l: &[String]| SessionSummary { is_error: true, finished: true, ..count(l) };
        let mut failed = spawn(&sh("echo '{}'"), &s).ok().unwrap();
        failed.release().unwrap();
        let out = attach(&target(&failed), &AtomicBool::new(false), reported);
        assert_eq!(classify(&out, CLAUDE_AUTH).0, EndReason::HostError);
    }

    #[test]
    fn a_reused_pid_is_not_mistaken_for_the_session() {
        let me = std::process::id();
        let start = process_start(me);
        assert!(start.is_some(), "this platform reads process start times");
        assert!(alive(me, start));
        assert!(!alive(me, start.map(|s| s + 1)), "same pid, different process");
        assert!(!alive(u32::MAX - 1, None));
    }

    #[test]
    fn plugin_hooks_point_at_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        write_hooks_plugin(dir.path(), std::path::Path::new("/opt/my tools/interlock")).unwrap();
        let hooks: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("hooks/hooks.json")).unwrap()).unwrap();
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "'/opt/my tools/interlock' hook pre-tool-use"
        );
        assert!(dir.path().join(".claude-plugin/plugin.json").exists());
    }
}
