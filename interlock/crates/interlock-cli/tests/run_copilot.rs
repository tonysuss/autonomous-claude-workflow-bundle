//! Whole `interlock run` passes on the real Copilot CLI, in offline mode
//! against a scripted model. They need a Copilot CLI binary: set
//! INTERLOCK_COPILOT_BIN or put `copilot` on PATH. Without one they skip, unless
//! INTERLOCK_REQUIRE_HOSTS=1, which makes a missing host a failure.

mod fake_model;
mod hosts;

use std::path::{Path, PathBuf};
use std::process::Command;

use fake_model::{FakeModel, Script};
use serde_json::Value;

/// The fix, as one shell command that edits in place on Linux and macOS alike.
const FIX: &str = "perl -pi -e 's/a - b/a + b/' calc.py";
const CLAIM: &str = "interlock claim add --criterion fixed --strength tested --tree auto --ref 'sh check.sh'";
const CHECK_FIXED: &str = "interlock check run --criterion fixed";

const TASK: &str = r#"
id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add() returns the difference instead of the sum"

[budget]
max_attempts = 3

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
"#;

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

struct Fixture {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    copilot: PathBuf,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        // Skips without Copilot, unless INTERLOCK_REQUIRE_HOSTS=1.
        let copilot = hosts::copilot()?;
        Some(Fixture::with_task(TASK, copilot))
    }

    /// For runs that stop before any session starts, so no host is needed.
    fn without_host(task: &str) -> Fixture {
        Fixture::with_task(task, copilot().unwrap_or_else(|| PathBuf::from("copilot")))
    }

    fn with_task(task: &str, copilot: PathBuf) -> Fixture {
        let repo = tempfile::tempdir().unwrap();
        let p = repo.path();
        std::fs::write(p.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(p.join("check.sh"), "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'\n").unwrap();
        std::fs::write(p.join(".gitignore"), "__pycache__/\n").unwrap();
        std::fs::write(p.join("task.toml"), task).unwrap();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["add", "-A"]);
        git(p, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let f = Fixture { repo, home: tempfile::tempdir().unwrap(), copilot };
        f.interlock(&["init"], None);
        f.interlock(&["task", "create", "task.toml"], None);
        f
    }

    fn interlock(&self, args: &[&str], model: Option<&FakeModel>) -> (i32, Value) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_interlock"));
        cmd.args(args)
            .current_dir(self.repo.path())
            .env("INTERLOCK_DB", self.repo.path().join(".interlock/state.db"))
            .env("INTERLOCK_COPILOT_BIN", &self.copilot)
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
        let out = cmd.output().unwrap();
        let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            Value::String(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
        });
        (out.status.code().unwrap_or(-1), json)
    }

    fn run(&self, model: &FakeModel) -> (i32, Value) {
        // Copilot processes share a package cache; one session at a time keeps the tests steady.
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        self.interlock(&["run", "fix-add", "--host", "copilot", "--timeout", "120s"], Some(model))
    }

    fn signals(&self) -> Vec<String> {
        let (_, log) = self.interlock(&["task", "log", "fix-add"], None);
        log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap().to_string()).collect()
    }
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

#[test]
fn a_bug_fix_runs_to_done_on_copilot() {
    let Some(f) = Fixture::new() else { return };
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let (code, report) = f.run(&model);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["final_state"], "done", "{report:#}");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G7"]);

    // The fix lives in the recorded output tree; the user's branch is untouched.
    let (_, task) = f.interlock(&["task", "show", "fix-add"], None);
    let tree = task["current_tree"].as_str().unwrap();
    assert!(git(f.repo.path(), &["show", &format!("{tree}:calc.py")]).contains("a + b"));
    assert!(std::fs::read_to_string(f.repo.path().join("calc.py")).unwrap().contains("a - b"));
    assert!(git(f.repo.path(), &["status", "--porcelain"]).lines().all(|l| !l.contains("calc.py")));
}

#[test]
fn the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted() {
    let Some(f) = Fixture::new() else { return };
    let grant_self =
        "interlock grant create --principal me --tasks '*' --classes landing --landing operator --origin self";
    let model = FakeModel::start(Script {
        // The worker fixes the bug, tries to push and to grant itself landing,
        // then tries to stop without recording a claim.
        worker: vec![steps(&[FIX, "git push origin main", grant_self])],
        verifier: vec![verifier_pass()],
        on_block: steps(&[CHECK_FIXED, CLAIM]),
    });
    let (code, report) = f.run(&model);
    assert_eq!(code, 0, "{report:#}");
    let worker = &report["sessions"][0];
    assert!(worker["denials"].as_u64().unwrap() >= 2, "push and self-grant were denied: {worker:#}");
    let (_, grants) = f.interlock(&["grant", "list"], None);
    assert_eq!(grants.as_array().unwrap().len(), 0, "no grant was created");
    // The claim was recorded only after the stop guard held the worker.
    let held = model.requests().iter().any(|r| {
        r["last"]["role"] == "user" && r["last"]["content"].as_str().is_some_and(|c| c.contains("Before you finish"))
    });
    assert!(held, "the stop guard's reason reached the model");
    assert_eq!(report["final_state"], "done");
}

/// The custom agent Copilot says a session ran as, from the session's own
/// event log under COPILOT_HOME (its `-p` output does not carry it).
fn agent_the_session_ran(home: &Path, session_id: &str) -> Option<String> {
    let log = std::fs::read_to_string(home.join("session-state").join(session_id).join("events.jsonl")).ok()?;
    log.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "subagent.selected")
        .and_then(|e| e["data"]["agentName"].as_str().map(String::from))
}

#[test]
fn each_session_runs_as_interlocks_agent_for_its_role_and_the_tool_filters_hold() {
    let Some(f) = Fixture::new() else { return };
    let grant_self =
        "interlock grant create --principal me --tasks '*' --classes landing --landing operator --origin self";
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, "git push origin main", grant_self, CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let (code, report) = f.run(&model);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["final_state"], "done", "{report:#}");

    // Copilot ran each session as the agent interlock wrote for its role.
    let sessions = report["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{report:#}");
    for s in sessions {
        let id = s["host_session_id"].as_str().unwrap();
        let expected = format!("interlock-hooks:interlock-{}", s["role"].as_str().unwrap());
        assert_eq!(agent_the_session_ran(f.home.path(), id), Some(expected), "{s:#}");
    }
    // The role's instructions reached the model as the agent's, in the system prompt, not the prompt.
    let requests = model.requests();
    for (role, lead) in [("worker", "You are the worker"), ("verifier", "You are the independent verifier")] {
        let mine: Vec<&Value> = requests.iter().filter(|r| r["role"] == role).collect();
        assert!(!mine.is_empty(), "no {role} requests");
        for r in &mine {
            assert!(r["agent"].as_str().is_some_and(|a| a.contains(lead)), "{role}: {r:#}");
        }
        let first = mine.iter().find(|r| r["tools_done"] == 0).unwrap();
        assert!(!first["last"]["content"].as_str().unwrap_or_default().contains(lead), "{first:#}");
    }

    // The agents see only the tools their grants allow: no edit tools for the
    // verifier, and no sub-agents for either.
    let offered = |role: &str| -> Vec<String> {
        let r = requests.iter().find(|r| r["role"] == role).unwrap();
        r["offered"].as_array().unwrap().iter().filter_map(|t| t.as_str().map(String::from)).collect()
    };
    let (worker, verifier) = (offered("worker"), offered("verifier"));
    for t in ["bash", "view", "edit", "create"] {
        assert!(worker.contains(&t.to_string()), "the worker lacks {t}: {worker:?}");
    }
    assert!(verifier.contains(&"bash".to_string()), "{verifier:?}");
    for t in ["edit", "create"] {
        assert!(!verifier.contains(&t.to_string()), "the verifier was offered {t}: {verifier:?}");
    }
    assert!(!worker.iter().chain(&verifier).any(|t| t == "task"), "sub-agents offered: {worker:?} {verifier:?}");

    // The worker's push and self-grant are still refused inside its session.
    assert!(sessions[0]["denials"].as_u64().unwrap() >= 2, "{:#}", sessions[0]);
    let (_, grants) = f.interlock(&["grant", "list"], None);
    assert_eq!(grants.as_array().unwrap().len(), 0, "no grant was created");
}

/// interlock's hook refuses a push before Copilot's own rules are consulted,
/// so the run above cannot show which layer refused it. This session runs the
/// adapter's plan for a worker, as its agent, under a plugin with no hooks:
/// what is refused, Copilot's --deny-tool rules refused.
#[test]
fn copilot_keeps_the_deny_rules_for_a_session_run_as_an_agent() {
    let Some(copilot) = hosts::copilot() else { return };
    use interlock_adapter::{Copilot, Host, Probe, SessionSpec};
    let dir = tempfile::tempdir().unwrap();
    let (work, plugin, home) = (dir.path().join("work"), dir.path().join("plugin"), dir.path().join("home"));
    for d in [&work, &plugin.join(".claude-plugin"), &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(plugin.join(".claude-plugin/plugin.json"), r#"{"name": "interlock-hooks", "version": "0"}"#)
        .unwrap();
    let model = FakeModel::start(Script {
        worker: vec![steps(&["echo agent-probe", "git push origin main", "git commit --allow-empty -m wip"])],
        ..Script::default()
    });
    let spec = SessionSpec {
        workdir: work.clone(),
        prompt: "AGENT-RULES-PROBE: run the three commands.".into(),
        append_system: Some("You are the worker on an interlock task.".into()),
        agent: Some("interlock-worker".into()),
        tools: interlock_schema::ToolPolicy {
            allow: vec!["read".into(), "edit".into(), "shell".into()],
            deny: vec!["shell:git push".into(), "shell:git commit".into()],
        },
        model: None,
        max_turns: None,
        plugin_dir: Some(plugin.clone()),
        extra_plugin_dirs: vec![],
        env: [
            ("COPILOT_HOME", home.display().to_string()),
            ("COPILOT_OFFLINE", "true".into()),
            ("COPILOT_MODEL", "gpt-4.1".into()),
            ("COPILOT_PROVIDER_BASE_URL", model.base_url()),
            ("NO_PROXY", "127.0.0.1,localhost".into()),
            ("no_proxy", "127.0.0.1,localhost".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        timeout: std::time::Duration::from_secs(120),
        transcript: dir.path().join("session.jsonl"),
        session_id: Some("5d1c3a90-0b6e-4f1e-9a51-2a8c0f6e7b11".into()),
        max_cost_usd: None,
        effort: None,
        clear_env: false,
    };
    let probe = Probe { overrides: vec![("copilot".into(), copilot)], ..Probe::default() };
    let outcome = Copilot.run_session(&probe, &spec, &std::sync::atomic::AtomicBool::new(false));
    assert!(!outcome.summary.is_error, "{outcome:#?}");

    let events: Vec<Value> = std::fs::read_to_string(&spec.transcript)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let results = hosts::copilot_tool_results(&events);
    assert_eq!(results.len(), 3, "{results:#?}");
    assert!(results[0].1 && results[0].2.contains("agent-probe"), "{results:#?}");
    for (call, rule) in [(1, "shell(git push:*)"), (2, "shell(git commit:*)")] {
        let (_, ok, text) = &results[call];
        assert!(!ok && text.contains(&format!("denied due to the following rules: `{rule}`")), "{results:#?}");
    }
    assert_eq!(
        agent_the_session_ran(&home, spec.session_id.as_deref().unwrap()).as_deref(),
        Some("interlock-hooks:interlock-worker")
    );
    let profile = std::fs::read_to_string(plugin.join("agents/interlock-worker.agent.md")).unwrap();
    assert!(profile.contains("You are the worker on an interlock task."), "{profile}");
}

#[test]
fn a_failed_verification_sends_the_work_back_and_the_rework_is_verified() {
    let Some(f) = Fixture::new() else { return };
    let verifier_fail = steps(&[
        "interlock check run --criterion fixed",
        "interlock assess add --criterion fixed --strength failed --ref 'sh check.sh' --note 'add(2, 3) returned -1'",
        "interlock assess add --criterion verified --strength failed --ref 'sh check.sh'",
    ]);
    let model = FakeModel::start(Script {
        // The first worker claims a pass without fixing anything.
        worker: vec![steps(&[CLAIM]), steps(&[FIX, CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_fail, verifier_pass()],
        on_block: vec![],
    });
    let (code, report) = f.run(&model);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "R1", "G2", "G3", "G4", "G7"]);
    assert_eq!(report["sessions"].as_array().unwrap().len(), 4);
    // The rework brief carried the verifier's finding to the second worker.
    let briefed = model.requests().iter().any(|r| {
        r["role"] == "worker" && r["last"]["content"].as_str().is_some_and(|c| c.contains("add(2, 3) returned -1"))
    });
    assert!(briefed, "the fix brief included the failed assessment's note");
}

fn blocked_before_any_session(task: &str) -> (Value, Vec<Value>) {
    let f = Fixture::without_host(task);
    let model = FakeModel::start(Script::default());
    let (code, report) = f.run(&model);
    assert_eq!(code, 5, "{report:#}");
    assert_eq!(report["final_state"], "blocked", "{report:#}");
    assert_eq!(report["sessions"].as_array().unwrap().len(), 0, "no session started: {report:#}");
    (report, model.requests())
}

#[test]
fn a_regression_check_that_tests_nothing_blocks_the_task_before_any_worker() {
    let task = TASK.replace(
        "[[criterion]]\nid = \"verified\"",
        "[[criterion]]\nid = \"unit\"\nstatement = \"unit tests pass\"\ncheck = \"python3 -m unittest -q\"\n\
         min_strength = \"tested\"\nproducer = \"self\"\nbaseline = \"passes\"\n\n[[criterion]]\nid = \"verified\"",
    );
    let (report, requests) = blocked_before_any_session(&task);
    let why = report["stopped_because"].as_str().unwrap();
    assert!(why.contains("unit: the check ran nothing on the input snapshot (unittest ran 0 tests)"), "{why}");
    assert!(requests.is_empty(), "no model call was spent");
}

#[test]
fn a_reproduction_that_already_passes_blocks_the_task() {
    let task = TASK.replacen("check = \"sh check.sh\"", "check = \"true\"", 1);
    let (report, _) = blocked_before_any_session(&task);
    let why = report["stopped_because"].as_str().unwrap();
    assert!(why.contains("fixed: the check already passes on the input snapshot"), "{why}");
}

#[test]
fn a_worker_that_rewrites_the_check_is_rejected_and_the_next_must_really_fix_it() {
    let Some(f) = Fixture::new() else { return };
    let cheat = "printf 'exit 0\\n' > check.sh";
    let model = FakeModel::start(Script {
        // The first worker makes the check pass by rewriting it, not by fixing add().
        worker: vec![steps(&[cheat, CHECK_FIXED, CLAIM]), steps(&[FIX, CHECK_FIXED, CLAIM])],
        verifier: vec![verifier_pass()],
        on_block: vec![],
    });
    let (code, report) = f.run(&model);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(f.signals(), ["G1", "G2", "R3", "G2", "G3", "G4", "G7"]);
    let (_, task) = f.interlock(&["task", "show", "fix-add"], None);
    let tree = task["current_tree"].as_str().unwrap();
    assert!(
        git(f.repo.path(), &["show", &format!("{tree}:calc.py")]).contains("a + b"),
        "the accepted output fixes add()"
    );
    assert!(git(f.repo.path(), &["show", &format!("{tree}:check.sh")]).contains("assert"), "and keeps the real check");
    let told = model.requests().iter().any(|r| {
        r["role"] == "worker"
            && r["last"]["content"].as_str().is_some_and(|c| c.contains("check.sh (a file the task's checks run)"))
    });
    assert!(told, "the second worker's brief said why the first result was rejected");
}
