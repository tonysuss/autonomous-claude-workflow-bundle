//! `interlock integrate run`, `interlock reconcile` and the reconcile at the
//! start of `interlock run`, as processes, against a fake `gh` and a local
//! bare repository. The crash test lets the fake kill interlock right after
//! it merges, before interlock hears back. They skip without python3.

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use interlock_forge::testing::{FakeGh, Remote, python3_available};
use serde_json::Value;

const TASK: &str = r#"
id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add() returns the difference instead of the sum"
integration_required = true

[scope]
paths = ["calc.py"]

[[criterion]]
id = "fixed"
statement = "add(2, 3) returns 5"
min_strength = "tested"
producer = "self"

[[criterion]]
id = "verified"
statement = "An independent check passes"
check = "python3 -c 'import calc; assert calc.add(2, 3) == 5'"
min_strength = "observed"
producer = "independent"
"#;

const CAPS: &str = "session_start,session_collect,session_cancel,tool_restriction";

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    dir: tempfile::TempDir,
    remote: Remote,
    gh: FakeGh,
    base: String,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        if !python3_available() {
            eprintln!("skipping: the fake gh needs python3");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"], &[]);
        std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(repo.join("task.toml"), TASK).unwrap();
        git(&repo, &["add", "-A"], &[]);
        git(&repo, &["commit", "-q", "-m", "init"], &[]);
        let base = git(&repo, &["rev-parse", "HEAD"], &[]);
        let remote = Remote::create(&dir.path().join("remote.git"), &repo);
        let gh = FakeGh::install(&dir.path().join("gh"), &remote);
        let f = Fixture { dir, remote, gh, base };
        f.ok(&["init"]);
        f.ok(&["task", "create", "task.toml"]);
        f.ok(&["task", "ready", "fix-add", "--base", &f.base]);
        Some(f)
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_interlock"));
        cmd.args(args)
            .current_dir(self.repo())
            .env("INTERLOCK_DB", self.repo().join(".interlock/state.db"))
            .env("INTERLOCK_FORGE_WAIT", "0s")
            .envs(self.gh.env());
        for k in ["INTERLOCK_ATTEMPT", "INTERLOCK_TOKEN", "INTERLOCK_TREE", "INTERLOCK_MODE", "INTERLOCK_FAULT"] {
            cmd.env_remove(k);
        }
        cmd.envs(env.iter().copied());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args, &[]).output().unwrap()
    }

    /// The pre-tool-use hook's answer to a shell command inside attempt `attempt`.
    fn hook(&self, attempt: &str, command: &str) -> Output {
        let mut child = self
            .command(&["hook", "pre-tool-use"], &[("INTERLOCK_ATTEMPT", attempt)])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": command }
        });
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), payload.to_string().as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// A tree like `from`'s with `path` = `content`.
    fn tree_with(&self, from: &str, path: &str, content: &str) -> String {
        let repo = self.repo();
        let index = repo.join(".git/test-index").display().to_string();
        let env = [("GIT_INDEX_FILE", index.as_str())];
        git(&repo, &["read-tree", from], &env);
        std::fs::write(repo.join(".git/blob.tmp"), content).unwrap();
        let blob = git(&repo, &["hash-object", "-w", ".git/blob.tmp"], &[]);
        git(&repo, &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], &env);
        git(&repo, &["write-tree"], &env)
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        let out = self.run(args);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            Value::String(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
        });
        (out.status.code(), v)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }

    fn attempt(&self, role: &str) -> (String, String) {
        self.attempt_with(role, &[])
    }

    fn attempt_with(&self, role: &str, extra: &[&str]) -> (String, String) {
        let mut args = vec![
            "attempt",
            "start",
            "fix-add",
            "--role",
            role,
            "--host",
            "test",
            "--mode",
            "headless",
            "--capabilities",
            CAPS,
        ];
        args.extend(extra);
        let v = self.ok(&args);
        (v["attempt"]["id"].as_str().unwrap().into(), v["token"].as_str().unwrap().into())
    }

    /// Drives one worker and one verifier with the CLI, as an interactive session would.
    fn verify(&self) -> String {
        let repo = self.repo();
        let index = repo.join(".git/test-index").display().to_string();
        let env = [("GIT_INDEX_FILE", index.as_str())];
        git(&repo, &["read-tree", &self.base], &env);
        std::fs::write(repo.join(".git/fixed.py"), "def add(a, b):\n    return a + b\n").unwrap();
        let blob = git(&repo, &["hash-object", "-w", ".git/fixed.py"], &[]);
        git(&repo, &["update-index", "--cacheinfo", &format!("100644,{blob},calc.py")], &env);
        let tree = git(&repo, &["write-tree"], &env);

        let (w, wt) = self.attempt("worker");
        self.ok(&[
            "result",
            "submit",
            "--attempt",
            &w,
            "--token",
            &wt,
            "--epoch",
            "1",
            "--tree",
            &tree,
            "--summary",
            "fixed add",
        ]);
        self.ok(&[
            "claim",
            "add",
            "--attempt",
            &w,
            "--token",
            &wt,
            "--criterion",
            "fixed",
            "--strength",
            "tested",
            "--tree",
            &tree,
        ]);
        // The verifier's own worktree holds the output; interlock runs the check
        // there, so the assessment counts though no host names the verifier.
        let (v, vt) = self.attempt_with("verifier", &["--worktree", "auto"]);
        let run = self.ok(&["check", "run", "--criterion", "verified", "--attempt", &v, "--token", &vt]);
        assert_eq!(run["passed"], true, "{run:#}");
        self.ok(&[
            "assess",
            "add",
            "--attempt",
            &v,
            "--token",
            &vt,
            "--criterion",
            "verified",
            "--strength",
            "observed",
            "--tree",
            &tree,
        ]);
        let advanced = self.ok(&["advance", "fix-add"]);
        assert_eq!(advanced["status"]["task"]["state"], "verified", "{advanced:#}");
        tree
    }

    fn grant_landing(&self) {
        self.ok(&[
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
            "land the add fix once it is verified",
        ]);
    }

    fn state(&self) -> String {
        self.ok(&["status", "fix-add"])["task"]["state"].as_str().unwrap().to_string()
    }

    /// With INTERLOCK_EVIDENCE_OUT set, keeps a command's JSON output as evidence.
    fn record(&self, test: &str, name: &str, v: &Value) {
        if let Some(out) = std::env::var_os("INTERLOCK_EVIDENCE_OUT").map(PathBuf::from) {
            let dir = out.join(test);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.json")), serde_json::to_string_pretty(v).unwrap()).unwrap();
        }
    }

    /// Keeps the task log, the operations and every gh call as evidence.
    fn record_end(&self, test: &str) {
        self.record(test, "transitions", &self.ok(&["task", "log", "fix-add"]));
        self.record(test, "operations", &self.ok(&["integrate", "operations", "fix-add"]));
        self.record(test, "gh-calls", &serde_json::json!(self.gh.calls()));
    }

    fn signals(&self) -> Vec<String> {
        let log = self.ok(&["task", "log", "fix-add"]);
        log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap().to_string()).collect()
    }

    fn operations(&self) -> Vec<(String, String)> {
        let ops = self.ok(&["integrate", "operations", "fix-add"]);
        ops.as_array()
            .unwrap()
            .iter()
            .map(|o| (o["kind"].as_str().unwrap().into(), o["state"].as_str().unwrap().into()))
            .collect()
    }

    /// Integrates until the fake kills interlock right after the merge call.
    fn crash_during_merge(&self, test: &str) {
        self.verify();
        self.grant_landing();
        self.gh.fault("merge", "crash", 1);
        let out = self.run(&["integrate", "run", "fix-add"]);
        let killed = serde_json::json!({
            "signal": out.status.signal(),
            "stdout": String::from_utf8_lossy(&out.stdout),
            "stderr": String::from_utf8_lossy(&out.stderr),
            "remote_main_after": self.remote.show("main"),
            "operations_after": self.ok(&["integrate", "operations", "fix-add"]),
        });
        self.record(test, "1-integrate-run-killed", &killed);
        assert_eq!(out.status.signal(), Some(9), "interlock was killed mid-merge: {out:?}");
        // The forge merged; interlock never heard. Its row says started, nothing more.
        let main = self.remote.tip("main").unwrap();
        assert_ne!(main, self.base, "the merge happened on the forge");
        assert_eq!(self.state(), "integrating");
        assert_eq!(
            self.operations(),
            [("merge".to_string(), "started".to_string()), ("open_pr".into(), "confirmed".into())]
        );
    }
}

#[test]
fn invariant_7_a_crash_right_after_the_merge_call_is_reconciled_to_done() {
    let Some(f) = Fixture::new() else { return };
    let test = "invariant-7-crash-then-reconcile";
    f.crash_during_merge(test);
    let (code, out) = f.json(&["reconcile", "fix-add"]);
    f.record(test, "2-reconcile", &out);
    f.record_end(test);
    assert_eq!(code, Some(0), "{out:#}");
    let r = &out["reconciled"][0];
    assert_eq!(
        (r["kind"].as_str(), r["before"].as_str(), r["after"].as_str()),
        (Some("merge"), Some("started"), Some("confirmed"))
    );
    assert_eq!(r["verdict"]["report"]["outcome"], "merged");
    let head = r["verdict"]["report"]["head_sha"].as_str().unwrap();
    assert_eq!(f.remote.show("main"), format!("{} {} {head}", f.remote.tip("main").unwrap(), f.base));
    assert_eq!(f.state(), "done");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G5", "G6"]);
    assert_eq!(f.gh.calls_to(&["pr", "merge"]).len(), 1, "reconcile asked; it never merged again");
}

#[test]
fn invariant_7_a_restarted_run_reconciles_before_anything_else() {
    let Some(f) = Fixture::new() else { return };
    let test = "invariant-7-crash-then-restarted-run";
    f.crash_during_merge(test);
    // A new controller starts. No host session is needed: reconcile settles the task first.
    let (code, report) = f.json(&["run", "fix-add", "--host", "copilot"]);
    f.record(test, "2-interlock-run", &report);
    f.record_end(test);
    assert_eq!(code, Some(0), "{report:#}");
    assert_eq!(report["final_state"], "done");
    assert_eq!(report["sessions"].as_array().unwrap().len(), 0);
    let reconciled: Vec<&str> = report["reconciled"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(reconciled.iter().any(|id| id.starts_with("op-")), "{report:#}");
    assert_eq!(f.signals().last().map(String::as_str), Some("G6"));
}

#[test]
fn integrate_run_delivers_and_exits_zero() {
    let Some(f) = Fixture::new() else { return };
    let tree = f.verify();
    f.grant_landing();
    let (code, d) = f.json(&["integrate", "run", "fix-add"]);
    f.record("integrate-run-delivers", "integrate-run", &d);
    f.record("integrate-run-delivers", "remote-main", &Value::String(f.remote.show("main")));
    f.record_end("integrate-run-delivers");
    assert_eq!(code, Some(0), "{d:#}");
    assert_eq!(d["final_state"], "done");
    assert_eq!(d["pull_request"], 1);
    let head = d["head"].as_str().unwrap();
    assert_eq!(git(&f.repo(), &["rev-parse", &format!("{head}^{{tree}}")], &[]), tree);
    assert_eq!(f.remote.file("main", "calc.py"), "def add(a, b):\n    return a + b");
}

#[test]
fn p4_without_landing_authority_the_task_blocks_at_verified() {
    let Some(f) = Fixture::new() else { return };
    f.verify();
    let (code, d) = f.json(&["integrate", "run", "fix-add"]);
    let status = f.ok(&["status", "fix-add"]);
    f.record("p4-no-landing-authority", "integrate-run", &d);
    f.record("p4-no-landing-authority", "status", &status);
    f.record_end("p4-no-landing-authority");
    assert_eq!(code, Some(5), "{d:#}");
    assert_eq!(d["final_state"], "blocked");
    assert_eq!(status["task"]["blocked_reason"], "no landing authority is granted for this task");
    assert_eq!(status["next_moves"][0]["to"], "verified", "unblocking returns it to verified");
    assert!(f.gh.calls().is_empty());
}

#[test]
fn p4_a_moved_head_refuses_the_merge() {
    let Some(f) = Fixture::new() else { return };
    f.verify();
    f.grant_landing();
    f.gh.fault("merge", "move_head", 1);
    let (code, d) = f.json(&["integrate", "run", "fix-add"]);
    f.record("p4-moved-head", "integrate-run", &d);
    f.record(
        "p4-moved-head",
        "remote",
        &serde_json::json!({
            "main": f.remote.show("main"),
            "interlock/fix-add": f.remote.show("interlock/fix-add"),
        }),
    );
    f.record_end("p4-moved-head");
    assert_eq!(code, Some(5), "{d:#}");
    assert_eq!(d["final_state"], "awaiting_verification");
    let why = d["stopped_because"].as_str().unwrap();
    assert!(why.contains("moved"), "{why}");
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()), "main never moved");
    assert_eq!(f.signals().last().map(String::as_str), Some("R2"));
}

#[test]
fn p4_a_changed_base_invalidates_the_evidence() {
    let Some(f) = Fixture::new() else { return };
    let tree = f.verify();
    f.grant_landing();
    f.remote.commit_file("main", "NOTES.md", "someone else's change\n");
    let (code, d) = f.json(&["integrate", "run", "fix-add"]);
    let status = f.ok(&["status", "fix-add"]);
    f.record("p4-changed-base", "integrate-run", &d);
    f.record("p4-changed-base", "status-after", &status);
    f.record("p4-changed-base", "verified-tree-before", &Value::String(tree.clone()));
    f.record_end("p4-changed-base");
    assert_eq!(code, Some(5), "{d:#}");
    assert_eq!(d["final_state"], "awaiting_verification");
    assert_ne!(status["task"]["current_tree"], tree.as_str());
    assert_eq!(status["evidence"]["all_pass"], false);
    assert!(status["evidence"]["criteria"].as_array().unwrap().iter().all(|c| c["stale"].as_u64() > Some(0)));
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty());
}

/// Review r6b: the tree changes while integrating; `interlock run` must apply
/// R2 instead of pushing and merging the unverified tree.
#[test]
fn r6_a_tree_recorded_while_integrating_is_r2_under_interlock_run() {
    let Some(f) = Fixture::new() else { return };
    let tree = f.verify();
    f.grant_landing();
    f.gh.checks("pending");
    let (_, d) = f.json(&["integrate", "run", "fix-add"]);
    assert_eq!(d["final_state"], "integrating", "{d:#}");
    let unverified = f.tree_with(&tree, "evil.py", "print('unverified')\n");
    f.ok(&["task", "tree", "fix-add", "--tree", &unverified]);
    f.gh.checks("pass");

    let (code, report) = f.json(&["run", "fix-add", "--host", "copilot", "--max-sessions", "0"]);
    f.record("r6-tree-changed-while-integrating", "interlock-run", &report);
    f.record_end("r6-tree-changed-while-integrating");
    assert_eq!(code, Some(5), "{report:#}");
    assert_eq!(report["final_state"], "awaiting_verification", "{report:#}");
    assert!(f.signals().contains(&"R2".to_string()));
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty(), "nothing was merged");
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()));
}

/// Review item 11: after a moved-head R2 the evidence still covers the
/// verified tree, so `interlock run` advances (G4) instead of spending a
/// verifier session, then refuses to overwrite the unverified push.
#[test]
fn a_moved_head_r2_needs_no_verifier_session() {
    let Some(f) = Fixture::new() else { return };
    f.verify();
    f.grant_landing();
    // Someone pushes to the pull request's branch while the merge request is in flight.
    f.gh.fault("merge", "move_head", 1);

    let (code, report) = f.json(&["run", "fix-add", "--host", "copilot", "--max-sessions", "0"]);
    assert_eq!(code, Some(5), "{report:#}");
    assert_eq!(report["final_state"], "blocked", "{report:#}");
    assert_eq!(report["sessions"].as_array().unwrap().len(), 0);
    let signals = f.signals();
    assert_eq!(signals[signals.len() - 5..], ["G5", "R2", "G4", "G5", "block"]);
    let status = f.ok(&["status", "fix-add"]);
    assert!(status["task"]["blocked_reason"].as_str().unwrap().contains("which interlock did not push"));
}

/// Review r3: an agent inside an attempt cannot point interlock at a forged
/// `gh` and reconcile its way to G6. The hook asks the operator (denies
/// headless), and the commands themselves refuse inside an attempt.
#[test]
fn r3_forge_commands_are_the_operators_inside_an_attempt() {
    let Some(f) = Fixture::new() else { return };
    f.verify();
    f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.json(&["integrate", "run", "fix-add"]).1["final_state"], "integrating");
    // A worker running on another task.
    std::fs::write(f.repo().join("other.toml"), TASK.replace("id = \"fix-add\"", "id = \"other\"")).unwrap();
    f.ok(&["task", "create", "other.toml"]);
    f.ok(&["task", "ready", "other", "--base", &f.base]);
    let a = f.ok(&[
        "attempt",
        "start",
        "other",
        "--role",
        "worker",
        "--host",
        "test",
        "--mode",
        "headless",
        "--capabilities",
        CAPS,
    ]);
    let attempt = a["attempt"]["id"].as_str().unwrap().to_string();

    let push_to_main = ["git", "push origin HEAD:main"].join(" ");
    for command in [
        "INTERLOCK_GH_BIN=/tmp/evil-gh interlock reconcile",
        "interlock reconcile fix-add",
        "interlock integrate run fix-add",
        push_to_main.as_str(),
        "gh pr merge 1 --merge",
    ] {
        let out = f.hook(&attempt, command);
        assert_eq!(out.status.code(), Some(2), "{command}: {}", String::from_utf8_lossy(&out.stdout));
    }
    for args in [
        &["reconcile"][..],
        &["reconcile", "fix-add"],
        &["integrate", "run", "fix-add"],
        &["integrate", "confirm", "fix-add", "--operation", "op-x", "--merged", "abcdef1"],
        &["run", "fix-add", "--host", "copilot"],
    ] {
        let env = [("INTERLOCK_ATTEMPT", attempt.as_str()), ("INTERLOCK_GH_BIN", "/tmp/evil-gh")];
        let out = f.command(args, &env).output().unwrap();
        assert_ne!(out.status.code(), Some(0), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("is the operator's"), "{args:?}");
    }
    let env = [("INTERLOCK_ATTEMPT", attempt.as_str())];
    let listed = f.command(&["integrate", "operations", "fix-add"], &env).output().unwrap();
    assert!(listed.status.success(), "reading operations is fine");
    let status = f.ok(&["status", "fix-add"]);
    assert_eq!(status["task"]["state"], "integrating", "nothing landed");
}
