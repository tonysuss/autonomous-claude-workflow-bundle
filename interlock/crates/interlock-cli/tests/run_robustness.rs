//! Execution robustness, end to end with real processes: a supervisor
//! killed mid-session and restarted, signals, `task cancel` from another
//! terminal, timeouts, budgets, version pins, and pausing safely. Most run
//! the real Copilot CLI offline against a scripted model and skip without
//! one; the cost-budget test uses a scripted stand-in for Claude Code.
//!
//! Every test ends by checking that each attempt it started is in a
//! terminal row.

mod fake_model;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fake_model::{FakeModel, Script};
use serde_json::Value;

const FIX: &str = "sed -i 's/a - b/a + b/' calc.py";
const CLAIM: &str = "interlock claim add --criterion fixed --strength tested --tree auto --ref 'sh check.sh'";
const CHECK_FIXED: &str = "interlock check run --criterion fixed";

fn task_toml(budget: &str) -> String {
    format!(
        r#"
id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add() returns the difference instead of the sum"

[budget]
{budget}

[scope]
paths = ["calc.py", "tests/**"]

[[criterion]]
id = "fixed"
statement = "add(2, 3) returns 5"
check = "sh check.sh"
min_strength = "tested"
producer = "self"
baseline = "fails"

[[criterion]]
id = "verified"
statement = "An independent run of the check passes"
check = "sh check.sh"
min_strength = "observed"
producer = "independent"
baseline = "fails"
"#
    )
}

fn copilot() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("INTERLOCK_COPILOT_BIN").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("copilot")).find(|p| p.is_file())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn steps(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn verifier_pass() -> Vec<String> {
    steps(&[
        "interlock check run --criterion fixed",
        "interlock check run --criterion verified",
        "interlock assess add --criterion fixed --strength tested --ref 'sh check.sh'",
        "interlock assess add --criterion verified --strength observed --ref 'sh check.sh'",
    ])
}

/// Whether a process is running: present and not a zombie.
fn running(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rfind(')').map(|i| !s[i + 2..].starts_with('Z')))
        .unwrap_or(false)
}

fn kill(pid: &str, signal: &str) {
    let status = Command::new("kill").args(["-s", signal, "--", pid]).status().unwrap();
    assert!(status.success(), "kill -s {signal} {pid}");
}

fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < limit, "gave up waiting until {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Copilot processes share a package cache; one session at a time keeps the tests steady.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    host: PathBuf,
    _turn: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(budget: &str) -> Option<Fixture> {
        let Some(host) = copilot() else {
            eprintln!("skipping: no Copilot CLI binary");
            return None;
        };
        Some(Fixture::with(budget, host))
    }

    fn with(budget: &str, host: PathBuf) -> Fixture {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let repo = tempfile::tempdir().unwrap();
        let p = repo.path();
        std::fs::write(p.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(p.join("check.sh"), "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'\n").unwrap();
        std::fs::write(p.join(".gitignore"), "__pycache__/\n").unwrap();
        std::fs::write(p.join("task.toml"), task_toml(budget)).unwrap();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["add", "-A"]);
        git(p, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let f = Fixture { repo, home: tempfile::tempdir().unwrap(), host, _turn: turn };
        f.interlock(&["init"], None);
        f.interlock(&["task", "create", "task.toml"], None);
        f
    }

    fn cmd(&self, args: &[&str], model: Option<&FakeModel>) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_interlock"));
        cmd.args(args)
            .current_dir(self.repo.path())
            .env("INTERLOCK_DB", self.repo.path().join(".interlock/state.db"))
            .env("INTERLOCK_COPILOT_BIN", &self.host)
            .env("INTERLOCK_CLAUDE_BIN", &self.host)
            .env("COPILOT_HOME", self.home.path())
            .env("COPILOT_OFFLINE", "true")
            .env("COPILOT_MODEL", "gpt-4.1")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        for k in ["INTERLOCK_ATTEMPT", "INTERLOCK_TOKEN", "INTERLOCK_TREE", "INTERLOCK_MODE"] {
            cmd.env_remove(k);
        }
        if let Some(m) = model {
            cmd.env("COPILOT_PROVIDER_BASE_URL", m.base_url());
        }
        cmd
    }

    fn interlock(&self, args: &[&str], model: Option<&FakeModel>) -> (i32, Value) {
        let out = self.cmd(args, model).output().unwrap();
        (out.status.code().unwrap_or(-1), parse(&out.stdout, &out.stderr))
    }

    fn run_args(host: &str, timeout: &str) -> Vec<String> {
        steps(&["run", "fix-add", "--host", host, "--timeout", timeout])
    }

    fn run(&self, model: &FakeModel, timeout: &str) -> (i32, Value) {
        let args = Fixture::run_args("copilot", timeout);
        self.interlock(&args.iter().map(String::as_str).collect::<Vec<_>>(), Some(model))
    }

    /// Starts `interlock run` in the background, as a person would in a terminal.
    fn spawn_run(&self, model: &FakeModel) -> Child {
        let args = Fixture::run_args("copilot", "120s");
        self.cmd(&args.iter().map(String::as_str).collect::<Vec<_>>(), Some(model))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn signals(&self) -> Vec<String> {
        let (_, log) = self.interlock(&["task", "log", "fix-add"], None);
        log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap().to_string()).collect()
    }

    fn attempts(&self) -> Vec<Value> {
        let (_, list) = self.interlock(&["attempt", "list", "fix-add"], None);
        list.as_array().cloned().unwrap_or_default()
    }

    fn task(&self) -> Value {
        self.interlock(&["task", "show", "fix-add"], None).1
    }

    /// P3's gate: every started attempt reconciles to a terminal row.
    fn assert_every_attempt_ended(&self) {
        for a in self.attempts() {
            let status = a["status"].as_str().unwrap();
            assert!(
                ["completed", "failed", "cancelled", "superseded"].contains(&status),
                "attempt {} is still {status}",
                a["id"]
            );
            assert!(a["ended_at"].is_string(), "attempt {} has no end time", a["id"]);
            assert!(a["end"]["reason"].is_string(), "attempt {} has no recorded end reason: {a:#}", a["id"]);
        }
    }
}

fn parse(stdout: &[u8], stderr: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|_| {
        Value::String(format!("{}{}", String::from_utf8_lossy(stdout), String::from_utf8_lossy(stderr)))
    })
}

fn finish(child: Child) -> (i32, Value) {
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), parse(&out.stdout, &out.stderr))
}

/// Worker sessions the model saw: each one's first request has no tool results yet.
fn worker_sessions(model: &FakeModel) -> usize {
    model.requests().iter().filter(|r| r["role"] == "worker" && r["tools_done"] == 0).count()
}

/// Live processes of a session, found by the marker interlock puts in every
/// session's environment, with their command lines.
fn session_processes(attempt: &str) -> Vec<(u64, String)> {
    let marker = format!("INTERLOCK_SESSION={attempt}");
    let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u64>().ok())
        .filter(|pid| running(*pid))
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|kv| kv == marker.as_bytes()))
        })
        .filter_map(|pid| {
            let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            Some((pid, String::from_utf8_lossy(&cmd).replace('\0', " ").trim().to_string()))
        })
        .collect()
}

/// Blocks until a session of `role` is running its `sleep` step: a
/// deterministic point mid-session, whatever the machine's load. Fails at
/// once, with the run's output, if `interlock run` exits first.
fn wait_for_sleep(f: &Fixture, run: &mut Child, role: &str) -> Value {
    let start = Instant::now();
    loop {
        if let Some(status) = run.try_wait().unwrap() {
            let mut out = String::new();
            let mut err = String::new();
            if let Some(mut s) = run.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut s, &mut out);
            }
            if let Some(mut s) = run.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut s, &mut err);
            }
            panic!("interlock run exited ({status}) before the {role} reached its sleep step:\n{out}\n{err}");
        }
        let attempt = f
            .attempts()
            .into_iter()
            .find(|a| a["role"] == role && a["status"] == "running" && a["handoff"].is_object());
        if let Some(a) = attempt
            && session_processes(a["id"].as_str().unwrap()).iter().any(|(_, cmd)| cmd.starts_with("sleep"))
        {
            return a["handoff"].clone();
        }
        assert!(start.elapsed() < Duration::from_secs(240), "gave up waiting for the {role}'s sleep step");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_supervisor_killed_mid_session_reattaches_and_carries_on() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, "sleep 8", CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let mut first = f.spawn_run(&model);
    let handoff = wait_for_sleep(&f, &mut first, "worker");
    let host_pid = handoff["pid"].as_u64().unwrap();
    assert!(running(host_pid));
    assert!(handoff["host_session_id"].is_string(), "the host's session id is known before it starts");

    // A second supervisor for the same checkout is refused while the first lives.
    let (code, out) = f.run(&model, "120s");
    assert_eq!(code, 1, "{out:#}");
    assert!(out.as_str().unwrap_or_default().contains("another supervisor"), "{out:#}");

    // kill -9 the supervisor: the session it started keeps running.
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    assert!(running(host_pid), "the host session outlives its supervisor");

    let (code, report) = f.run(&model, "120s");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["final_state"], "done", "{report:#}");
    let worker = &f.attempts()[0];
    assert_eq!(report["reattached"], serde_json::json!([worker["id"]]), "{report:#}");
    assert_eq!(report["reconciled"], serde_json::json!([]));
    assert_eq!(report["sessions"][0]["reattached"], true);
    assert_eq!(report["sessions"][0]["ended_because"], "completed");
    assert_eq!(worker["handoff"]["reattached_at"].as_array().unwrap().len(), 1);
    // Exactly as if the supervisor had never died: one worker session, no retry.
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G7"]);
    assert_eq!(worker_sessions(&model), 1, "no second session started for the attempt");
    assert_eq!(f.attempts().iter().filter(|a| a["role"] == "worker").count(), 1);
    let tree = f.task()["current_tree"].as_str().unwrap().to_string();
    assert!(git(f.repo.path(), &["show", &format!("{tree}:calc.py")]).contains("a + b"));
    f.assert_every_attempt_ended();
}

#[test]
fn a_verifier_session_is_reattached_too() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let mut slow_verifier = verifier_pass();
    slow_verifier.insert(1, "sleep 8".into());
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, CHECK_FIXED, CLAIM])],
        verifier: vec![slow_verifier],
        on_block: vec![],
    });
    let mut first = f.spawn_run(&model);
    wait_for_sleep(&f, &mut first, "verifier");
    let verifier = f.attempts().into_iter().find(|a| a["role"] == "verifier").unwrap();
    assert_eq!(verifier["status"], "running");
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    assert!(running(verifier["handoff"]["pid"].as_u64().unwrap()));

    let (code, report) = f.run(&model, "120s");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["reattached"], serde_json::json!([verifier["id"]]));
    assert_eq!(report["sessions"][0]["role"], "verifier");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G7"]);
    assert_eq!(f.attempts().iter().filter(|a| a["role"] == "verifier").count(), 1, "one verifier session");
    f.assert_every_attempt_ended();
}

#[test]
fn a_session_that_died_with_its_supervisor_is_reconciled_and_retried() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, "sleep 30", CHECK_FIXED, CLAIM]), steps(&[FIX, CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let mut first = f.spawn_run(&model);
    let handoff = wait_for_sleep(&f, &mut first, "worker");
    kill(&first.id().to_string(), "KILL");
    let _ = first.wait();
    // The session dies too, while no supervisor is watching.
    kill(&format!("-{}", handoff["pgid"]), "KILL");
    wait_until("the host is gone", Duration::from_secs(10), || !running(handoff["pid"].as_u64().unwrap()));

    let (code, report) = f.run(&model, "120s");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["final_state"], "done", "{report:#}");
    let first_attempt = f.attempts()[0].clone();
    assert_eq!(report["reconciled"], serde_json::json!([first_attempt["id"]]));
    assert_eq!(first_attempt["status"], "failed");
    assert_eq!(first_attempt["end"]["reason"], "crash");
    assert_eq!(first_attempt["end"]["synthetic"], true, "a synthetic failure report");
    assert!(first_attempt["end"]["detail"].as_str().unwrap().contains("was gone when a restarted supervisor found it"));
    // The orphan's unfinished work was salvaged before its worktree went.
    assert_eq!(report["exports"][0]["reference"], "refs/heads/interlock/wip/fix-add");
    assert_eq!(f.signals(), ["G1", "G2", "R3", "G2", "G3", "G4", "G7"]);
    let (_, events) = f.interlock(&["task", "events", "fix-add"], None);
    let synthetic = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["attempt_id"] == first_attempt["id"] && e["type"] == "attempt.failed")
        .expect("the synthetic failure is an event");
    assert_eq!(synthetic["outcome"]["synthetic"], true);
    f.assert_every_attempt_ended();
}

#[test]
fn sigint_cancels_the_session_and_the_exported_work_resumes() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let model = FakeModel::start(Script {
        // The first worker fixes add() and is interrupted; the second resumes
        // from the export, so it only has to check and claim.
        worker: vec![steps(&[FIX, "sleep 60", CHECK_FIXED, CLAIM]), steps(&[CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let head = git(f.repo.path(), &["rev-parse", "HEAD"]);
    let mut run = f.spawn_run(&model);
    let handoff = wait_for_sleep(&f, &mut run, "worker");
    let started = Instant::now();
    kill(&run.id().to_string(), "INT");
    // An impatient second Ctrl-C must not orphan the host.
    std::thread::sleep(Duration::from_millis(200));
    let _ = Command::new("kill").args(["-s", "INT", "--", &run.id().to_string()]).status();
    let (code, report) = finish(run);
    assert_eq!(code, 6, "an interrupted run has its own exit status: {report:#}");
    assert!(started.elapsed() < Duration::from_secs(15), "the run stopped promptly");
    assert_eq!(report["interrupted"], true);
    assert_eq!(report["final_state"], "ready", "R3 leaves the task resumable");
    assert_eq!(report["sessions"][0]["ended_because"], "cancelled");
    assert!(!running(handoff["pid"].as_u64().unwrap()), "the session's process group was stopped");
    let attempt = &f.attempts()[0];
    assert_eq!((attempt["status"].as_str(), attempt["end"]["reason"].as_str()), (Some("cancelled"), Some("cancelled")));
    assert_eq!(f.signals(), ["G1", "G2", "R3"]);

    // Pausing safely: the interrupted work is a wip: commit on interlock's own
    // ref, with a resume note; the user's branch and files are untouched.
    let export = &report["sessions"][0]["export"];
    assert_eq!(export["reference"], "refs/heads/interlock/wip/fix-add");
    let wip = git(f.repo.path(), &["show", "interlock/wip/fix-add:calc.py"]);
    assert!(wip.contains("a + b"), "{wip}");
    let message = git(f.repo.path(), &["log", "-1", "--format=%B", "interlock/wip/fix-add"]);
    assert!(message.starts_with("wip: fix-add"), "{message}");
    assert!(message.contains("interlock task resume fix-add"), "{message}");
    assert_eq!(git(f.repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(f.repo.path(), &["branch", "--show-current"]), "main");
    assert!(std::fs::read_to_string(f.repo.path().join("calc.py")).unwrap().contains("a - b"));

    // Work that does not descend from this task's base is not this task's to resume.
    let empty = git(f.repo.path(), &["hash-object", "-t", "tree", "/dev/null"]);
    let stranger = git(f.repo.path(), &["commit-tree", &empty, "-m", "unrelated"]);
    let (code, refused) = f.interlock(&["task", "resume", "fix-add", "--from", &stranger], None);
    assert_ne!(code, 0);
    assert!(refused.to_string().contains("does not descend from task fix-add's base commit"), "{refused:#}");
    let (code, resumed) = f.interlock(&["task", "resume", "fix-add"], None);
    assert_eq!(code, 0, "{resumed:#}");
    assert_eq!(resumed["resumes_from"], export["commit"]);
    let (_, events) = f.interlock(&["task", "events", "fix-add"], None);
    let audit =
        events.as_array().unwrap().iter().find(|e| e["type"] == "task.resumed").expect("the resume is on record");
    assert_eq!(audit["outcome"]["commit"], export["commit"]);
    let (code, report) = f.run(&model, "120s");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["final_state"], "done");
    assert_eq!(f.signals(), ["G1", "G2", "R3", "G2", "G3", "G4", "G7"]);
    let resumed_brief = model.requests().iter().any(|r| {
        r["role"] == "worker" && r["last"]["content"].as_str().is_some_and(|c| c.contains("exported work in progress"))
    });
    assert!(resumed_brief, "the second worker was told it starts from exported work");
    // It only checked and claimed: the fix it submitted came from the export.
    assert_eq!(worker_sessions(&model), 2);
    let tree = f.task()["current_tree"].as_str().unwrap().to_string();
    assert_eq!(tree, export["tree"].as_str().unwrap(), "the accepted output is the exported tree");
    f.assert_every_attempt_ended();
}

#[test]
fn sigterm_stops_a_run_the_same_way() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let model = FakeModel::start(Script { worker: vec![steps(&["sleep 60"])], verifier: vec![], on_block: vec![] });
    let mut run = f.spawn_run(&model);
    let handoff = wait_for_sleep(&f, &mut run, "worker");
    kill(&run.id().to_string(), "TERM");
    let out = run.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(6));
    let said: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(said["interrupted"], "SIGTERM");
    assert_eq!(said["resume"], "interlock run fix-add --host copilot");
    assert!(!running(handoff["pid"].as_u64().unwrap()));
    assert_eq!(f.signals(), ["G1", "G2", "R3"]);
    f.assert_every_attempt_ended();
}

#[test]
fn task_cancel_from_another_terminal_stops_the_running_session() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, "sleep 60", CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let mut run = f.spawn_run(&model);
    let handoff = wait_for_sleep(&f, &mut run, "worker");

    // Export by hand while the session runs: the live worktree's tree, as a wip: commit.
    let (code, export) = f.interlock(&["task", "export", "fix-add"], None);
    assert_eq!(code, 0, "{export:#}");
    assert!(export["source"].as_str().unwrap().contains("fix-add-worker-1"), "{export:#}");
    assert!(
        git(f.repo.path(), &["show", &format!("{}:calc.py", export["commit"].as_str().unwrap())]).contains("a + b")
    );
    let note = std::fs::read_to_string(export["note"].as_str().unwrap()).unwrap();
    assert!(note.contains("# Resume note for fix-add") && note.contains("## Criteria"), "{note}");

    let started = Instant::now();
    let (code, out) = f.interlock(&["task", "cancel", "fix-add", "--reason", "operator stopped it"], None);
    assert_eq!(code, 0, "{out:#}");
    let (code, report) = finish(run);
    assert!(started.elapsed() < Duration::from_secs(15), "the supervisor noticed promptly");
    assert_eq!(code, 5, "{report:#}");
    assert_eq!(report["final_state"], "cancelled");
    assert_eq!(report["stopped_because"], "cancelled");
    assert_eq!(report["interrupted"], false);
    assert_eq!(report["sessions"][0]["ended_because"], "cancelled");
    assert!(!running(handoff["pid"].as_u64().unwrap()), "the session was stopped");
    let attempt = &f.attempts()[0];
    assert_eq!(attempt["status"], "cancelled");
    assert_eq!(attempt["end"]["reason"], "cancelled");
    assert!(attempt["spent"]["wall_ms"].as_u64().unwrap() > 0);
    assert_eq!(f.signals(), ["G1", "G2", "cancel"]);
    f.assert_every_attempt_ended();
}

#[test]
fn a_session_timeout_retries_until_the_attempts_are_spent() {
    let Some(f) = Fixture::new("max_attempts = 2") else { return };
    let model = FakeModel::start(Script { worker: vec![steps(&["sleep 60"])], verifier: vec![], on_block: vec![] });
    let (code, report) = f.run(&model, "8s");
    assert_eq!(code, 5, "{report:#}");
    assert_eq!(report["final_state"], "failed");
    assert_eq!(f.signals(), ["G1", "G2", "R3", "G2", "fail"]);
    let why = report["stopped_because"].as_str().unwrap();
    assert!(why.contains("all 2 attempts are used"), "{why}");
    for (s, a) in report["sessions"].as_array().unwrap().iter().zip(f.attempts()) {
        assert_eq!(s["ended_because"], "timeout", "{s:#}");
        assert_eq!(a["end"]["reason"], "timeout");
        assert!(a["spent"]["wall_ms"].as_u64().unwrap() >= 8_000);
    }
    f.assert_every_attempt_ended();
}

#[test]
fn a_spent_wall_clock_budget_fails_the_task() {
    let Some(f) = Fixture::new("max_attempts = 3\nmax_wall_secs = 6") else { return };
    let model = FakeModel::start(Script { worker: vec![steps(&["sleep 60"])], verifier: vec![], on_block: vec![] });
    let started = Instant::now();
    let (code, report) = f.run(&model, "120s");
    assert!(started.elapsed() < Duration::from_secs(60), "the budget, not the 120s timeout, stopped the session");
    assert_eq!(code, 5, "{report:#}");
    assert_eq!(report["final_state"], "failed");
    assert_eq!(report["sessions"][0]["ended_because"], "budget_exhausted");
    let why = report["stopped_because"].as_str().unwrap();
    assert!(why.contains("budget exhausted") && why.contains("wall-clock budget of 6s"), "{why}");
    assert_eq!(f.signals(), ["G1", "G2", "fail"]);
    assert!(report["spent"]["wall_ms"].as_u64().unwrap() >= 6_000);
    f.assert_every_attempt_ended();
}

#[test]
fn a_host_version_that_differs_from_its_pin_blocks_the_run() {
    let Some(f) = Fixture::new("max_attempts = 3") else { return };
    let (_, inspect) = f.interlock(&["host", "inspect", "--host", "copilot"], None);
    let installed = inspect[0]["version"].as_str().unwrap().to_string();
    assert_eq!(inspect[0]["pin"]["status"], "unpinned");

    std::fs::write(f.repo.path().join(".interlock/config.toml"), "[pins]\ncopilot = \"0.0.1\"\n").unwrap();
    let (_, inspect) = f.interlock(&["host", "inspect", "--host", "copilot"], None);
    assert_eq!(inspect[0]["pin"]["status"], "mismatch");
    let model = FakeModel::start(Script::default());
    let (code, report) = f.run(&model, "120s");
    assert_eq!(code, 5, "{report:#}");
    assert_eq!(report["final_state"], "blocked");
    let why = report["stopped_because"].as_str().unwrap();
    assert!(why.contains(&format!("copilot {installed} is installed, but .interlock/config.toml pins 0.0.1")), "{why}");
    assert!(model.requests().is_empty(), "no session started");
    assert!(f.attempts().is_empty());

    // Pinning the installed version lets the same task proceed.
    std::fs::write(f.repo.path().join(".interlock/config.toml"), format!("[pins]\ncopilot = \"{installed}\"\n"))
        .unwrap();
    let (_, inspect) = f.interlock(&["host", "inspect", "--host", "copilot"], None);
    assert_eq!(inspect[0]["pin"]["status"], "match");
    f.interlock(&["task", "unblock", "fix-add"], None);
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let (code, report) = f.run(&model, "120s");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["pin"]["status"], "match");
    f.assert_every_attempt_ended();
}

/// A stand-in for Claude Code that reads its prompt, changes nothing, and
/// reports a cost, as `claude -p --output-format stream-json` does.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "2.1.289 (Claude Code)"; exit 0 ;;
  --help) printf '  -p, --print\n  --output-format <format> (choices: "text", "json", "stream-json")\n  --allowedTools, --allowed-tools <tools...>\n  --disallowedTools, --disallowed-tools <tools...>\n  --model <model>\n  --agents <json-or-file>\n  --plugin-dir <path>\n'; exit 0 ;;
esac
echo "$@" >> "$(dirname "$0")/args.log"
cat > /dev/null
echo '{"type":"system","subtype":"init","session_id":"s"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Nothing to change.","num_turns":1,"total_cost_usd":0.6,"permission_denials":[],"session_id":"s"}'
"#;

#[test]
fn a_spent_cost_budget_fails_the_task() {
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, FAKE_CLAUDE).unwrap();
    Command::new("chmod").arg("+x").arg(&claude).status().unwrap();
    let f = Fixture::with("max_attempts = 3\nmax_cost_usd = 0.5", claude);
    let args = Fixture::run_args("claude-code", "60s");
    let (code, report) = f.interlock(&args.iter().map(String::as_str).collect::<Vec<_>>(), None);
    assert_eq!(code, 5, "{report:#}");
    assert_eq!(report["final_state"], "failed", "{report:#}");
    let why = report["stopped_because"].as_str().unwrap();
    assert!(why.contains("cost budget of $0.50 is spent ($0.6000 used)"), "{why}");
    assert_eq!(report["spent"]["cost_usd"], 0.6);
    assert_eq!(f.attempts()[0]["spent"]["cost_usd"], 0.6, "spending is recorded per attempt");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "fail"]);
    // Claude Code also enforces what is left of the dollar budget itself.
    let logged = std::fs::read_to_string(bin.path().join("args.log")).unwrap();
    assert!(logged.contains("--max-budget-usd 0.5"), "{logged}");
    assert!(logged.contains("--session-id"), "{logged}");
    f.assert_every_attempt_ended();
}
