//! Guidance mode without a host: `interlock setup`, skill validation, attempt
//! worktrees, and the hooks as a host would call them, with the session and
//! agent ids a host puts in each payload.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

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
check = "sh checks/repro.sh"
min_strength = "tested"
producer = "self"

[[criterion]]
id = "explained"
statement = "the summary says why add subtracted"
min_strength = "static"
producer = "independent"
"#;

const INVESTIGATION: &str = r#"id = "why-add"
repository = "."
workflow = "investigation"
intent = "Why does add() subtract?"

[[criterion]]
id = "answer"
statement = "names the line that subtracts"
min_strength = "static"
producer = "independent"
"#;

/// A Copilot CLI stand-in that reports the flags interlock looks for.
const FAKE_COPILOT: &str = "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'GitHub Copilot CLI 1.0.0' ;;\n  *) echo '-p, --prompt <text>  --output-format <f>  --allow-tool --deny-tool --available-tools  --model <m>  --agent <a>  --plugin-dir <d>' ;;\nesac\n";

/// Who is calling, as a host reports it in a hook payload.
#[derive(Clone, Copy)]
struct Who {
    session: &'static str,
    agent: Option<&'static str>,
    agent_type: Option<&'static str>,
}

/// The session that does the work.
const WORKER: Who = Who { session: "sess-1", agent: None, agent_type: None };
/// interlock's verifier subagent inside that session, as Claude Code names it.
const VERIFIER: Who = Who { session: "sess-1", agent: Some("agent-v"), agent_type: Some("interlock:verifier") };
/// Some other subagent of the same session.
const HELPER: Who = Who { session: "sess-1", agent: Some("agent-h"), agent_type: Some("general-purpose") };
/// Another session in the same repository.
const OTHER: Who = Who { session: "sess-2", agent: None, agent_type: None };

impl Who {
    fn payload(&self, mut v: Value) -> Value {
        v["session_id"] = self.session.into();
        if let Some(a) = self.agent {
            v["agent_id"] = a.into();
        }
        if let Some(t) = self.agent_type {
            v["agent_type"] = t.into();
        }
        v
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_env(dir, args, &[])
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> String {
    let mut c = Command::new("git");
    c.args(args).current_dir(dir);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn quote(arg: &str) -> String {
    if arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,".contains(c)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

struct Repo {
    dir: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(p.join("README.md"), "calc\n").unwrap();
        std::fs::create_dir_all(p.join("checks")).unwrap();
        std::fs::write(p.join("checks/repro.sh"), "python3 -c 'import calc; assert calc.add(2, 3) == 5'\n").unwrap();
        std::fs::write(p.join(".gitignore"), "__pycache__/\n.interlock/\n*.toml\n").unwrap();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["add", "-A"]);
        git(p, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        std::fs::write(p.join("task.toml"), TASK).unwrap();
        std::fs::write(p.join("investigation.toml"), INVESTIGATION).unwrap();
        let bin = tempfile::tempdir().unwrap();
        let fake = bin.path().join("copilot");
        std::fs::write(&fake, FAKE_COPILOT).unwrap();
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&fake, perms).unwrap();
        Repo { dir, bin }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn db(&self) -> PathBuf {
        self.path().join(".interlock/state.db")
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
        c.env("INTERLOCK_COPILOT_BIN", self.bin.path().join("copilot"));
        c.env("INTERLOCK_CLAUDE_BIN", self.bin.path().join("no-claude-here"));
        c
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd(dir, args).output().unwrap()
    }

    fn ok_in(&self, dir: &Path, args: &[&str]) -> Value {
        let out = self.run_in(dir, args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
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
            .env("INTERLOCK_DB", self.db())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.to_string().as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// The hook's verdict on a shell command from `who`, run from the repository root.
    fn bash(&self, who: Who, command: &str) -> Output {
        self.bash_in(who, &self.path(), command)
    }

    /// The same from another directory: Claude Code keeps the directory a `cd` left the session in.
    fn bash_in(&self, who: Who, cwd: &Path, command: &str) -> Output {
        let p = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_input": {"command": command}, "cwd": cwd});
        self.hook("pre-tool-use", &who.payload(p))
    }

    fn edit(&self, who: Who, path: &Path) -> Output {
        let p = json!({"hook_event_name": "PreToolUse", "tool_name": "Edit",
            "tool_input": {"file_path": path, "old_string": "a", "new_string": "b"}, "cwd": self.path()});
        self.hook("pre-tool-use", &who.payload(p))
    }

    /// `interlock <args>` run by `who` in `dir`: the hook sees the command first, as on a host.
    fn guided(&self, who: Who, dir: &Path, args: &[&str]) -> Value {
        let line = format!(
            "cd {} && interlock {}",
            dir.display(),
            args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        );
        let verdict = self.bash(who, &line);
        assert_eq!(verdict.status.code(), Some(0), "the hook refused {line}: {}", stderr(&verdict));
        self.ok_in(dir, args)
    }

    fn ready(&self, spec: &str, id: &str) {
        if !self.db().exists() {
            self.ok(&["init"]);
        }
        self.ok(&["task", "create", spec]);
        let base = git(&self.path(), &["rev-parse", "HEAD"]);
        self.ok(&["task", "ready", id, "--base", &base]);
    }

    fn start(&self, who: Who, task: &str, role: &str) -> Attempt {
        let args = [
            "attempt",
            "start",
            task,
            "--role",
            role,
            "--host",
            "claude-code",
            "--worktree",
            "auto",
            "--capabilities",
            CAPS,
        ];
        let v = self.guided(who, &self.path(), &args);
        assert_eq!(v["started"], "yes", "{v:#}");
        Attempt {
            id: v["attempt"]["id"].as_str().unwrap().into(),
            token: v["token"].as_str().unwrap().into(),
            epoch: v["attempt"]["epoch"].to_string(),
            wt: PathBuf::from(v["attempt"]["worktree"].as_str().unwrap()),
            record: v["attempt"].clone(),
        }
    }

    fn status(&self, task: &str) -> Value {
        self.ok(&["status", task])
    }
}

struct Attempt {
    id: String,
    token: String,
    epoch: String,
    wt: PathBuf,
    record: Value,
}

impl Attempt {
    fn auth<'a>(&'a self, args: &[&'a str]) -> Vec<&'a str> {
        let mut v = args.to_vec();
        v.extend(["--attempt", &self.id, "--token", &self.token]);
        v
    }
}

/// A worker in session `WORKER` that fixed add() and submitted; returns it.
fn submitted_fix(repo: &Repo) -> Attempt {
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    std::fs::write(w.wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    let submit = w.auth(&["result", "submit", "--epoch", &w.epoch, "--tree", "auto", "--summary", "add used minus"]);
    let out = repo.guided(WORKER, &w.wt, &submit);
    assert_eq!(out["outcome"]["result"]["status"], "accepted", "{out:#}");
    w
}

#[test]
fn setup_installs_prefixed_skills_and_never_overwrites_what_is_not_its_own() {
    let repo = Repo::new();
    let root = repo.path();
    let mine = root.join(".github/skills/interlock-route/SKILL.md");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, "my own route skill\n").unwrap();

    let out = repo.run_in(&root, &["setup", "--host", "copilot"]);
    assert_eq!(out.status.code(), Some(2), "a file it did not write stops setup: {}", stderr(&out));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let refused = report["refused"].as_array().unwrap();
    assert_eq!(refused.len(), 1, "{report:#}");
    assert!(refused[0][0].as_str().unwrap().ends_with(".github/skills/interlock-route/SKILL.md"));
    assert!(refused[0][1].as_str().unwrap().contains("interlock did not write it"));
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "my own route skill\n");
    for s in ["design", "implement", "investigate", "review", "verify"] {
        assert!(root.join(format!(".github/skills/interlock-{s}/SKILL.md")).exists(), "{s}");
    }
    assert!(!root.join(".github/agents").exists(), "Copilot verifies through `interlock verify`, not a custom agent");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".interlock/setup-manifest.json")).unwrap()).unwrap();
    let hash = manifest["files"][".github/skills/interlock-verify/SKILL.md"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(manifest["files"][".github/skills/interlock-route/SKILL.md"].is_null(), "not interlock's");

    repo.ok(&["setup", "--host", "copilot", "--force"]);
    assert!(std::fs::read_to_string(&mine).unwrap().contains("interlock"), "--force overwrites");
    let again = repo.ok(&["setup", "--host", "copilot"]);
    assert_eq!(again["written"], json!([]), "a second run changes nothing");
    assert!(again["unchanged"].as_array().unwrap().len() > 6);

    let verify = root.join(".github/skills/interlock-verify/SKILL.md");
    std::fs::write(&verify, "edited by hand\n").unwrap();
    let out = repo.run_in(&root, &["setup", "--host", "copilot"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("changed since interlock wrote it"));
    assert_eq!(std::fs::read_to_string(&verify).unwrap(), "edited by hand\n");

    let hooks = std::fs::read_to_string(root.join(".interlock/guided/copilot-plugin/hooks/hooks.json")).unwrap();
    let hooks: Value = serde_json::from_str(&hooks).unwrap();
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    assert!(command.starts_with("INTERLOCK_MODE=interactive INTERLOCK_HOST=copilot INTERLOCK_DB="), "{command}");
    assert!(command.ends_with("hook pre-tool-use"), "{command}");
    assert!(hooks["hooks"]["SubagentStop"].is_array());

    let claude = repo.ok(&["setup", "--host", "claude-code"]);
    let plugin = root.join(".interlock/guided/claude-code-plugin");
    assert_eq!(claude["plugin_dir"], plugin.display().to_string());
    assert!(plugin.join("skills/verify/SKILL.md").exists() && plugin.join("agents/verifier.md").exists());
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
    assert_eq!(manifest["name"], "interlock", "the hooks join the skills' plugin");
    assert_eq!(repo.run_in(&root, &["setup", "--host", "cursor"]).status.code(), Some(1));
}

#[test]
fn setup_never_writes_through_a_symlink_or_outside_the_repository() {
    let repo = Repo::new();
    let root = repo.path();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join(".github")).unwrap();
    let out = repo.run_in(&root, &["setup", "--host", "copilot"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("symlink"));
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0, "nothing written through the link");

    // A store outside the repository is refused before anything is written.
    let elsewhere = tempfile::tempdir().unwrap();
    let mut c = repo.cmd(&root, &["setup", "--host", "claude-code"]);
    c.env("INTERLOCK_DB", elsewhere.path().join("state.db"));
    let out = c.output().unwrap();
    assert!(!out.status.success());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0, "{}", stderr(&out));
    assert!(!root.join(".interlock/guided/claude-code-plugin").exists());
}

#[test]
fn setup_refuses_to_save_a_host_report_without_capabilities() {
    let repo = Repo::new();
    let root = repo.path();
    let mute = repo.bin.path().join("mute-copilot");
    std::fs::write(&mute, "#!/bin/sh\necho 1.0.0\n").unwrap();
    let mut perms = std::fs::metadata(&mute).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&mute, perms).unwrap();
    let mut c = repo.cmd(&root, &["setup", "--host", "copilot"]);
    c.env("INTERLOCK_COPILOT_BIN", &mute);
    let out = c.output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no capabilities twice"), "{}", stderr(&out));
    assert!(!root.join(".github/skills").exists(), "nothing installed");

    // Nothing was saved: a later attempt probes again and finds no host at all.
    repo.ready("task.toml", "fix-add");
    let mut c = repo.cmd(&root, &["attempt", "start", "fix-add", "--role", "worker", "--host", "copilot"]);
    c.env("INTERLOCK_COPILOT_BIN", repo.bin.path().join("missing"));
    let out = c.output().unwrap();
    assert!(stderr(&out).contains("not installed"), "{}", stderr(&out));
}

#[test]
fn skills_validate_checks_skills_and_agent_files() {
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

    let plugin = repo.path().join("plugin");
    let plugin_arg = plugin.to_str().unwrap();
    repo.ok(&["skills", "generate", "--target", "claude-code", "--out", plugin_arg]);
    assert_eq!(repo.ok(&["skills", "validate", plugin_arg, "--target", "claude-code"])["valid"], true);
    let agent = plugin.join("agents/verifier.md");
    let text = std::fs::read_to_string(&agent).unwrap();
    let no_tools: String = text.lines().filter(|l| !l.starts_with("tools:")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&agent, no_tools).unwrap();
    let bad = repo.run_in(&repo.path(), &["skills", "validate", plugin_arg, "--target", "claude-code"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("the agent lists no tools"));

    // Generating over files it did not write is refused too.
    let out = repo.run_in(&repo.path(), &["skills", "generate", "--target", "claude-code", "--out", plugin_arg]);
    assert_eq!(out.status.code(), Some(2), "the edited agent file is not interlock's any more");
}

#[test]
fn result_submit_reads_the_change_from_the_tree_and_fails_closed() {
    let repo = Repo::new();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    // The worker makes interlock's check pass by rewriting it, and builds the tree by hand.
    std::fs::write(w.wt.join("checks/repro.sh"), "exit 0\n").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let index = scratch.path().join("index");
    git_env(&w.wt, &["read-tree", "HEAD"], &[("GIT_INDEX_FILE", &index)]);
    git_env(&w.wt, &["add", "-A"], &[("GIT_INDEX_FILE", &index)]);
    let tree = git_env(&w.wt, &["write-tree"], &[("GIT_INDEX_FILE", &index)]);

    // An id that is not a tree is refused, with nothing recorded.
    let bogus = w.auth(&["result", "submit", "--epoch", "1", "--tree", "0123456789abcdef0123456789abcdef01234567"]);
    let out = repo.run_in(&w.wt, &bogus);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("could not read what the result changes"), "{}", stderr(&out));
    assert_eq!(repo.status("fix-add")["task"]["state"], "running");
    // The caller cannot supply the change set.
    let listed = w.auth(&["result", "submit", "--epoch", "1", "--tree", &tree, "--changed", "calc.py"]);
    assert_eq!(repo.run_in(&w.wt, &listed).status.code(), Some(2));

    // Git variables that point elsewhere do not change what interlock reads.
    let submit = w.auth(&["result", "submit", "--epoch", "1", "--tree", &tree, "--summary", "s"]);
    let mut c = repo.cmd(&w.wt, &submit);
    c.env("GIT_DIR", "/nonexistent").env("GIT_WORK_TREE", "/").env("GIT_INDEX_FILE", scratch.path().join("other"));
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["outcome"]["result"]["status"], "rejected", "{v:#}");
    assert!(v["outcome"]["result"]["superseded_reason"].as_str().unwrap().contains("checks/repro.sh"), "{v:#}");
}

#[test]
fn guided_hooks_keep_every_caller_out_of_interlocks_state() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    let denied = |out: Output| {
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr(&out).contains("interlock's own state"), "{}", stderr(&out));
    };
    denied(repo.edit(WORKER, &root.join(".interlock/state.db")));
    denied(repo.edit(WORKER, &w.wt.join("../../tasks.json")));
    denied(repo.bash(WORKER, "rm -rf .interlock"));
    denied(repo.bash(WORKER, &format!("cd {} && sed -i s/a/b/ ../../state.db", w.wt.display())));
    denied(repo.bash(WORKER, &format!("echo '{{}}' > {}/.interlock/guided.json", root.display())));
    denied(repo.bash(WORKER, "mv .interlock/state.db /tmp/x"));
    // Sessions no attempt governs are kept out too.
    denied(repo.bash(OTHER, "rm -f .interlock/state.db"));
    denied(repo.edit(OTHER, &root.join(".interlock/worktrees/fix-add-worker-1/calc.py")));
    // Reading is fine: Claude Code's skills keep their references under .interlock/.
    assert_eq!(repo.bash(OTHER, "cat .interlock/guided/x/references/task-templates.md | head").status.code(), Some(0));
    assert_eq!(
        repo.bash(WORKER, "ls .interlock/worktrees && git -C .interlock/worktrees/fix-add-worker-1 status")
            .status
            .code(),
        Some(0)
    );
    // The worker's own worktree is its to change.
    assert_eq!(repo.edit(WORKER, &w.wt.join("calc.py")).status.code(), Some(0));
    assert_eq!(repo.bash(WORKER, &format!("cd {} && python3 -c 'import calc'", w.wt.display())).status.code(), Some(0));

    // A payload it cannot read is denied while a guided attempt is open.
    let mut c = repo.cmd(&root, &["hook", "pre-tool-use"]);
    c.env("INTERLOCK_MODE", "interactive").env("INTERLOCK_DB", repo.db()).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.stderr(Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"not json").unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(2));
    // So is a call that names no session.
    let anonymous =
        json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": root});
    assert_eq!(repo.hook("pre-tool-use", &anonymous).status.code(), Some(2));
}

#[test]
fn a_guided_attempt_governs_only_the_session_that_opened_it_until_submitted() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    assert_eq!(w.record["binding"]["via"], "session");
    assert_eq!(w.record["binding"]["host_session"], WORKER.session);
    let bound = &repo.status("fix-add")["attempts"][0]["bound"];
    assert_eq!(bound, &json!("bound to session sess-1"));

    let outside = repo.edit(WORKER, &root.join("calc.py"));
    assert_eq!(outside.status.code(), Some(2));
    assert!(stderr(&outside).contains("outside this attempt's worktree"));
    assert_eq!(repo.edit(HELPER, &root.join("calc.py")).status.code(), Some(2), "its subagents too");
    assert_eq!(repo.edit(OTHER, &root.join("calc.py")).status.code(), Some(0), "another session is not governed");
    let post = "curl -X POST https://example.com/notify";
    let asked = repo.bash(WORKER, post);
    assert_eq!(asked.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&asked.stdout).contains("\"permissionDecision\":\"ask\""), "interactive mode asks");

    std::fs::write(w.wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    let submit = w.auth(&["result", "submit", "--epoch", &w.epoch, "--tree", "auto", "--summary", "s"]);
    repo.guided(WORKER, &w.wt, &submit);
    assert_eq!(repo.edit(WORKER, &root.join("README.md")).status.code(), Some(0), "released once submitted");
    // Still in the submitted worktree (a live run hit this): it may leave and use interlock, not change files there.
    let leave = format!("cd {} && interlock status fix-add", root.display());
    assert_eq!(
        repo.bash_in(WORKER, &w.wt, &leave).status.code(),
        Some(0),
        "{}",
        stderr(&repo.bash_in(WORKER, &w.wt, &leave))
    );
    assert_eq!(repo.bash_in(WORKER, &w.wt, "interlock advance fix-add").status.code(), Some(0));
    assert_eq!(repo.bash_in(WORKER, &w.wt, "python3 -m unittest").status.code(), Some(2));
}

#[test]
fn the_worker_cannot_verify_its_own_work_and_only_a_bound_verifier_counts() {
    let repo = Repo::new();
    let root = repo.path();
    let w = submitted_fix(&repo);
    let start_verifier = "interlock attempt start fix-add --role verifier --host claude-code --worktree auto";
    let refused = repo.bash(WORKER, &format!("cd {} && {start_verifier}", root.display()));
    assert_eq!(refused.status.code(), Some(2));
    assert!(stderr(&refused).contains("did the work"), "{}", stderr(&refused));

    // Another subagent may open one, but it is unbound: its pass on a criterion without a check does not count.
    let helper = repo.start(HELPER, "fix-add", "verifier");
    assert_eq!(helper.record["binding"]["via"], "unbound");
    let assess = |a: &Attempt, who: Who, criterion: &str| {
        let args = a.auth(&["assess", "add", "--criterion", criterion, "--strength", "static", "--tree", "auto"]);
        repo.guided(who, &a.wt, &args)
    };
    assess(&helper, HELPER, "explained");
    repo.ok(&["advance", "fix-add"]);
    let status = repo.status("fix-add");
    assert_eq!(status["task"]["state"], "awaiting_verification");
    let needs = status.to_string();
    assert!(needs.contains("from an unbound verifier do not count"), "{status:#}");
    repo.guided(HELPER, &root, &["attempt", "end", &helper.id, "--token", &helper.token, "--note", "done"]);

    // interlock's verifier subagent is bound to its own identity, and only it can use its credentials.
    let v = repo.start(VERIFIER, "fix-add", "verifier");
    assert_eq!(v.record["binding"]["via"], "subagent");
    assert_eq!(v.record["binding"]["agent_type"], "interlock:verifier");
    for cmd in [
        format!(
            "interlock assess add --criterion explained --strength static --tree auto --attempt {} --token {}",
            v.id, v.token
        ),
        format!(
            "cd {} && interlock check run --criterion repro --attempt {} --token {}",
            v.wt.display(),
            v.id,
            v.token
        ),
        format!("interlock attempt end {} --token {}", v.id, v.token),
    ] {
        let out = repo.bash(WORKER, &cmd);
        assert_eq!(out.status.code(), Some(2), "{cmd}");
        assert!(stderr(&out).contains("belongs to another agent"), "{}", stderr(&out));
    }
    let run = repo.guided(VERIFIER, &v.wt, &v.auth(&["check", "run", "--criterion", "repro"]));
    assert_eq!(run["passed"], true, "{run:#}");
    let args = v.auth(&["assess", "add", "--criterion", "repro", "--strength", "tested", "--tree", "auto"]);
    repo.guided(VERIFIER, &v.wt, &args);
    assess(&v, VERIFIER, "explained");
    let brief = String::from_utf8(repo.run_in(&root, &["brief", "fix-add"]).stdout).unwrap();
    assert!(brief.contains("interlock's verifier subagent: session sess-1, agent agent-v"), "{brief}");
    repo.guided(VERIFIER, &root, &["attempt", "end", &v.id, "--token", &v.token, "--note", "pass"]);

    let advanced = repo.ok(&["advance", "fix-add"]);
    assert_eq!(advanced["status"]["task"]["state"], "done", "{advanced:#}");
    // Done closes what was left open and removes the attempts' worktrees.
    let statuses: Vec<&str> =
        advanced["status"]["attempts"].as_array().unwrap().iter().map(|a| a["status"].as_str().unwrap()).collect();
    assert!(statuses.iter().all(|s| *s != "running" && *s != "submitted"), "{statuses:?}");
    for wt in [&w.wt, &helper.wt, &v.wt] {
        assert!(!wt.exists(), "{} still there", wt.display());
    }
    // The verified output outlives them, as a ref the person can apply.
    assert_eq!(advanced["settled"]["output_ref"], "refs/interlock/tasks/fix-add", "{advanced:#}");
    assert_eq!(git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "no branch moved");
    git(&root, &["-c", "user.name=t", "-c", "user.email=t@t", "cherry-pick", "refs/interlock/tasks/fix-add"]);
    assert_eq!(std::fs::read_to_string(root.join("calc.py")).unwrap(), "def add(a, b):\n    return a + b\n");
}

#[test]
fn an_investigation_changes_nothing_at_all() {
    let repo = Repo::new();
    repo.ready("investigation.toml", "why-add");
    let w = repo.start(WORKER, "why-add", "worker");
    let edit = repo.edit(WORKER, &w.wt.join("calc.py"));
    assert_eq!(edit.status.code(), Some(2), "the edit tools are denied");
    // A file made some other way still changes the tree, and the result is rejected.
    std::fs::write(w.wt.join("notes.txt"), "scratch\n").unwrap();
    let submit =
        w.auth(&["result", "submit", "--epoch", &w.epoch, "--tree", "auto", "--summary", "calc.py:2 subtracts"]);
    let out = repo.guided(WORKER, &w.wt, &submit);
    assert_eq!(out["outcome"]["result"]["status"], "rejected", "{out:#}");
    let reason = out["outcome"]["result"]["superseded_reason"].as_str().unwrap();
    assert!(reason.contains("notes.txt") && reason.contains("investigation"), "{reason}");
}

#[test]
fn an_empty_scope_allows_no_change() {
    let repo = Repo::new();
    let spec = TASK.replace("paths = [\"calc.py\"]", "paths = []");
    std::fs::write(repo.path().join("empty.toml"), spec).unwrap();
    repo.ready("empty.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    std::fs::write(w.wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    let submit = w.auth(&["result", "submit", "--epoch", &w.epoch, "--tree", "auto"]);
    let out = repo.guided(WORKER, &w.wt, &submit);
    assert_eq!(out["outcome"]["result"]["status"], "rejected", "{out:#}");
}

#[test]
fn notes_and_tree_auto_follow_the_attempt_not_the_directory() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    let note = repo
        .bash(WORKER, "interlock note add --task fix-add --kind design --file - <<'EOF'\npick the one-line fix\nEOF");
    assert_eq!(note.status.code(), Some(0), "{}", stderr(&note));
    let mut c = repo.cmd(&w.wt, &["note", "add", "--task", "fix-add", "--kind", "design", "--file", "-"]);
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"pick the one-line fix\n").unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let notes = repo.ok(&["note", "list", "--task", "fix-add"]);
    assert_eq!(notes[0]["kind"], "design");
    assert_eq!(notes[0]["body"], "pick the one-line fix\n");

    // Run from the repository root, --tree auto still means the attempt's worktree.
    std::fs::write(w.wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    let base_tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let claim = repo.ok(&w.auth(&["claim", "add", "--criterion", "repro", "--strength", "tested", "--tree", "auto"]));
    let claimed = claim["outcome"]["evidence"]["currency"]["tree"].as_str().unwrap().to_string();
    let submitted = repo.ok(&w.auth(&["result", "submit", "--epoch", &w.epoch, "--tree", "auto"]));
    let tree = submitted["outcome"]["result"]["output_tree"].as_str().unwrap();
    assert_ne!(tree, base_tree, "not the root's unchanged files");
    assert_eq!(submitted["outcome"]["result"]["changed_paths"], json!(["calc.py"]));
    assert_eq!(claimed, tree, "the claim names the worktree's tree too");
}

#[test]
fn stop_guard_messages_are_complete_and_ask_each_role_for_its_own_record() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    let stop = |who: Who, event: &str| {
        let p = json!({"hook_event_name": event, "stop_hook_active": false, "cwd": root});
        String::from_utf8(repo.hook("stop", &who.payload(p)).stdout).unwrap()
    };
    let held = stop(WORKER, "Stop");
    assert!(held.contains("\"decision\":\"block\""), "{held}");
    let auth = format!("--attempt {} --token <your token from attempt start>", w.id);
    assert!(held.contains(&format!("interlock check run --criterion repro {auth}")), "{held}");
    assert!(held.contains("interlock claim add --criterion repro"), "{held}");
    assert!(!held.contains("assess"), "a worker is never asked for an assessment: {held}");
    assert_eq!(stop(OTHER, "Stop"), "", "another session is not held");

    std::fs::write(w.wt.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    repo.guided(WORKER, &w.wt, &w.auth(&["result", "submit", "--epoch", &w.epoch, "--tree", "auto"]));
    let v = repo.start(VERIFIER, "fix-add", "verifier");
    let held = stop(VERIFIER, "SubagentStop");
    assert!(held.contains("interlock assess add --criterion repro --strength"), "{held}");
    assert!(held.contains(&format!("--attempt {} --token", v.id)), "{held}");
    assert_eq!(stop(WORKER, "Stop"), "", "the submitted worker is free to stop");
}

#[test]
fn cancel_closes_the_attempts_and_removes_their_worktrees() {
    let repo = Repo::new();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    assert!(w.wt.exists());
    let cancelled = repo.ok(&["task", "cancel", "fix-add", "--reason", "not needed"]);
    assert_eq!(cancelled["settled"]["removed_worktrees"], json!([w.wt]), "{cancelled:#}");
    assert!(!w.wt.exists());
    assert_eq!(repo.status("fix-add")["attempts"][0]["status"], "cancelled");
}

#[test]
fn the_store_is_found_from_a_worktree_and_never_made_inside_one() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ready("task.toml", "fix-add");
    let w = repo.start(WORKER, "fix-add", "worker");
    assert_eq!(w.wt, root.join(".interlock/worktrees/fix-add-worker-1"));
    assert_eq!(repo.ok_in(&w.wt, &["status", "fix-add"])["task"]["state"], "running");

    // A worktrees folder with no store above it gets no fresh store.
    let stray = tempfile::tempdir().unwrap();
    let inside = stray.path().join(".interlock/worktrees/x-worker-1");
    std::fs::create_dir_all(&inside).unwrap();
    let out = repo.run_in(&inside, &["status", "x"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("attempt's worktree"), "{}", stderr(&out));
    assert!(!stray.path().join(".interlock/state.db").exists());

    // A separate repository nested in this one still gets its own store.
    let nested = root.join("vendor/other");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    repo.ok_in(&nested, &["init"]);
    assert!(nested.join(".interlock/state.db").exists());
}

#[test]
fn a_task_id_cannot_climb_out_of_the_store() {
    let repo = Repo::new();
    let root = repo.path();
    repo.ok(&["init"]);
    let spec = TASK.replace("id = \"fix-add\"", "id = \"../../../victim\"");
    std::fs::write(root.join("trav.toml"), spec).unwrap();
    let out = repo.run_in(&root, &["task", "create", "trav.toml"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("does not match"), "{}", stderr(&out));
    assert!(!root.parent().unwrap().join("victim-worker-1").exists());
}
