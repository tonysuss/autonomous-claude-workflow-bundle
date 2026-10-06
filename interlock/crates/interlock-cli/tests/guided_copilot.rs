//! Guidance mode on the real Copilot CLI, offline, with a scripted model
//! standing in for the person's session: `interlock setup --host copilot`
//! installs the generated skills, the verifier agent and the interactive hooks;
//! the session invokes the skills with Copilot's `skill` tool, runs the
//! interlock commands they prescribe, and delegates verification to the
//! verifier custom agent with Copilot's `task` tool. They skip unless a
//! Copilot CLI binary is available (INTERLOCK_COPILOT_BIN or `copilot` on PATH).
//!
//! Set INTERLOCK_EVIDENCE_DIR to keep each run's transcript, model log, task
//! log and status there.

mod guided_model;

use std::path::{Path, PathBuf};
use std::process::Command;

use guided_model::{Conversation, GuidedModel, Step, bash, tool};
use serde_json::{Value, json};

fn copilot() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("INTERLOCK_COPILOT_BIN").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("copilot")).find(|p| p.is_file())
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

struct Guided {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    copilot: PathBuf,
}

impl Guided {
    fn new(files: &[(&str, &str)]) -> Option<Guided> {
        let Some(copilot) = copilot() else {
            eprintln!("skipping: no Copilot CLI binary");
            return None;
        };
        let repo = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let p = repo.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::write(repo.path().join(".gitignore"), "__pycache__/\n").unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let g = Guided { repo, home: tempfile::tempdir().unwrap(), copilot };
        let setup = g.interlock(&["setup", "--host", "copilot"]);
        assert_eq!(setup["skills"], json!(["design", "implement", "investigate", "review", "route", "verify"]));
        assert_eq!(setup["host_report"]["installed"], true, "setup saves the host's capabilities: {setup:#}");
        Some(g)
    }

    fn path(&self) -> PathBuf {
        self.repo.path().canonicalize().unwrap()
    }

    fn env(&self, cmd: &mut Command) {
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_interlock")).parent().unwrap().to_path_buf();
        let path = std::env::join_paths(
            std::iter::once(bin_dir).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        cmd.current_dir(self.repo.path())
            .env("PATH", path)
            .env("INTERLOCK_COPILOT_BIN", &self.copilot)
            .env("COPILOT_HOME", self.home.path())
            .env("COPILOT_OFFLINE", "true")
            .env("COPILOT_MODEL", "gpt-4.1")
            .env("COPILOT_AUTO_UPDATE", "false")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        for k in ["INTERLOCK_ATTEMPT", "INTERLOCK_TOKEN", "INTERLOCK_TREE", "INTERLOCK_MODE", "INTERLOCK_DB"] {
            cmd.env_remove(k);
        }
    }

    fn interlock(&self, args: &[&str]) -> Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_interlock"));
        cmd.args(args);
        self.env(&mut cmd);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "interlock {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// One guided session: the person's prompt, through Copilot, against the model.
    fn session(&self, model: &GuidedModel, prompt: &str) -> Vec<Value> {
        // Copilot processes share a package cache; one session at a time keeps the tests steady.
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let plugin = self.path().join(".interlock/guided/copilot-plugin");
        let mut cmd = Command::new(&self.copilot);
        cmd.args(["-p", prompt, "--output-format", "json", "--allow-all-tools", "--no-ask-user", "--no-auto-update"])
            .args(["--plugin-dir", plugin.to_str().unwrap()])
            .env("COPILOT_PROVIDER_BASE_URL", model.base_url());
        self.env(&mut cmd);
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(out.status.success(), "copilot failed: {stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        stdout.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    fn keep(&self, name: &str, events: &[Value], model: &GuidedModel, task: &str) {
        let Some(dir) = std::env::var_os("INTERLOCK_EVIDENCE_DIR").map(PathBuf::from) else { return };
        let dir = dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let lines = |vs: &[Value]| redact(&vs.iter().map(|v| v.to_string() + "\n").collect::<String>());
        std::fs::write(dir.join("copilot-transcript.jsonl"), lines(events)).unwrap();
        std::fs::write(dir.join("model-requests.jsonl"), lines(&model.log())).unwrap();
        let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap();
        std::fs::write(dir.join("task-log.json"), pretty(self.interlock(&["task", "log", task]))).unwrap();
        std::fs::write(dir.join("status.json"), pretty(self.interlock(&["status", task]))).unwrap();
        std::fs::write(dir.join("attempts.json"), pretty(self.interlock(&["attempt", "list", task]))).unwrap();
    }
}

/// Attempt tokens are bookkeeping, not secrets, but evidence never carries them.
/// Every value shown after a token marker is replaced wherever it appears,
/// since hosts repeat command arguments in their own metadata.
fn redact(text: &str) -> String {
    let mut tokens = std::collections::BTreeSet::new();
    for marker in ["--token ", "token\\\": \\\"", "token\": \""] {
        for (i, _) in text.match_indices(marker) {
            let hex: String = text[i + marker.len()..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if hex.len() >= 32 {
                tokens.insert(hex);
            }
        }
    }
    tokens.iter().fold(text.to_string(), |out, t| out.replace(t.as_str(), "<redacted>"))
}

/// Tool results in a Copilot JSONL stream, in order, with the tool's name.
fn tool_results(events: &[Value]) -> Vec<(String, bool, String)> {
    let mut names = std::collections::HashMap::new();
    for e in events.iter().filter(|e| e["type"] == "tool.execution_start") {
        names.insert(e["data"]["toolCallId"].as_str().unwrap_or("").to_string(), e["data"]["toolName"].to_string());
    }
    events
        .iter()
        .filter(|e| e["type"] == "tool.execution_complete")
        .map(|e| {
            let d = &e["data"];
            let name = names.get(d["toolCallId"].as_str().unwrap_or("")).cloned().unwrap_or_default();
            let text = if d["success"] == true { d["result"]["content"].to_string() } else { d["error"].to_string() };
            (name.trim_matches('"').to_string(), d["success"] == true, text)
        })
        .collect()
}

fn signals(g: &Guided, task: &str) -> Vec<String> {
    let log = g.interlock(&["task", "log", task]);
    log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap().to_string()).collect()
}

const CALC: &str = "def add(a, b):\n    return a - b\n";
const TEST_CALC: &str = "import unittest\n\nimport calc\n\n\nclass T(unittest.TestCase):\n    def test_zero(self):\n        self.assertEqual(calc.add(0, 0), 0)\n";

const BUG_TASK: &str = r#"id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add(2, 3) returns -1 instead of 5"

[scope]
paths = ["calc.py", "tests/**"]

[[criterion]]
id = "repro"
statement = "add(2, 3) returns 5"
check = "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'"
min_strength = "observed"
producer = "independent"
baseline = "fails"

[[criterion]]
id = "regression"
statement = "The existing tests pass"
check = "python3 -m unittest discover -s tests -t ."
min_strength = "tested"
producer = "self"
baseline = "passes"
"#;

fn on_worktree(cmd: &str) -> Step {
    bash(&format!("cd {{worktree}} && {cmd} --attempt {{attempt}} --token {{token}}"))
}

#[test]
fn a_bug_fix_runs_end_to_end_in_guidance_mode() {
    let Some(g) = Guided::new(&[("calc.py", CALC), ("tests/__init__.py", ""), ("tests/test_calc.py", TEST_CALC)])
    else {
        return;
    };
    let repo = g.path();
    let root_calc = repo.join("calc.py").display().to_string();
    let main = Conversation {
        marker: "GUIDED-BUGFIX".into(),
        steps: vec![
            tool("skill", json!({"skill": "route"})),
            bash(&format!(
                "interlock init && mkdir -p .interlock/tasks && cat > .interlock/tasks/fix-add.toml <<'EOF'\n{BUG_TASK}EOF"
            )),
            bash("interlock task create .interlock/tasks/fix-add.toml"),
            bash("interlock task ready fix-add --base \"$(git rev-parse HEAD)\""),
            tool("skill", json!({"skill": "implement"})),
            bash("interlock attempt start fix-add --role worker --host copilot --worktree auto"),
            on_worktree("interlock check run --criterion repro --target base"),
            on_worktree("interlock check run --criterion regression --target base"),
            // The hooks hold the worker to its worktree: this edit is denied.
            tool("edit", json!({"path": root_calc, "old_str": "a - b", "new_str": "a + b"})),
            tool("edit", json!({"path": "{worktree}/calc.py", "old_str": "a - b", "new_str": "a + b"})),
            // An external action the grant does not cover: the hooks ask instead of denying.
            bash("curl -s -X POST http://127.0.0.1:9/notify || echo 'notify failed'"),
            on_worktree("interlock check run --criterion repro"),
            on_worktree("interlock check run --criterion regression"),
            on_worktree(
                "interlock claim add --criterion repro --strength tested --tree auto --ref 'check repro' --note 'add(2, 3) is 5'",
            ),
            on_worktree(
                "interlock claim add --criterion regression --strength tested --tree auto --ref 'unittest' --note '1 test passed'",
            ),
            on_worktree(
                "interlock result submit --epoch {epoch} --tree auto --summary 'add() subtracted its arguments; it now adds them'",
            ),
            tool("skill", json!({"skill": "verify"})),
            tool(
                "task",
                json!({"name": "verifier", "agent_type": "interlock-verifier", "description": "Verify fix-add", "mode": "sync",
                       "prompt": format!("GUIDED-VERIFY Verify interlock task fix-add in the repository at {}. Host: copilot. \
                                          Findings to act on: none.", repo.display())}),
            ),
            bash("interlock advance fix-add"),
            bash("interlock status fix-add"),
        ],
        reply: "fix-add is done: verified by the independent verifier.".into(),
    };
    let verifier = Conversation {
        marker: "GUIDED-VERIFY".into(),
        steps: vec![
            bash(&format!(
                "cd {} && interlock attempt start fix-add --role verifier --host copilot --worktree auto --agent interlock-verifier",
                repo.display()
            )),
            bash("cd {worktree} && interlock brief fix-add --role verifier"),
            on_worktree("interlock check run --criterion repro"),
            on_worktree("interlock check run --criterion regression"),
            on_worktree(
                "interlock assess add --criterion repro --strength observed --tree auto --ref 'check repro' --note 'add(2, 3) is 5 on the submitted files'",
            ),
            on_worktree(
                "interlock assess add --criterion regression --strength tested --tree auto --ref 'unittest' --note 'the suite passes'",
            ),
            bash("interlock attempt end {attempt} --token {token} --note 'repro: pass, regression: pass'"),
        ],
        reply: "Verifier attempt ended. repro: pass. regression: pass.".into(),
    };
    let model = GuidedModel::start(vec![verifier, main]);
    let events = g.session(&model, "GUIDED-BUGFIX add(2, 3) in calc.py returns -1 instead of 5. Please fix it.");
    g.keep("copilot-bug-fix", &events, &model, "fix-add");

    let results = tool_results(&events);
    let skills: Vec<&str> = results
        .iter()
        .filter(|(n, _, _)| n == "skill")
        .map(|(_, ok, t)| if *ok { t.as_str() } else { "failed" })
        .collect();
    assert_eq!(skills.len(), 3, "{results:#?}");
    assert!(skills.iter().all(|t| t.contains("loaded successfully")), "{skills:?}");
    let denied = results.iter().find(|(n, _, t)| n == "edit" && t.contains("outside this attempt's worktree"));
    assert!(denied.is_some(), "the edit outside the worktree is denied: {results:#?}");
    assert_eq!(std::fs::read_to_string(repo.join("calc.py")).unwrap(), CALC, "the repository's own file is untouched");
    // In interactive mode the hook asks; with no one to ask, `copilot -p` turns that into a denial.
    let asked = results.iter().find(|(n, ok, t)| n == "bash" && !ok && t.contains("unable to ask user"));
    assert!(asked.is_some_and(|(_, _, t)| t.contains("external_reversible is not granted")), "{results:#?}");

    let status = g.interlock(&["status", "fix-add"]);
    assert_eq!(status["task"]["state"], "done", "{status:#}");
    assert_eq!(signals(&g, "fix-add"), ["G1", "G2", "G3", "G4", "G7"]);
    let attempts = g.interlock(&["attempt", "list", "fix-add"]);
    let verifier = attempts.as_array().unwrap().iter().find(|a| a["role"] == "verifier").expect("a verifier attempt");
    assert_eq!(verifier["agent"], "interlock-verifier");
    assert_eq!(verifier["status"], "completed");

    // The verifier ran as the custom agent: its profile was its system prompt, and it had no edit tools.
    let verifier_requests: Vec<Value> =
        model.log().into_iter().filter(|r| r["conversation"] == "GUIDED-VERIFY").collect();
    assert!(!verifier_requests.is_empty());
    for r in &verifier_requests {
        let offered: Vec<&str> = r["offered"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(offered.contains(&"bash") && !offered.contains(&"edit") && !offered.contains(&"create"), "{offered:?}");
    }
}

const EXPORTER: &str = "def export(rows, sink, retries=2):\n    \"\"\"Write rows; on a transient failure, retry the whole batch.\"\"\"\n    for attempt in range(retries + 1):\n        try:\n            for row in rows:\n                sink.write(row)\n            return attempt + 1\n        except IOError:\n            continue\n    raise IOError(\"export failed\")\n";

const DEMO: &str = "python3 -c 'import exporter\nclass S:\n    n = 0\n    def write(self, r):\n        S.n += 1\n        raise IOError\ntry:\n    exporter.export([1], S())\nexcept IOError:\n    print(\"tries:\", S.n)'";

const QUESTION_TASK: &str = r#"id = "export-tries"
repository = "."
workflow = "investigation"
intent = "How many times does export() try a batch before giving up, and where is that decided?"

[[criterion]]
id = "answer"
statement = "The answer gives the number of tries with file and line citations, and a command shows it"
min_strength = "observed"
producer = "independent"
"#;

#[test]
fn an_investigation_runs_end_to_end_in_guidance_mode() {
    let Some(g) = Guided::new(&[("exporter.py", EXPORTER)]) else { return };
    let repo = g.path();
    let main = Conversation {
        marker: "GUIDED-INVESTIGATION".into(),
        steps: vec![
            tool("skill", json!({"skill": "route"})),
            bash(&format!(
                "interlock init && mkdir -p .interlock/tasks && cat > .interlock/tasks/export-tries.toml <<'EOF'\n{QUESTION_TASK}EOF"
            )),
            bash("interlock task create .interlock/tasks/export-tries.toml"),
            bash("interlock task ready export-tries --base \"$(git rev-parse HEAD)\""),
            tool("skill", json!({"skill": "investigate"})),
            bash("interlock attempt start export-tries --role worker --host copilot --worktree auto"),
            bash("cd {worktree} && grep -n 'retries' exporter.py && git log --oneline -3 -- exporter.py"),
            bash(&format!("cd {{worktree}} && {DEMO}")),
            on_worktree(
                "interlock claim add --criterion answer --strength observed --tree auto --ref 'exporter.py:1' \
                 --ref 'python3 demo with a sink that always fails' --note 'retries=2 means range(3): three tries'",
            ),
            on_worktree(
                "interlock result submit --epoch {epoch} --tree auto --summary 'export() tries a batch retries + 1 times: \
                 three by default (exporter.py:1 sets retries=2; exporter.py:3 loops over range(retries + 1)). The demo \
                 prints tries: 3.'",
            ),
            tool("skill", json!({"skill": "verify"})),
            tool(
                "task",
                json!({"name": "verifier", "agent_type": "interlock-verifier", "description": "Verify export-tries",
                       "mode": "sync", "prompt": format!("GUIDED-VERIFY Verify interlock task export-tries in the repository \
                       at {}. Host: copilot. Findings to act on: none.", repo.display())}),
            ),
            bash("interlock advance export-tries"),
            bash("interlock status export-tries"),
        ],
        reply: "export() tries a batch three times by default.".into(),
    };
    let verifier = Conversation {
        marker: "GUIDED-VERIFY".into(),
        steps: vec![
            bash(&format!(
                "cd {} && interlock attempt start export-tries --role verifier --host copilot --worktree auto --agent interlock-verifier",
                repo.display()
            )),
            bash("cd {worktree} && interlock brief export-tries --role verifier"),
            bash("cd {worktree} && sed -n '1,4p' exporter.py"),
            bash(&format!("cd {{worktree}} && {DEMO}")),
            on_worktree(
                "interlock assess add --criterion answer --strength observed --tree auto --ref 'demo' --note 'tries: 3, as the answer says; citations match'",
            ),
            bash("interlock attempt end {attempt} --token {token} --note 'answer: pass'"),
        ],
        reply: "Verifier attempt ended. answer: pass.".into(),
    };
    let model = GuidedModel::start(vec![verifier, main]);
    let events = g.session(&model, "GUIDED-INVESTIGATION How many times does export() try a batch?");
    g.keep("copilot-investigation", &events, &model, "export-tries");

    let results = tool_results(&events);
    assert!(
        results.iter().filter(|(n, _, _)| n == "skill").all(|(_, ok, t)| *ok && t.contains("loaded successfully")),
        "{results:#?}"
    );
    let demo = results.iter().filter(|(n, ok, t)| n == "bash" && *ok && t.contains("tries: 3")).count();
    assert!(demo >= 1, "the worker's demo ran: {results:#?}");
    let status = g.interlock(&["status", "export-tries"]);
    assert_eq!(status["task"]["state"], "done", "{status:#}");
    assert_eq!(signals(&g, "export-tries"), ["G1", "G2", "G3", "G4", "G7"]);
    let verifier_ran_demo = model.log().iter().any(|r| {
        r["conversation"] == "GUIDED-VERIFY"
            && r["last"]["role"] == "tool"
            && r["last"].to_string().contains("tries: 3")
    });
    assert!(verifier_ran_demo, "the verifier reran the demo itself");
}
