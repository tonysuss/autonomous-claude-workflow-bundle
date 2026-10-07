//! Whole `interlock run` processes against a scripted stand-in for Claude
//! Code: a shell script that answers `--version` and `--help` like the real
//! CLI and, as a session, does what each test asks before printing a
//! `result` event. Nothing here needs a model or a network, so these always
//! run. They cover the runtime review's findings: sessions that escape their
//! process group, sessions left behind by a dead supervisor, signals, the
//! controller lock, pausing safely, configuration, and each session's
//! environment.
//!
//! `INTERLOCK_TEST_BIN` points the tests at another `interlock` binary, to
//! show a test failing on an earlier build.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn bin() -> PathBuf {
    std::env::var_os("INTERLOCK_TEST_BIN").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_BIN_EXE_interlock").into())
}

fn task_toml(id: &str, budget: &str, independent: bool) -> String {
    let verified = if independent {
        "\n[[criterion]]\nid = \"verified\"\nstatement = \"An independent run passes\"\ncheck = \"sh check.sh\"\n\
         min_strength = \"observed\"\nproducer = \"independent\"\nbaseline = \"fails\"\n"
    } else {
        ""
    };
    format!(
        "id = \"{id}\"\nrepository = \".\"\nworkflow = \"bug-fix\"\nintent = \"add() returns the difference\"\n\n\
         [budget]\n{budget}\n\n[scope]\npaths = [\"calc.py\", \"tests/**\"]\n\n[[criterion]]\nid = \"fixed\"\n\
         statement = \"add(2, 3) returns 5\"\ncheck = \"sh check.sh\"\nmin_strength = \"tested\"\nproducer = \"self\"\n\
         baseline = \"fails\"\n{verified}"
    )
}

const HELP: &str = r#"  -p, --print\n  --output-format <format> (choices: "text", "json", "stream-json")\n  --allowedTools, --allowed-tools <tools...>\n  --disallowedTools, --disallowed-tools <tools...>\n  --model <model>\n  --agents <json-or-file>\n  --plugin-dir <path>\n"#;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Whether a process is running: present and not a zombie, as ps reports it
/// on Linux and macOS alike.
fn running(pid: u64) -> bool {
    Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().is_ok_and(|o| {
        let state = String::from_utf8_lossy(&o.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    })
}

/// Sends `signal` (a name such as `KILL`) to a process, or to a process group
/// written as `-<pgid>`.
fn kill(target: &str, signal: &str) {
    use nix::sys::signal::{Signal, kill as send, killpg};
    use nix::unistd::Pid;
    let sig: Signal = format!("SIG{signal}").parse().unwrap();
    let sent = match target.strip_prefix('-') {
        Some(group) => killpg(Pid::from_raw(group.parse().unwrap()), sig),
        None => send(Pid::from_raw(target.parse().unwrap()), sig),
    };
    sent.unwrap_or_else(|e| panic!("kill -s {signal} {target}: {e}"));
}

fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < limit, "gave up waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn parse(stdout: &[u8], stderr: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|_| {
        Value::String(format!("{}{}", String::from_utf8_lossy(stdout), String::from_utf8_lossy(stderr)))
    })
}

fn read_pid(path: &Path) -> u64 {
    wait_until(&format!("{} is written", path.display()), Duration::from_secs(60), || {
        std::fs::read_to_string(path).is_ok_and(|s| s.trim().parse::<u64>().is_ok())
    });
    std::fs::read_to_string(path).unwrap().trim().parse().unwrap()
}

struct Fixture {
    repo: tempfile::TempDir,
    /// Holds the fake host, its logs, and the runtime directory for tokens.
    side: tempfile::TempDir,
    host: PathBuf,
}

impl Fixture {
    /// A repository with the `fix-add` task, and a fake Claude Code whose
    /// session runs `body` (a POSIX shell fragment; `$here` is the fake's
    /// directory). `help` is shell run before it prints its help text.
    fn new(budget: &str, help: &str, body: &str) -> Fixture {
        Fixture::with(budget, false, help, body)
    }

    fn with(budget: &str, independent: bool, help: &str, body: &str) -> Fixture {
        let repo = tempfile::tempdir().unwrap();
        let side = tempfile::tempdir().unwrap();
        let p = repo.path();
        std::fs::write(p.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(p.join("check.sh"), "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'\n").unwrap();
        std::fs::write(p.join(".gitignore"), "__pycache__/\n").unwrap();
        std::fs::write(p.join("task.toml"), task_toml("fix-add", budget, independent)).unwrap();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["add", "-A"]);
        git(p, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let host = side.path().join("claude");
        let here = side.path().display().to_string();
        let script = format!(
            "#!/bin/sh\nhere='{here}'\ncase \"$1\" in\n  --version) echo \"2.1.289 (Claude Code)\"; exit 0 ;;\n  \
             --help) {help}printf '{HELP}'; exit 0 ;;\nesac\necho \"$@\" >> \"$here/args.log\"\ncat > /dev/null\n\
             echo '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}}'\n{body}\n\
             echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"num_turns\":1,\
             \"total_cost_usd\":0.01,\"permission_denials\":[],\"session_id\":\"s\"}}'\n"
        );
        std::fs::write(&host, script).unwrap();
        Command::new("chmod").arg("+x").arg(&host).status().unwrap();
        std::fs::create_dir_all(side.path().join("runtime")).unwrap();
        let f = Fixture { repo, side, host };
        f.ok(&["init"]);
        f.ok(&["task", "create", "task.toml"]);
        f
    }

    fn here(&self, name: &str) -> PathBuf {
        self.side.path().join(name)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin());
        cmd.args(args)
            .current_dir(self.repo.path())
            .env("INTERLOCK_DB", self.repo.path().join(".interlock/state.db"))
            .env("INTERLOCK_CLAUDE_BIN", &self.host)
            .env("XDG_RUNTIME_DIR", self.side.path().join("runtime"));
        for k in ["INTERLOCK_ATTEMPT", "INTERLOCK_TOKEN", "INTERLOCK_TREE", "INTERLOCK_MODE", "INTERLOCK_SESSION"] {
            cmd.env_remove(k);
        }
        cmd
    }

    fn interlock(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self.cmd(args).output().unwrap();
        (out.status.code().unwrap_or(-1), parse(&out.stdout, &out.stderr), String::from_utf8_lossy(&out.stderr).into())
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, v, err) = self.interlock(args);
        assert_eq!(code, 0, "{args:?}: {v:#} {err}");
        v
    }

    fn run_args<'a>(task: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["run", task, "--host", "claude-code", "--timeout", "120s"];
        args.extend(extra);
        args
    }

    fn run(&self, extra: &[&str]) -> (i32, Value, String) {
        self.interlock(&Fixture::run_args("fix-add", extra))
    }

    fn spawn_run(&self, extra: &[&str]) -> Child {
        self.cmd(&Fixture::run_args("fix-add", extra)).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
    }

    fn attempts(&self) -> Vec<Value> {
        self.ok(&["attempt", "list", "fix-add"]).as_array().cloned().unwrap_or_default()
    }

    fn signals(&self, task: &str) -> Vec<String> {
        let log = self.ok(&["task", "log", task]);
        log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap().to_string()).collect()
    }

    /// The running worker's handoff, once its session has started.
    fn wait_for_handoff(&self, run: &mut Child) -> Value {
        let start = Instant::now();
        loop {
            if let Some(status) = run.try_wait().unwrap() {
                let out = run.stdout.take().map(std::io::read_to_string).and_then(Result::ok).unwrap_or_default();
                let err = run.stderr.take().map(std::io::read_to_string).and_then(Result::ok).unwrap_or_default();
                panic!("interlock run exited ({status}) before its session started:\n{out}\n{err}");
            }
            if let Some(a) = self.attempts().into_iter().find(|a| a["status"] == "running" && a["handoff"].is_object())
            {
                return a["handoff"].clone();
            }
            assert!(start.elapsed() < Duration::from_secs(60), "no session started");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A token file for `attempt` anywhere under the test's runtime directory.
    fn token_file(&self, attempt: &str) -> Option<PathBuf> {
        let runtime = self.side.path().join("runtime/interlock");
        std::fs::read_dir(runtime)
            .ok()?
            .filter_map(|d| d.ok())
            .map(|d| d.path().join(format!("{attempt}.token")))
            .find(|p| p.exists())
    }
}

fn finish(child: Child) -> (i32, Value, String) {
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), parse(&out.stdout, &out.stderr), String::from_utf8_lossy(&out.stderr).into())
}

/// A stray that leaves the session's process group in a new session, and one
/// that also drops its environment (so no marker) and loses its parent. Perl
/// stands in for setsid(1), which macOS lacks.
const STRAYS: &str = "/usr/bin/perl -MPOSIX=setsid -e 'setsid(); exec @ARGV or die' sleep 301 \
</dev/null >/dev/null 2>&1 & echo $! > \"$here/setsid.pid\"\n\
( env -i /usr/bin/perl -MPOSIX=setsid -e 'setsid(); exec @ARGV or die' /bin/sleep 302 \
</dev/null >/dev/null 2>&1 & echo $! > \"$here/stripped.pid\" )";

/// setsid(1) for the fake host's shell fragments.
const SETSID: &str = "/usr/bin/perl -MPOSIX=setsid -e 'setsid(); exec @ARGV or die'";

#[test]
fn processes_that_leave_the_session_are_stopped_when_it_ends() {
    let f = Fixture::new("max_attempts = 3", "", STRAYS);
    let (code, report, err) = f.run(&["--max-sessions", "1"]);
    assert_eq!(code, 5, "{report:#} {err}");
    let (setsid, stripped) = (read_pid(&f.here("setsid.pid")), read_pid(&f.here("stripped.pid")));
    let stopped: Vec<u64> =
        report["sessions"][0]["stopped_strays"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    assert!(stopped.contains(&setsid), "found by its marker: {report:#}");
    if cfg!(target_os = "linux") {
        wait_until("both strays are gone", Duration::from_secs(10), || !running(setsid) && !running(stripped));
        assert!(stopped.contains(&stripped), "found as an orphan the supervisor adopted: {report:#}");
    } else {
        // macOS has no subreaper: an orphan that also dropped its environment
        // goes to launchd with nothing tying it to the session (docs/runtime.md).
        wait_until("the marked stray is gone", Duration::from_secs(10), || !running(setsid));
        if running(stripped) {
            kill(&stripped.to_string(), "KILL");
        }
    }
}

#[test]
fn a_reattached_session_leaves_no_strays() {
    let f = Fixture::new(
        "max_attempts = 3",
        "",
        &format!("{SETSID} sleep 303 </dev/null >/dev/null 2>&1 & echo $! > \"$here/stray.pid\"\nsleep 4"),
    );
    let mut first = f.spawn_run(&["--max-sessions", "1"]);
    let handoff = f.wait_for_handoff(&mut first);
    let stray = read_pid(&f.here("stray.pid"));
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    assert!(running(handoff["pid"].as_u64().unwrap()) && running(stray));
    let (_, report, err) = f.run(&["--max-sessions", "1"]);
    assert_eq!(report["reattached"].as_array().map(Vec::len), Some(1), "{report:#} {err}");
    wait_until("the stray is gone", Duration::from_secs(10), || !running(stray));
}

#[test]
fn task_cancel_stops_a_session_whose_supervisor_is_gone() {
    let f = Fixture::new(
        "max_attempts = 3",
        "",
        &format!("{SETSID} sleep 304 </dev/null >/dev/null 2>&1 & echo $! > \"$here/stray.pid\"\nsleep 60"),
    );
    let mut first = f.spawn_run(&[]);
    let handoff = f.wait_for_handoff(&mut first);
    let stray = read_pid(&f.here("stray.pid"));
    let attempt = f.attempts()[0]["id"].as_str().unwrap().to_string();
    assert!(f.token_file(&attempt).is_some(), "the token is kept outside the repository");
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    let host = handoff["pid"].as_u64().unwrap();
    assert!(running(host));

    let out = f.ok(&["task", "cancel", "fix-add", "--reason", "nobody is watching"]);
    assert_eq!(out["stopped_sessions"], serde_json::json!([attempt]), "{out:#}");
    wait_until("the session and its stray are gone", Duration::from_secs(10), || !running(host) && !running(stray));
    let a = f.attempts()[0].clone();
    assert_eq!((a["status"].as_str(), a["end"]["reason"].as_str()), (Some("cancelled"), Some("cancelled")), "{a:#}");
    assert!(a["end"]["detail"].as_str().unwrap().contains("interlock task cancel"), "{a:#}");
    assert!(a["spent"]["wall_ms"].as_u64().is_some());
    assert!(f.token_file(&attempt).is_none(), "the token is dropped");
    // A later run finds nothing left to account for.
    let (_, report, _) = f.run(&[]);
    assert_eq!(report["reconciled"], serde_json::json!([]), "{report:#}");
    assert_eq!(report["final_state"], "cancelled");
}

#[test]
fn a_restart_for_one_task_ends_sessions_left_by_another() {
    let f = Fixture::new("max_attempts = 3", "", "sleep 60");
    std::fs::write(f.repo.path().join("other.toml"), task_toml("other", "max_attempts = 3", false)).unwrap();
    f.ok(&["task", "create", "other.toml"]);
    let mut first = f.spawn_run(&[]);
    let handoff = f.wait_for_handoff(&mut first);
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    let host = handoff["pid"].as_u64().unwrap();

    let (code, report, err) = f.interlock(&Fixture::run_args("other", &["--max-sessions", "0"]));
    assert_eq!(code, 5, "{report:#} {err}");
    let attempt = f.attempts()[0].clone();
    assert_eq!(report["reconciled"], serde_json::json!([attempt["id"]]), "{report:#}");
    wait_until("the other task's session is gone", Duration::from_secs(10), || !running(host));
    assert_eq!(attempt["end"]["reason"], "crash");
    assert!(attempt["end"]["detail"].as_str().unwrap().contains("its supervisor was gone"), "{attempt:#}");
    assert_eq!(f.signals("fix-add"), ["G1", "G2", "R3"], "the abandoned task is ready again");
}

#[test]
fn a_dead_sessions_children_are_stopped_at_restart() {
    let f = Fixture::new("max_attempts = 3", "", "sleep 300 & echo $! > \"$here/child.pid\"\nsleep 60");
    let mut first = f.spawn_run(&[]);
    let handoff = f.wait_for_handoff(&mut first);
    let child = read_pid(&f.here("child.pid"));
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    // Only the host dies; its child in the same group lives on.
    kill(&handoff["pid"].to_string(), "KILL");
    wait_until("the host is gone", Duration::from_secs(10), || !running(handoff["pid"].as_u64().unwrap()));
    assert!(running(child));
    let (_, report, err) = f.run(&["--max-sessions", "0"]);
    assert_eq!(report["reconciled"].as_array().map(Vec::len), Some(1), "{report:#} {err}");
    wait_until("the child is gone", Duration::from_secs(10), || !running(child));
}

#[test]
fn a_signal_during_a_baseline_check_stops_it_and_the_first_signal_is_reported() {
    let f = Fixture::new("max_attempts = 3", "", "true");
    let pidfile = f.here("check.pid");
    std::fs::write(f.repo.path().join("check.sh"), format!("echo $$ > {}\nsleep 30\nexit 1\n", pidfile.display()))
        .unwrap();
    git(f.repo.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "slow check"]);
    let run = f.spawn_run(&[]);
    let check = read_pid(&pidfile);
    let started = Instant::now();
    kill(&run.id().to_string(), "INT");
    std::thread::sleep(Duration::from_millis(200));
    let _ = Command::new("kill").args(["-s", "TERM", "--", &run.id().to_string()]).status();
    let (code, report, err) = finish(run);
    assert_eq!(code, 6, "{report:#} {err}");
    assert!(started.elapsed() < Duration::from_secs(10), "the run did not wait out the 30s check");
    let said: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(said["interrupted"], "SIGINT", "the first signal, not the last");
    assert_eq!(report["baseline"], serde_json::json!([]), "a cancelled check run is not recorded");
    wait_until("the check is gone", Duration::from_secs(10), || !running(check));
    assert!(!f.here("args.log").exists(), "no session started");
}

#[test]
fn a_signal_while_the_host_is_inspected_opens_no_attempt() {
    let f = Fixture::new("max_attempts = 3", "touch \"$here/help.started\"; sleep 3; ", "true");
    let run = f.spawn_run(&[]);
    wait_until("the host is being inspected", Duration::from_secs(60), || f.here("help.started").exists());
    kill(&run.id().to_string(), "INT");
    let (code, report, err) = finish(run);
    assert_eq!(code, 6, "{report:#} {err}");
    assert_eq!(report["sessions"], serde_json::json!([]));
    assert!(f.attempts().is_empty(), "no attempt was opened");
    assert!(!f.here("args.log").exists(), "the host never ran a session");
}

#[test]
fn sighup_stops_a_run_like_sigterm() {
    let f = Fixture::new("max_attempts = 3", "", "sleep 60");
    let mut run = f.spawn_run(&[]);
    let handoff = f.wait_for_handoff(&mut run);
    kill(&run.id().to_string(), "HUP");
    let (code, report, err) = finish(run);
    assert_eq!(code, 6, "{report:#} {err}");
    let said: Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(said["interrupted"], "SIGHUP");
    assert!(!running(handoff["pid"].as_u64().unwrap()));
    assert_eq!(f.signals("fix-add"), ["G1", "G2", "R3"]);
}

#[test]
fn pausing_safely_keeps_work_the_worker_committed() {
    let f = Fixture::new(
        "max_attempts = 3",
        "",
        "perl -pi -e 's/a - b/a + b/' calc.py\ngit -c user.name=a -c user.email=a@a commit -qam 'agent commit'\n\
         touch \"$here/committed\"\nsleep 60",
    );
    let run = f.spawn_run(&[]);
    wait_until("the worker committed", Duration::from_secs(60), || f.here("committed").exists());
    kill(&run.id().to_string(), "INT");
    let (code, report, err) = finish(run);
    assert_eq!(code, 6, "{report:#} {err}");
    let export = &report["sessions"][0]["export"];
    assert_eq!(export["reference"], "refs/heads/interlock/wip/fix-add", "{report:#}");
    assert!(git(f.repo.path(), &["show", "interlock/wip/fix-add:calc.py"]).contains("a + b"));
}

#[test]
fn a_bad_config_does_not_stand_in_the_way_of_recovery() {
    let f = Fixture::new("max_attempts = 3", "", "sleep 60");
    let mut first = f.spawn_run(&[]);
    let handoff = f.wait_for_handoff(&mut first);
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    kill(&format!("-{}", handoff["pgid"]), "KILL");
    std::fs::write(f.repo.path().join(".interlock/config.toml"), "[pins\n").unwrap();
    let (code, out, err) = f.run(&[]);
    assert_eq!(code, 1, "{out:#} {err}");
    assert!(err.contains("config.toml"), "{err}");
    let a = f.attempts()[0].clone();
    assert_eq!(a["end"]["reason"], "crash", "recovered before the config was read: {a:#}");
    assert_eq!(f.signals("fix-add"), ["G1", "G2", "R3"]);
    std::fs::write(f.repo.path().join(".interlock/config.toml"), "[pins]\nclaude = \"2.1.289\"\n").unwrap();
    let (code, _, err) = f.run(&[]);
    assert_eq!(code, 1);
    assert!(err.contains("unknown host `claude`"), "{err}");
}

#[test]
fn sessions_get_an_allowlisted_environment_and_the_effort_asked_for() {
    let f = Fixture::new("max_attempts = 3", "printf '  --effort <level>\\n'; ", "env > \"$here/env.txt\"");
    std::fs::write(f.repo.path().join(".interlock/config.toml"), "[env]\npass = [\"MY_TOOL_*\"]\n").unwrap();
    let parent = [
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_SESSION_ID", "parent-session"),
        ("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/parent.sock"),
        ("CLAUDE_CODE_MESSAGING_TOKEN", "secret-messaging-token"),
        ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
        ("CLAUDE_SESSION_INGRESS_TOKEN_FILE", "/tmp/ingress"),
        ("CLAUDE_CODE_ACCOUNT_UUID", "acct"),
        ("GH_TOKEN", "fake-gh-token"),
        ("COPILOT_PROVIDER_BASE_URL", "http://127.0.0.1:9/v1"),
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ("MY_TOOL_HOME", "/opt/my-tool"),
    ];
    let mut cmd = f.cmd(&Fixture::run_args("fix-add", &["--max-sessions", "1", "--effort", "high"]));
    cmd.envs(parent);
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(5), "{}", String::from_utf8_lossy(&out.stderr));
    let seen = std::fs::read_to_string(f.here("env.txt")).unwrap();
    let names: Vec<&str> = seen.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
    for want in ["ANTHROPIC_BASE_URL", "CLAUDE_CODE_USE_BEDROCK", "MY_TOOL_HOME", "PATH", "INTERLOCK_ATTEMPT"] {
        assert!(names.contains(&want), "{want} reaches the session: {names:?}");
    }
    for unwanted in [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_EFFORT_LEVEL",
        "CLAUDE_SESSION_INGRESS_TOKEN_FILE",
        "CLAUDE_CODE_ACCOUNT_UUID",
        "GH_TOKEN",
        "COPILOT_PROVIDER_BASE_URL",
    ] {
        assert!(!names.contains(&unwanted), "{unwanted} must not reach a Claude Code session: {names:?}");
    }
    assert!(!seen.contains("secret-messaging-token") && !seen.contains("fake-gh-token"));
    let handoff = f.attempts()[0]["handoff"].clone();
    let recorded: Vec<&str> = handoff["env"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(recorded.contains(&"ANTHROPIC_BASE_URL") && !recorded.contains(&"GH_TOKEN"), "{recorded:?}");
    assert!(recorded.iter().all(|n| !n.contains('=')), "names only, never values");
    assert_eq!(handoff["effort"], "high");
    let args = std::fs::read_to_string(f.here("args.log")).unwrap();
    assert!(args.contains("--effort high"), "{args}");
}

#[test]
fn an_effort_the_host_cannot_take_is_refused_before_anything_starts() {
    let f = Fixture::new("max_attempts = 3", "", "true");
    let (code, _, err) = f.run(&["--effort", "high"]);
    assert_eq!(code, 1);
    assert!(err.contains("claude-code has no effort setting"), "{err}");
    let f = Fixture::new("max_attempts = 3", "printf '  --effort <level>\\n'; ", "true");
    let (code, _, err) = f.run(&["--effort", "extreme"]);
    assert_eq!(code, 1);
    assert!(err.contains("claude-code takes --effort low, medium, high, xhigh, max, not extreme"), "{err}");
    assert!(f.attempts().is_empty());
}

#[test]
fn a_session_cannot_grant_itself_authority_however_it_calls_interlock() {
    // The session drops its attempt variables and the session marker from its
    // own environment, and calls interlock through a variable the hook's text
    // check cannot see. An ancestor still carries the marker.
    let body = "I=interlock; env -u INTERLOCK_ATTEMPT -u INTERLOCK_TOKEN -u INTERLOCK_SESSION \
                $I grant create --principal worker --tasks fix-add --classes landing --landing coordinator \
                --origin self > \"$here/grant.out\" 2>&1; echo \"exit $?\" >> \"$here/grant.out\"; \
                env -u INTERLOCK_ATTEMPT -u INTERLOCK_TOKEN -u INTERLOCK_SESSION \
                $I task unblock fix-add >> \"$here/grant.out\" 2>&1; echo \"exit $?\" >> \"$here/grant.out\"";
    let f = Fixture::new("max_attempts = 3", "", body);
    f.run(&["--max-sessions", "1"]);
    let out = std::fs::read_to_string(f.here("grant.out")).unwrap();
    assert!(out.contains("is the operator's"), "{out}");
    assert!(!out.contains("exit 0"), "both commands are refused: {out}");
    assert_eq!(f.ok(&["grant", "list"]), serde_json::json!([]), "no grant exists afterwards");
    // From the operator's own shell the same command works.
    let made = f.ok(&[
        "grant",
        "create",
        "--principal",
        "operator",
        "--tasks",
        "fix-add",
        "--classes",
        "landing",
        "--landing",
        "coordinator",
        "--origin",
        "operator",
    ]);
    assert!(made["id"].as_str().is_some_and(|id| id.starts_with("grant-")), "{made:#}");
}

#[test]
fn the_host_policy_narrows_every_session_and_a_tool_the_worker_needs_refuses_g2() {
    let f = Fixture::new("max_attempts = 3", "", "true");
    let config = f.repo.path().join(".interlock/config.toml");
    // A narrower deny reaches the host's own tool filter.
    std::fs::write(&config, "[host_policy.claude-code]\ndeny = [\"shell:curl\"]\n").unwrap();
    f.run(&["--max-sessions", "1"]);
    let args = std::fs::read_to_string(f.here("args.log")).unwrap();
    assert!(args.contains("Bash(curl:*)"), "the host denies what its policy denies: {args}");
    let worker = &f.attempts()[0];
    assert!(
        worker["effective_grant"]["tools"]["deny"].as_array().unwrap().iter().any(|d| d == "shell:curl"),
        "{worker:#}"
    );

    // Taking away a tool the worker needs refuses G2 and starts no session.
    let g = Fixture::new("max_attempts = 3", "", "true");
    std::fs::write(g.repo.path().join(".interlock/config.toml"), "[host_policy.claude-code]\ndeny = [\"edit\"]\n")
        .unwrap();
    let (code, report, err) = g.run(&["--max-sessions", "1"]);
    assert_ne!(code, 0, "{report:#}");
    assert!(format!("{report}{err}").contains("lacks tools a Worker needs: edit"), "{report:#} {err}");
    assert!(!g.here("args.log").exists(), "no session started");

    // An unknown host in the policy is a config error, not a silent no-op.
    let h = Fixture::new("max_attempts = 3", "", "true");
    std::fs::write(h.repo.path().join(".interlock/config.toml"), "[host_policy.claud]\ndeny = [\"web\"]\n").unwrap();
    let (code, report, err) = h.run(&["--max-sessions", "1"]);
    assert_ne!(code, 0);
    assert!(format!("{report}{err}").contains("unknown host `claud`"), "{report:#} {err}");
}

#[test]
fn capabilities_the_adapter_finds_missing_give_the_declared_fallback_or_block() {
    // No --model in the host's help: model selection falls back to the current
    // model, which the session then reports and the attempt records.
    let no_model = "printf '  -p, --print\\n  --output-format <format> (choices: \"text\", \"json\", \"stream-json\")\\n  \
                    --allowedTools, --allowed-tools <tools...>\\n  --disallowedTools, --disallowed-tools <tools...>\\n  \
                    --plugin-dir <path>\\n'; exit 0; ";
    let f = Fixture::new("max_attempts = 3", no_model, "true");
    f.run(&["--max-sessions", "1"]);
    let log = f.ok(&["task", "log", "fix-add"]);
    let g2 = log.as_array().unwrap().iter().find(|t| t["signal"] == "G2").expect("the worker started");
    assert!(g2["reason"].as_str().unwrap().contains("CurrentModel for model selection"), "{g2:#}");

    // Neither tool restriction nor hooks: nothing could hold the worker to its
    // grant, so the task is blocked with the reason and no session starts.
    let bare = "printf '  -p, --print\\n  --output-format <format> (choices: \"text\", \"json\", \"stream-json\")\\n'; exit 0; ";
    let g = Fixture::new("max_attempts = 3", bare, "true");
    let (code, report, err) = g.run(&["--max-sessions", "1"]);
    assert_eq!(code, 5, "{report:#} {err}");
    assert_eq!(report["final_state"], "blocked", "{report:#}");
    let task = g.ok(&["task", "show", "fix-add"]);
    assert!(task["blocked_reason"].as_str().unwrap().contains("tool restriction enforced by the host"), "{task:#}");
    assert!(!g.here("args.log").exists(), "no session started");
}

#[test]
fn a_skills_plugin_is_loaded_into_every_session_and_a_non_plugin_is_refused() {
    let f = Fixture::new("max_attempts = 3", "", "true");
    let skills = f.here("skills");
    std::fs::create_dir_all(skills.join(".claude-plugin")).unwrap();
    std::fs::write(skills.join(".claude-plugin/plugin.json"), r#"{"name": "interlock"}"#).unwrap();
    let dir = skills.canonicalize().unwrap().display().to_string();
    f.run(&["--max-sessions", "1", "--skills", &dir]);
    let args = std::fs::read_to_string(f.here("args.log")).unwrap();
    assert!(args.contains(&format!("--plugin-dir {dir}")), "the skills plugin reaches the host: {args}");
    assert_eq!(args.matches("--plugin-dir").count(), 2, "interlock's hooks plugin and the skills plugin: {args}");

    let plain = f.here("not-a-plugin");
    std::fs::create_dir_all(&plain).unwrap();
    let (code, _, err) = f.run(&["--skills", &plain.display().to_string()]);
    assert_ne!(code, 0);
    assert!(err.contains("is not a plugin"), "{err}");
    assert_eq!(args, std::fs::read_to_string(f.here("args.log")).unwrap(), "no session started");
}

#[test]
fn max_sessions_zero_runs_the_baseline_only() {
    let f = Fixture::new("max_attempts = 3", "", "true");
    let (code, report, err) = f.run(&["--max-sessions", "0"]);
    assert_eq!(code, 5, "{report:#} {err}");
    assert_eq!(report["baseline"][0]["criterion_id"], "fixed", "{report:#}");
    assert_eq!(report["baseline"][0]["exit_code"], 1);
    assert_eq!(report["sessions"], serde_json::json!([]));
    assert!(report["stopped_because"].as_str().unwrap().starts_with("baseline only"), "{report:#}");
    assert_eq!(f.signals("fix-add"), ["G1"]);
    assert!(!f.here("args.log").exists());
}

#[test]
fn only_one_supervisor_gets_the_lock_however_many_race() {
    for stale in [false, true] {
        let f = Fixture::new("max_attempts = 3", "", "sleep 3");
        if stale {
            // A pid that is alive but holds nothing.
            std::fs::write(f.repo.path().join(".interlock/supervisor.lock"), "1").unwrap();
        }
        let runs: Vec<Child> = (0..6).map(|_| f.spawn_run(&["--max-sessions", "1"])).collect();
        let outs: Vec<(i32, Value, String)> = runs.into_iter().map(finish).collect();
        let refused = outs.iter().filter(|(_, _, err)| err.contains("another supervisor")).count();
        assert_eq!(refused, 5, "exactly one of six held the lock (stale file: {stale}): {outs:#?}");
        assert_eq!(f.attempts().len(), 1, "one session in all");
    }
}

/// A worker's escaped process that waits for the verifier to start, reads its
/// token from the verifier's environment (/proc on Linux, `ps -E` on macOS),
/// and records assessments as the verifier.
const FORGER: &str = r#"#!/bin/sh
out="$1"; me="$INTERLOCK_ATTEMPT"; i=0
echo "watching as $me" >> "$out"
# One line per process holding an attempt token: its environment, space-separated.
scan() {
  if [ -d /proc/self ]; then
    for f in $(grep -l -a "INTERLOCK_TOKEN=" /proc/[0-9]*/environ 2>/dev/null); do
      tr '\0' ' ' < "$f" 2>/dev/null; echo
    done
  else
    ps -A -E -ww -o command= 2>/dev/null | grep "INTERLOCK_TOKEN="
  fi
}
val() { printf '%s\n' "$line" | tr ' ' '\n' | sed -n "s/^$1=//p" | head -1; }
while [ $i -lt 600 ]; do
  scan > "$out.scan"
  while IFS= read -r line; do
    id=$(val INTERLOCK_ATTEMPT); db=$(val INTERLOCK_DB)
    [ -n "$id" ] && [ "$id" != "$me" ] && [ "$db" = "$INTERLOCK_DB" ] || continue
    tok=$(val INTERLOCK_TOKEN); wt=$(val PWD)
    [ -n "$tok" ] || continue
    echo "stole the token of $id" >> "$out"
    cd "${wt:-.}" && for c in fixed verified; do
      INTERLOCK_ATTEMPT="$id" INTERLOCK_TOKEN="$tok" interlock assess add --criterion $c --strength observed \
        --tree auto --ref forged --note "forged by the worker" >> "$out" 2>&1
    done
    exit 0
  done < "$out.scan"
  i=$((i+1)); sleep 0.1
done
"#;

#[test]
fn an_escaped_worker_process_cannot_forge_the_verifiers_evidence() {
    let f = Fixture::with(
        "max_attempts = 3",
        true,
        "",
        &format!(
            "if [ -z \"$INTERLOCK_TREE\" ]; then\n  perl -pi -e 's/a - b/a + b/' calc.py\n  \
         {SETSID} \"$here/forger.sh\" \"$here/forger.log\" </dev/null >/dev/null 2>&1 &\nelse\n  sleep 8\nfi"
        ),
    );
    std::fs::write(f.here("forger.sh"), FORGER).unwrap();
    Command::new("chmod").arg("+x").arg(f.here("forger.sh")).status().unwrap();
    let (_, report, err) = f.run(&["--max-sessions", "2"]);
    assert_eq!(report["sessions"].as_array().map(Vec::len), Some(2), "a worker and a verifier: {report:#} {err}");
    let events = f.ok(&["task", "events", "fix-add"]);
    let forged = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "assessment.added")
        .filter(|e| e["outcome"]["evidence"]["note"] == "forged by the worker")
        .count();
    assert_eq!(forged, 0, "the forger was stopped with the worker's session: {}", {
        std::fs::read_to_string(f.here("forger.log")).unwrap_or_default()
    });
    assert_ne!(report["final_state"], "done");
}

/// A verifier that finds a gap the worker cannot close records `blocked` with the reason. The
/// task does not reach done: a second verifier is asked, then the task waits for the operator,
/// whose status shows the verifier's reason.
#[test]
fn a_gap_the_verifier_records_as_blocked_keeps_the_task_from_done_and_reaches_the_operator() {
    let f = Fixture::with(
        "max_attempts = 3",
        true,
        "",
        "if [ -z \"$INTERLOCK_TREE\" ]; then\n  perl -pi -e 's/a - b/a + b/' calc.py\n  \
         interlock check run --criterion fixed >/dev/null\n  \
         interlock claim add --criterion fixed --strength tested --tree auto --ref 'sh check.sh' --note ok >/dev/null\n\
         else\n  interlock check run --criterion verified >/dev/null\n  \
         interlock assess add --criterion fixed --strength tested --tree auto --ref 'sh check.sh' --note ok >/dev/null\n  \
         interlock assess add --criterion verified --strength blocked --tree auto --ref 'read the change' \
         --note 'the goal also needs sub, which is outside the scope' >/dev/null\nfi",
    );
    let (_, report, err) = f.run(&["--max-sessions", "6"]);
    assert_eq!(report["final_state"], "blocked", "{report:#} {err}");
    let roles: Vec<&str> = report["sessions"].as_array().unwrap().iter().filter_map(|s| s["role"].as_str()).collect();
    assert_eq!(roles, ["worker", "verifier", "verifier"], "{report:#}");
    let status = f.ok(&["status", "fix-add"]).to_string();
    assert!(
        status.contains("a verifier recorded blocked: the goal also needs sub, which is outside the scope"),
        "{status}"
    );
    assert!(!f.signals("fix-add").iter().any(|s| s == "G7"), "never done");
}

#[test]
fn budgets_must_be_finite_amounts() {
    let f = Fixture::new("max_attempts = 3", "", "true");
    std::fs::write(
        f.repo.path().join("nan.toml"),
        task_toml("nan-budget", "max_attempts = 3\nmax_cost_usd = nan", false),
    )
    .unwrap();
    let (code, out, err) = f.interlock(&["task", "create", "nan.toml"]);
    assert_eq!(code, 2, "{out:#}");
    assert!(err.contains("max_cost_usd must be a finite number above zero"), "{err}");
    std::fs::write(
        f.repo.path().join("inf.toml"),
        task_toml("inf-budget", "max_attempts = 3\nmax_premium_requests = inf", false),
    )
    .unwrap();
    let (code, _, err) = f.interlock(&["task", "create", "inf.toml"]);
    assert_eq!(code, 2, "{err}");
}
