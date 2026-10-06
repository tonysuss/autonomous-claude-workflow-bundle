//! The pieces of guidance mode that need no host: `interlock setup`, skill
//! validation, attempt worktrees, results read from the tree, the store found
//! from inside a worktree, and hooks that find the guided attempt and ask.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

// Declared so these tests do not depend on which hosts are installed.
const CAPS: &str = "session_start,session_collect,session_cancel,tool_restriction,per_call_policy,custom_agents";

const TASK: &str = r#"id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add() subtracts"

[scope]
paths = ["calc.py"]

[[criterion]]
id = "repro"
statement = "add(2, 3) is 5"
check = "python3 -c 'import calc; assert calc.add(2, 3) == 5'"
min_strength = "tested"
producer = "self"
baseline = "fails"
"#;

struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(p.join("README.md"), "calc\n").unwrap();
        std::fs::write(p.join(".gitignore"), "__pycache__/\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "-A"],
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"],
        ] {
            assert!(Command::new("git").args(args).current_dir(p).status().unwrap().success());
        }
        std::fs::write(p.join("task.toml"), TASK).unwrap();
        Repo { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_interlock"));
        c.args(args).current_dir(dir);
        for k in [
            "INTERLOCK_DB",
            "INTERLOCK_ATTEMPT",
            "INTERLOCK_TOKEN",
            "INTERLOCK_MODE",
            "INTERLOCK_TREE",
            "INTERLOCK_FAULT",
        ] {
            c.env_remove(k);
        }
        c
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd(dir, args).output().unwrap()
    }

    fn ok_in(&self, dir: &Path, args: &[&str]) -> Value {
        let out = self.run_in(dir, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }

    fn ok(&self, args: &[&str]) -> Value {
        self.ok_in(&self.path(), args)
    }

    /// Runs `interlock hook <event>` the way the guided plugin does.
    fn hook(&self, event: &str, payload: &Value) -> Output {
        let mut c = self.cmd(&self.path(), &["hook", event]);
        c.env("INTERLOCK_MODE", "interactive")
            .env("INTERLOCK_HOST", "claude-code")
            .env("INTERLOCK_DB", self.path().join(".interlock/state.db"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.to_string().as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
}

#[test]
fn setup_installs_skills_the_verifier_and_interactive_hooks() {
    let repo = Repo::new();
    let copilot = repo.ok(&["setup", "--host", "copilot"]);
    let root = repo.path();
    for s in ["route", "investigate", "design", "implement", "verify", "review"] {
        assert!(root.join(".github/skills").join(s).join("SKILL.md").exists(), "{s}");
    }
    assert!(root.join(".github/agents/interlock-verifier.agent.md").exists());
    let hooks = std::fs::read_to_string(root.join(".interlock/guided/copilot-plugin/hooks/hooks.json")).unwrap();
    let hooks: Value = serde_json::from_str(&hooks).unwrap();
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    assert!(command.starts_with("INTERLOCK_MODE=interactive INTERLOCK_HOST=copilot INTERLOCK_DB="), "{command}");
    assert!(command.ends_with("hook pre-tool-use"), "{command}");
    assert!(hooks["hooks"]["SubagentStop"].is_array());
    assert!(copilot["session_command"].as_str().unwrap().starts_with("copilot --plugin-dir "));
    assert!(root.join(".interlock/state.db").exists(), "setup creates the store");

    let claude = repo.ok(&["setup", "--host", "claude-code"]);
    let plugin = root.join(".interlock/guided/claude-code-plugin");
    assert_eq!(claude["plugin_dir"], plugin.display().to_string());
    assert!(plugin.join("skills/verify/SKILL.md").exists() && plugin.join("agents/verifier.md").exists());
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
    assert_eq!(manifest["name"], "interlock", "the hooks join the skills' plugin");
    assert!(std::fs::read_to_string(plugin.join("hooks/hooks.json")).unwrap().contains("INTERLOCK_HOST=claude-code"));
    assert!(repo.run_in(&root, &["setup", "--host", "cursor"]).status.code() == Some(1));
}

#[test]
fn skills_validate_reports_problems_and_exits_nonzero() {
    let repo = Repo::new();
    let out = repo.path().join("agent-skills");
    repo.ok(&["skills", "generate", "--target", "agent-skills", "--out", out.to_str().unwrap()]);
    assert_eq!(repo.ok(&["skills", "validate", out.to_str().unwrap()])["valid"], true);
    std::fs::write(out.join("verify/SKILL.md"), "---\nname: Verify\n---\nbody\n").unwrap();
    let bad = repo.run_in(&repo.path(), &["skills", "validate", out.to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&bad.stdout).unwrap();
    let messages: Vec<&str> =
        report["problems"].as_array().unwrap().iter().filter_map(|p| p["message"].as_str()).collect();
    assert!(messages.iter().any(|m| m.contains("lowercase")), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("description")), "{messages:?}");
}

#[test]
fn a_guided_worker_gets_a_worktree_and_scope_holds_on_the_tree() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ok(&["init"]);
    repo.ok(&["task", "create", "task.toml"]);
    let base =
        String::from_utf8(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&root).output().unwrap().stdout)
            .unwrap();
    repo.ok(&["task", "ready", "fix-add", "--base", base.trim()]);
    let started = repo.ok(&[
        "attempt",
        "start",
        "fix-add",
        "--role",
        "worker",
        "--host",
        "claude-code",
        "--worktree",
        "auto",
        "--capabilities",
        CAPS,
    ]);
    let wt = PathBuf::from(started["attempt"]["worktree"].as_str().unwrap());
    assert_eq!(wt, root.join(".interlock/worktrees/fix-add-worker-1"));
    let (id, token) = (started["attempt"]["id"].as_str().unwrap(), started["token"].as_str().unwrap());
    let auth = ["--attempt", id, "--token", token];

    // From inside the worktree, with no INTERLOCK_DB, interlock finds the repository's store.
    assert_eq!(repo.ok_in(&wt, &["status", "fix-add"])["task"]["state"], "running");
    // A separate repository nested in this one still gets its own store.
    let nested = root.join("vendor/other");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&nested).status().unwrap().success());
    repo.ok_in(&nested, &["init"]);
    assert!(nested.join(".interlock/state.db").exists());

    // The hooks find the guided attempt with nothing in the environment.
    let edit = |path: &Path| serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Edit", "tool_input": {"file_path": path}, "cwd": root});
    let outside = repo.hook("pre-tool-use", &edit(&root.join("calc.py")));
    assert_eq!(outside.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&outside.stderr).contains("outside this attempt's worktree"));
    assert_eq!(repo.hook("pre-tool-use", &edit(&wt.join("calc.py"))).status.code(), Some(0));
    let post = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": {"command": "curl -X POST https://example.com/notify"}, "cwd": root});
    let asked = repo.hook("pre-tool-use", &post);
    assert_eq!(asked.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&asked.stdout).contains("\"permissionDecision\":\"ask\""), "interactive mode asks");

    // An out-of-scope change is rejected from the tree itself, with no --changed list.
    std::fs::write(wt.join("README.md"), "calc, edited\n").unwrap();
    let mut submit = vec!["result", "submit", "--epoch", "1", "--tree", "auto", "--summary", "s"];
    submit.extend(auth);
    let rejected = repo.ok_in(&wt, &submit);
    assert_eq!(rejected["outcome"]["result"]["status"], "rejected", "{rejected:#}");
    assert!(rejected["outcome"]["result"]["superseded_reason"].as_str().unwrap().contains("README.md"));

    std::fs::write(wt.join("README.md"), "calc\n").unwrap();
    std::fs::write(wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    let accepted = repo.ok_in(&wt, &submit);
    assert_eq!(accepted["outcome"]["result"]["status"], "accepted", "{accepted:#}");
    assert_eq!(accepted["outcome"]["result"]["changed_paths"], serde_json::json!(["calc.py"]));
    let tree = accepted["outcome"]["result"]["output_tree"].as_str().unwrap();
    assert_eq!(tree.len(), 40, "--tree auto recorded the worktree's tree, not the word auto");
    assert_eq!(std::fs::read_to_string(root.join("calc.py")).unwrap(), "def add(a, b):\n    return a - b\n");

    // The stop guard holds the guided worker until it has run the check and claimed.
    let stop = serde_json::json!({"hook_event_name": "Stop", "stop_hook_active": false, "cwd": root});
    let held = repo.hook("stop", &stop);
    assert!(String::from_utf8_lossy(&held.stdout).contains("\"decision\":\"block\""));
}
