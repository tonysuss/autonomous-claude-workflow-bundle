//! The design's walkthrough, step 7, end to end: a bug-fix task that must be
//! delivered runs through `interlock run` on the real Copilot CLI (offline,
//! against a scripted model), then lands through G5 and G6 on a fake `gh`
//! whose repository is a local bare git repository. No network, no model
//! calls. Skips without a Copilot CLI binary or python3.

mod fake_model;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fake_model::{FakeModel, Script};
use interlock_forge::testing::{FakeGh, Remote, python3_available};
use serde_json::Value;

const TASK: &str = r#"
id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add() returns the difference instead of the sum"
integration_required = true

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
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn steps(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The worker fixes the bug and claims it; the verifier checks independently.
fn script() -> Script {
    Script {
        worker: vec![steps(&[
            "perl -pi -e 's/a - b/a + b/' calc.py",
            "interlock check run --criterion fixed",
            "interlock claim add --criterion fixed --strength tested --tree auto --ref 'sh check.sh'",
        ])],
        verifier: vec![steps(&[
            "interlock check run --criterion fixed",
            "interlock check run --criterion verified",
            "interlock assess add --criterion fixed --strength tested --ref 'sh check.sh'",
            "interlock assess add --criterion verified --strength observed --ref 'sh check.sh'",
        ])],
        on_block: vec![],
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
    copilot: PathBuf,
    remote: Remote,
    gh: FakeGh,
    base: String,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let Some(copilot) = copilot() else {
            eprintln!("skipping: no Copilot CLI binary");
            return None;
        };
        if !python3_available() {
            eprintln!("skipping: the fake gh needs python3");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(repo.join("check.sh"), "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'\n")
            .unwrap();
        std::fs::write(repo.join(".gitignore"), "__pycache__/\n").unwrap();
        std::fs::write(repo.join("task.toml"), TASK).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let remote = Remote::create(&dir.path().join("remote.git"), &repo);
        let gh = FakeGh::install(&dir.path().join("gh"), &remote);
        let f = Fixture { dir, home: tempfile::tempdir().unwrap(), copilot, remote, gh, base };
        f.interlock(&["init"], None);
        f.interlock(&["task", "create", "task.toml"], None);
        Some(f)
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn command(&self, args: &[&str], model: Option<&FakeModel>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_interlock"));
        cmd.args(args)
            .current_dir(self.repo())
            .env("INTERLOCK_DB", self.repo().join(".interlock/state.db"))
            .env("INTERLOCK_COPILOT_BIN", &self.copilot)
            .env("INTERLOCK_FORGE_WAIT", "0s")
            .envs(self.gh.env())
            .env("COPILOT_HOME", self.home.path())
            .env("COPILOT_OFFLINE", "true")
            .env("COPILOT_MODEL", "gpt-4.1")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        for k in ["INTERLOCK_ATTEMPT", "INTERLOCK_TOKEN", "INTERLOCK_TREE", "INTERLOCK_MODE", "INTERLOCK_FAULT"] {
            cmd.env_remove(k);
        }
        if let Some(m) = model {
            cmd.env("COPILOT_PROVIDER_BASE_URL", m.base_url());
        }
        cmd.output().unwrap()
    }

    fn interlock(&self, args: &[&str], model: Option<&FakeModel>) -> (Option<i32>, Value) {
        let out = self.command(args, model);
        let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            Value::String(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
        });
        (out.status.code(), json)
    }

    fn run(&self, model: &FakeModel) -> Output {
        // Copilot processes share a package cache; one session at a time keeps the tests steady.
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        self.command(&["run", "fix-add", "--host", "copilot", "--timeout", "120s"], Some(model))
    }

    fn report(out: &Output) -> Value {
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            Value::String(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
        })
    }

    fn grant_landing(&self) {
        let (code, out) = self.interlock(
            &[
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
            ],
            None,
        );
        assert_eq!(code, Some(0), "{out}");
    }

    fn signals(&self) -> Vec<String> {
        let (_, log) = self.interlock(&["task", "log", "fix-add"], None);
        log.as_array().unwrap().iter().map(|r| r["signal"].as_str().unwrap().to_string()).collect()
    }

    /// With INTERLOCK_EVIDENCE_OUT set, keeps the run report, the task log,
    /// the operations, every gh call, and what landed.
    fn record(&self, test: &str, report: &Value) {
        let Some(out) = std::env::var_os("INTERLOCK_EVIDENCE_OUT").map(PathBuf::from) else { return };
        let dir = out.join(test);
        std::fs::create_dir_all(&dir).unwrap();
        let (_, log) = self.interlock(&["task", "log", "fix-add"], None);
        let (_, ops) = self.interlock(&["integrate", "operations", "fix-add"], None);
        let landed = serde_json::json!({ "base": self.base, "main_and_parents": self.remote.show("main") });
        for (name, v) in [
            ("run-report", report.clone()),
            ("transitions", log),
            ("operations", ops),
            ("gh-calls", serde_json::json!(self.gh.calls())),
            ("remote", landed),
        ] {
            std::fs::write(dir.join(format!("{name}.json")), serde_json::to_string_pretty(&v).unwrap()).unwrap();
        }
    }
}

#[test]
fn walkthrough_step_7_a_verified_bug_fix_lands_through_g5_and_g6_on_copilot() {
    let Some(f) = Fixture::new() else { return };
    f.grant_landing();
    let model = FakeModel::start(script());
    let out = f.run(&model);
    let report = Fixture::report(&out);
    f.record("copilot-walkthrough-step-7", &report);
    assert_eq!(out.status.code(), Some(0), "{report:#}");
    assert_eq!(report["final_state"], "done", "{report:#}");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G5", "G6"]);

    // What landed is exactly the verified tree, on the snapshot base, merged pinned to that head.
    let (_, task) = f.interlock(&["task", "show", "fix-add"], None);
    let tree = task["current_tree"].as_str().unwrap();
    let (_, ops) = f.interlock(&["integrate", "operations", "fix-add"], None);
    let merge = ops.as_array().unwrap().iter().find(|o| o["kind"] == "merge").unwrap();
    assert_eq!(merge["state"], "confirmed");
    let head = merge["intent"]["expected_head_sha"].as_str().unwrap();
    assert_eq!(git(&f.repo(), &["rev-parse", &format!("{head}^{{tree}}")]), tree);
    assert_eq!(git(&f.repo(), &["rev-parse", &format!("{head}^")]), f.base);
    let merges = f.gh.calls_to(&["pr", "merge"]);
    assert_eq!(merges, [vec!["pr", "merge", "1", "--merge", "--match-head-commit", head]]);
    let main = f.remote.tip("main").unwrap();
    assert_eq!(f.remote.show("main"), format!("{main} {} {head}", f.base));
    assert!(f.remote.file("main", "calc.py").contains("a + b"));

    // The user's checkout never moved.
    assert_eq!(git(&f.repo(), &["rev-parse", "main"]), f.base);
    assert!(std::fs::read_to_string(f.repo().join("calc.py")).unwrap().contains("a - b"));
}

#[test]
fn without_landing_authority_interlock_run_blocks_at_verified_with_the_reason() {
    let Some(f) = Fixture::new() else { return };
    let model = FakeModel::start(script());
    let out = f.run(&model);
    let report = Fixture::report(&out);
    f.record("copilot-no-landing-authority", &report);
    assert_eq!(out.status.code(), Some(5), "{report:#}");
    assert_eq!(report["final_state"], "blocked");
    assert_eq!(report["stopped_because"], "blocked: no landing authority is granted for this task");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "block"]);
    let (_, status) = f.interlock(&["status", "fix-add"], None);
    assert_eq!(status["next_moves"][0]["to"], "verified");
    assert!(f.gh.calls().is_empty(), "the forge was never touched");
}

#[test]
fn a_run_killed_right_after_the_merge_call_is_finished_by_the_next_run() {
    let Some(f) = Fixture::new() else { return };
    f.grant_landing();
    f.gh.fault("merge", "crash", 1);
    let model = FakeModel::start(script());
    let out = f.run(&model);
    assert_eq!(out.status.signal(), Some(9), "the fake gh killed interlock mid-merge: {out:?}");
    assert_ne!(f.remote.tip("main").unwrap(), f.base, "the forge merged");
    let (_, status) = f.interlock(&["status", "fix-add"], None);
    assert_eq!(status["task"]["state"], "integrating");
    let model_calls = model.requests().len();
    assert!(model_calls > 0);

    let out = f.run(&model);
    let report = Fixture::report(&out);
    f.record("copilot-run-killed-mid-merge-then-restarted", &report);
    assert_eq!(out.status.code(), Some(0), "{report:#}");
    assert_eq!(report["final_state"], "done");
    assert_eq!(report["sessions"].as_array().unwrap().len(), 0, "no session was needed to finish");
    assert_eq!(model.requests().len(), model_calls, "and no model call");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G5", "G6"]);
    assert_eq!(f.gh.calls_to(&["pr", "merge"]).len(), 1, "it never merged twice");
}
