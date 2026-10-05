//! Whole `interlock run` passes on the real Copilot CLI, in offline mode
//! against a scripted model. They need a Copilot CLI binary: set
//! INTERLOCK_COPILOT_BIN or put `copilot` on PATH. Without one they skip.

mod fake_model;

use std::path::{Path, PathBuf};
use std::process::Command;

use fake_model::{FakeModel, Script};
use serde_json::Value;

const FIX: &str = "sed -i 's/a - b/a + b/' calc.py";
const CLAIM: &str = "interlock claim add --criterion fixed --strength tested --tree auto --ref 'sh check.sh'";

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

[[criterion]]
id = "verified"
statement = "An independent run of the check passes"
check = "sh check.sh"
min_strength = "observed"
producer = "independent"
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
        let Some(copilot) = copilot() else {
            eprintln!("skipping: no Copilot CLI binary");
            return None;
        };
        let repo = tempfile::tempdir().unwrap();
        let p = repo.path();
        std::fs::write(p.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(p.join("check.sh"), "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'\n").unwrap();
        std::fs::write(p.join(".gitignore"), "__pycache__/\n").unwrap();
        std::fs::write(p.join("task.toml"), TASK).unwrap();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["add", "-A"]);
        git(p, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let f = Fixture { repo, home: tempfile::tempdir().unwrap(), copilot };
        f.interlock(&["init"], None);
        f.interlock(&["task", "create", "task.toml"], None);
        Some(f)
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
        "sh check.sh",
        "interlock assess add --criterion fixed --strength tested --ref 'sh check.sh'",
        "interlock assess add --criterion verified --strength observed --ref 'sh check.sh'",
    ])
}

#[test]
fn a_bug_fix_runs_to_done_on_copilot() {
    let Some(f) = Fixture::new() else { return };
    let model = FakeModel::start(Script {
        worker: vec![steps(&[FIX, CLAIM])],
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
        on_block: steps(&[CLAIM]),
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

#[test]
fn a_failed_verification_sends_the_work_back_and_the_rework_is_verified() {
    let Some(f) = Fixture::new() else { return };
    let verifier_fail = steps(&[
        "sh check.sh",
        "interlock assess add --criterion fixed --strength failed --ref 'sh check.sh' --note 'add(2, 3) returned -1'",
        "interlock assess add --criterion verified --strength failed --ref 'sh check.sh'",
    ]);
    let model = FakeModel::start(Script {
        // The first worker claims a pass without fixing anything.
        worker: vec![steps(&[CLAIM]), steps(&[FIX, CLAIM])],
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
