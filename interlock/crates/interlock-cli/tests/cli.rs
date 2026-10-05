//! End-to-end runs of the `interlock` binary against a store on disk.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

const TREE_A: &str = "aaaaaaa1111111111111111111111111111111111";

const TASK: &str = r#"
id = "export-retry"
repository = "."
workflow = "bug-fix"
intent = "Fix duplicate rows when an export retries"
environment = "linux-x86_64"

[budget]
max_attempts = 3

[scope]
paths = ["src/export/**"]

[[criterion]]
id = "repro"
statement = "Retrying an export produces no duplicate rows"
check = "checks/export-retry.sh"
min_strength = "observed"
producer = "independent"

[[criterion]]
id = "regression"
statement = "The regression suite passes"
min_strength = "tested"
producer = "self"
"#;

// Declared so these tests do not depend on which hosts are installed.
const CAPS: &str = "session_start,session_collect,session_cancel,tool_restriction,custom_agents";

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let env = Env { dir: tempfile::tempdir().unwrap() };
        std::fs::write(env.path("task.toml"), TASK).unwrap();
        env.ok(&["init"]);
        env
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_interlock"));
        c.args(args)
            .env("INTERLOCK_DB", self.path("state.db"))
            .env_remove("INTERLOCK_FAULT")
            .env_remove("INTERLOCK_TOKEN")
            .env_remove("INTERLOCK_ATTEMPT")
            .current_dir(self.dir.path());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }

    fn start(&self, role: &str) -> (String, String, u64) {
        let v = self.ok(&[
            "attempt",
            "start",
            "export-retry",
            "--role",
            role,
            "--host",
            "test",
            "--mode",
            "headless",
            "--capabilities",
            CAPS,
        ]);
        assert_eq!(v["started"], "yes", "{v}");
        (
            v["attempt"]["id"].as_str().unwrap().into(),
            v["token"].as_str().unwrap().into(),
            v["attempt"]["epoch"].as_u64().unwrap(),
        )
    }

    fn state(&self) -> String {
        self.ok(&["status", "export-retry"])["task"]["state"].as_str().unwrap().into()
    }
}

fn to_running(env: &Env) -> (String, String, u64) {
    env.ok(&["task", "create", "task.toml"]);
    env.ok(&["task", "ready", "export-retry", "--base", "e43c7ee"]);
    env.start("worker")
}

fn submit(env: &Env, attempt: &str, token: &str, epoch: u64, event: &str) -> Output {
    env.run(&[
        "result",
        "submit",
        "--attempt",
        attempt,
        "--token",
        token,
        "--epoch",
        &epoch.to_string(),
        "--tree",
        TREE_A,
        "--summary",
        "made retries idempotent",
        "--event-id",
        event,
    ])
}

#[test]
fn a_bug_fix_runs_from_create_to_done() {
    let env = Env::new();
    let (w, wt, epoch) = to_running(&env);
    assert_eq!(epoch, 1);
    assert!(submit(&env, &w, &wt, epoch, "evt-1").status.success());
    env.ok(&[
        "claim",
        "add",
        "--attempt",
        &w,
        "--token",
        &wt,
        "--criterion",
        "regression",
        "--strength",
        "tested",
        "--tree",
        TREE_A,
    ]);
    assert_eq!(env.state(), "awaiting_verification");

    // The worker's claim does not satisfy the independent criterion.
    let status = env.ok(&["status", "export-retry"]);
    let next = &status["next_moves"][0];
    assert_eq!((next["signal"].as_str(), next["ready"].as_bool()), (Some("G4"), Some(false)));

    let (v, vt, _) = env.start("verifier");
    env.ok(&[
        "assess",
        "add",
        "--attempt",
        &v,
        "--token",
        &vt,
        "--criterion",
        "repro",
        "--strength",
        "observed",
        "--tree",
        TREE_A,
    ]);
    let advanced = env.ok(&["advance", "export-retry"]);
    let signals: Vec<&str> =
        advanced["moves"].as_array().unwrap().iter().map(|m| m["signal"].as_str().unwrap()).collect();
    assert_eq!(signals, vec!["G4", "G7"]);
    assert_eq!(env.state(), "done");

    let log = env.ok(&["task", "log", "export-retry"]);
    let path: Vec<&str> = log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap()).collect();
    assert_eq!(path, vec!["G1", "G2", "G3", "G4", "G7"]);
}

#[test]
fn refusals_exit_2_with_a_json_reason() {
    let env = Env::new();
    let (w, wt, _) = to_running(&env);
    submit(&env, &w, &wt, 1, "evt-1");
    let out = env.run(&[
        "assess",
        "add",
        "--attempt",
        &w,
        "--token",
        &wt,
        "--criterion",
        "repro",
        "--strength",
        "observed",
        "--tree",
        TREE_A,
    ]);
    assert_eq!(out.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["refusal"]["code"], "wrong_role");

    let out = env.run(&[
        "claim",
        "add",
        "--attempt",
        &w,
        "--token",
        "wrong",
        "--criterion",
        "repro",
        "--strength",
        "observed",
        "--tree",
        TREE_A,
    ]);
    assert_eq!(out.status.code(), Some(4), "bad token");
}

#[test]
fn a_crash_mid_apply_loses_nothing_and_the_replay_applies_once() {
    let env = Env::new();
    let (w, wt, epoch) = to_running(&env);
    // The process dies after writing the result, before the commit.
    let out = env
        .cmd(&[
            "result",
            "submit",
            "--attempt",
            &w,
            "--token",
            &wt,
            "--epoch",
            "1",
            "--tree",
            TREE_A,
            "--event-id",
            "evt-crash",
        ])
        .env("INTERLOCK_FAULT", "crash_before_commit")
        .output()
        .unwrap();
    assert!(!out.status.success(), "the process aborted");
    // Restarted: the actionable work is intact and nothing half-applied.
    assert_eq!(env.state(), "running");
    let status = env.ok(&["status", "export-retry"]);
    assert_eq!(status["task"]["current_attempt"].as_str(), Some(w.as_str()));
    // The sender redelivers the same event.
    let first: Value = serde_json::from_slice(&submit(&env, &w, &wt, epoch, "evt-crash").stdout).unwrap();
    assert_eq!(first["duplicate"], false);
    assert_eq!(env.state(), "awaiting_verification");
    let again: Value = serde_json::from_slice(&submit(&env, &w, &wt, epoch, "evt-crash").stdout).unwrap();
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["outcome"]["result"]["id"], first["outcome"]["result"]["id"]);
}

#[test]
fn policy_check_pauses_irreversible_actions_and_respects_grants() {
    let env = Env::new();
    let (w, _, _) = to_running(&env);
    let check = |line: &str| env.ok(&["policy", "check", "--attempt", &w, "--shell", line]);
    assert_eq!(check("cargo test")["decision"], "allow");
    assert_eq!(check("git push origin main --force")["decision"], "ask");
    assert_eq!(check("git push origin fix")["decision"], "ask", "external actions need a grant");
    assert_eq!(check("git push origin fix")["class"], "external_reversible");
}

#[test]
fn grants_reject_the_irreversible_class() {
    let env = Env::new();
    let out = env.run(&[
        "grant",
        "create",
        "--principal",
        "op",
        "--tasks",
        "*",
        "--classes",
        "irreversible",
        "--origin",
        "test",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("irreversible"));
    let g = env.ok(&[
        "grant",
        "create",
        "--principal",
        "op",
        "--tasks",
        "export-retry",
        "--classes",
        "external-reversible",
        "--expires-in",
        "8h",
        "--origin",
        "going to bed, keep going",
    ]);
    assert!(g["expires_at"].is_string());
}

#[test]
fn one_grant_translates_to_both_hosts() {
    let env = Env::new();
    env.ok(&["task", "create", "task.toml"]);
    let copilot = env.ok(&["host", "tools", "export-retry", "--role", "verifier", "--host", "copilot"]);
    let claude = env.ok(&["host", "tools", "export-retry", "--role", "verifier", "--host", "claude-code"]);
    assert_eq!(copilot["host_neutral"], claude["host_neutral"]);
    assert!(copilot["host_patterns"]["deny"].as_array().unwrap().iter().any(|p| p == "write"));
    assert!(claude["host_patterns"]["deny"].as_array().unwrap().iter().any(|p| p == "Edit"));
}

#[test]
fn the_brief_is_built_from_records() {
    let env = Env::new();
    let (w, wt, epoch) = to_running(&env);
    submit(&env, &w, &wt, epoch, "evt-1");
    let (v, vt, _) = env.start("verifier");
    env.ok(&[
        "assess",
        "add",
        "--attempt",
        &v,
        "--token",
        &vt,
        "--criterion",
        "repro",
        "--strength",
        "failed",
        "--tree",
        TREE_A,
        "--note",
        "a retry after a timeout still inserts twice",
    ]);
    env.ok(&["advance", "export-retry"]);
    assert_eq!(env.state(), "ready", "R1 sent it back for rework");
    let out = env.run(&["brief", "export-retry"]);
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("## Fix first"), "{md}");
    assert!(md.contains("a retry after a timeout still inserts twice"));
    assert!(md.contains("made retries idempotent"), "previous result summary");
}

#[test]
fn schema_validate_checks_files() {
    let env = Env::new();
    let task = env.ok(&["task", "create", "task.toml"]);
    let file = env.path("t.json");
    std::fs::write(&file, task.to_string()).unwrap();
    env.ok(&["schema", "validate", "task", file.to_str().unwrap()]);
    let mut bad = task.clone();
    bad["state"] = "almost_done".into();
    std::fs::write(&file, bad.to_string()).unwrap();
    assert!(!env.run(&["schema", "validate", "task", file.to_str().unwrap()]).status.success());
}
