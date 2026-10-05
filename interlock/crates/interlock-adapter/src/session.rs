//! Running one headless session. Each host turns a host-neutral
//! `SessionSpec` into a `CommandPlan`; one runner executes any plan with a
//! timeout and cancellation, and keeps the raw event stream as a transcript.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use interlock_schema::ToolPolicy;
use serde::{Deserialize, Serialize};

/// What to run, in host-neutral terms.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// The worktree the agent works in.
    pub workdir: PathBuf,
    pub prompt: String,
    /// Role instructions appended to the host's system prompt, where supported.
    pub append_system: Option<String>,
    /// Host-neutral tool policy; the host translates it.
    pub tools: ToolPolicy,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    /// A Claude-format plugin carrying interlock's hooks. Both hosts load it.
    pub plugin_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// Where the raw output stream is written, one event per line.
    pub transcript: PathBuf,
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
    pub denials: u64,
    pub events: u64,
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
    pub duration_ms: u64,
    pub summary: SessionSummary,
    pub transcript: PathBuf,
}

/// Runs a plan to completion, timeout or cancellation. The host's
/// `summarize` reads the transcript lines afterwards.
pub fn run(
    plan: &CommandPlan,
    spec: &SessionSpec,
    cancel: &AtomicBool,
    summarize: impl Fn(&[String]) -> SessionSummary,
) -> SessionOutcome {
    let started = Instant::now();
    let finish = |exit: Exit, code: Option<i32>, lines: &[String]| {
        let summary = summarize(lines);
        let exit = match exit {
            Exit::Completed if summary.is_error => Exit::Failed { reason: "the host reported an error".into() },
            Exit::Completed if code.is_some_and(|c| c != 0) => {
                Exit::Failed { reason: format!("the host exited with code {}", code.unwrap_or(-1)) }
            }
            other => other,
        };
        SessionOutcome {
            exit,
            exit_code: code,
            duration_ms: started.elapsed().as_millis() as u64,
            summary,
            transcript: spec.transcript.clone(),
        }
    };

    if let Some(dir) = spec.transcript.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let stderr_path = spec.transcript.with_extension("stderr.txt");
    let stderr_file = std::fs::File::create(&stderr_path).ok();
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .current_dir(&spec.workdir)
        .envs(plan.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(if plan.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(stderr_file.map(Stdio::from).unwrap_or_else(Stdio::null));
    // Its own process group, so a timeout or cancel stops every subprocess the
    // host started, not just the host.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return finish(
                Exit::Failed { reason: format!("could not start {}: {e}", plan.program.display()) },
                None,
                &[],
            );
        }
    };
    if let (Some(text), Some(mut stdin)) = (&plan.stdin, child.stdin.take()) {
        let text = text.clone();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let stdout = child.stdout.take().expect("stdout is piped");
    let transcript_path = spec.transcript.clone();
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let sink = lines.clone();
    std::thread::spawn(move || {
        let mut out = std::fs::File::create(&transcript_path).ok();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(f) = out.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if let Ok(mut l) = sink.lock() {
                l.push(line);
            }
        }
        let _ = done_tx.send(());
    });
    let stop = |child: &mut std::process::Child| {
        #[cfg(unix)]
        let _ = Command::new("kill")
            .args(["-s", "KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
    };

    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Exit::Completed, status.code()),
            Ok(None) => {}
            Err(e) => break (Exit::Failed { reason: e.to_string() }, None),
        }
        if cancel.load(Ordering::SeqCst) {
            stop(&mut child);
            break (Exit::Cancelled, None);
        }
        if started.elapsed() >= spec.timeout {
            stop(&mut child);
            break (Exit::TimedOut, None);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // A subprocess that escaped the group can hold the pipe open; read what
    // arrived and move on rather than wait for it.
    let _ = done_rx.recv_timeout(Duration::from_secs(5));
    let lines = lines.lock().map(|l| l.clone()).unwrap_or_default();
    finish(exit.0, exit.1, &lines)
}

/// Quotes one argument for a POSIX shell command string, as hook commands are.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Writes interlock's hooks as a Claude-format plugin, which both Claude Code
/// and Copilot CLI load with `--plugin-dir`.
pub fn write_hooks_plugin(dir: &std::path::Path, interlock_bin: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join(".claude-plugin"))?;
    std::fs::create_dir_all(dir.join("hooks"))?;
    let manifest = serde_json::json!({
        "name": "interlock-hooks",
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
            tools: ToolPolicy::default(),
            model: None,
            max_turns: None,
            plugin_dir: None,
            env: vec![],
            timeout: Duration::from_millis(timeout_ms),
            transcript: dir.join("t.jsonl"),
        }
    }

    fn sh(script: &str) -> CommandPlan {
        CommandPlan { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], stdin: None, env: vec![] }
    }

    fn count(lines: &[String]) -> SessionSummary {
        SessionSummary { events: lines.len() as u64, final_text: lines.last().cloned(), ..Default::default() }
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
    }

    #[test]
    fn times_out_and_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&sh("sleep 5"), &spec(dir.path(), 200), &AtomicBool::new(false), count);
        assert_eq!(out.exit, Exit::TimedOut);
        assert!(out.duration_ms < 3000);
        let out = run(&sh("sleep 5"), &spec(dir.path(), 5000), &AtomicBool::new(true), count);
        assert_eq!(out.exit, Exit::Cancelled);
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
