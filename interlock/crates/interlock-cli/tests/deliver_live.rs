//! Opt-in live run: the design's walkthrough (the export-retry example) on
//! Claude Code with a real model, then delivered through G5 and G6 on the
//! fake `gh` and a local bare repository. It costs model calls, so it is
//! ignored by default:
//!
//! ```bash
//! INTERLOCK_LIVE=1 INTERLOCK_LIVE_OUT=evidence/forge/live-claude-code \
//!   cargo test -p interlock-cli --test deliver_live -- --ignored --nocapture
//! ```
//!
//! With INTERLOCK_LIVE_OUT set, the run's records are written there.

use std::path::{Path, PathBuf};
use std::process::Command;

use interlock_forge::testing::{FakeGh, Remote, python3_available};
use serde_json::{Value, json};

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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
#[ignore = "live: makes real model calls on Claude Code"]
fn the_export_retry_walkthrough_is_delivered_on_claude_code() {
    if std::env::var("INTERLOCK_LIVE").as_deref() != Ok("1") || !python3_available() {
        eprintln!("skipping: set INTERLOCK_LIVE=1 (and have python3) to run");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/export-retry");
    copy_dir(&example, &repo);
    let task = std::fs::read_to_string(repo.join("task.toml")).unwrap();
    let task = task.replacen("environment = ", "integration_required = true\nenvironment = ", 1);
    std::fs::write(repo.join("task.toml"), task).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "Exporter with retry"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let remote = Remote::create(&dir.path().join("remote.git"), &repo);
    let gh = FakeGh::install(&dir.path().join("gh"), &remote);

    let interlock = |args: &[&str]| -> (Option<i32>, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_interlock"))
            .args(args)
            .current_dir(&repo)
            .env("INTERLOCK_DB", repo.join(".interlock/state.db"))
            .env("INTERLOCK_FORGE_WAIT", "0s")
            .envs(gh.env())
            .env_remove("INTERLOCK_ATTEMPT")
            .env_remove("INTERLOCK_TOKEN")
            .output()
            .unwrap();
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&out.stderr).into_owned()));
        (out.status.code(), v)
    };
    interlock(&["init"]);
    interlock(&["task", "create", "task.toml"]);
    interlock(&[
        "grant",
        "create",
        "--principal",
        "operator",
        "--tasks",
        "export-retry",
        "--classes",
        "landing",
        "--landing",
        "operator",
        "--origin",
        "land the export fix once it is verified",
    ]);
    let (code, report) = interlock(&["run", "export-retry", "--host", "claude-code", "--timeout", "10m"]);
    let (_, log) = interlock(&["task", "log", "export-retry"]);
    let (_, operations) = interlock(&["integrate", "operations", "export-retry"]);
    let (_, status) = interlock(&["status", "export-retry"]);
    let landed = remote.show("main");
    let diff = git(&repo, &["diff", &base, "refs/interlock/export-retry/head"]);
    println!("{report:#}\n{log:#}");

    if let Some(out) = std::env::var_os("INTERLOCK_LIVE_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        let write = |name: &str, v: &Value| std::fs::write(out.join(name), serde_json::to_string_pretty(v).unwrap());
        write("run-report.json", &report).unwrap();
        write("transitions.json", &log).unwrap();
        write("operations.json", &operations).unwrap();
        write("status.json", &status).unwrap();
        write("gh-calls.json", &json!(gh.calls())).unwrap();
        let claude = std::env::var("INTERLOCK_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
        let version = Command::new(claude)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let summary = json!({
            "exit_code": code,
            "base": base,
            "remote_main_and_parents": landed,
            "claude_code": version.unwrap_or_default(),
        });
        write("summary.json", &summary).unwrap();
        std::fs::write(out.join("verified-head.diff"), format!("{diff}\n")).unwrap();
    }

    assert_eq!(code, Some(0), "{report:#}");
    assert_eq!(report["final_state"], "done");
    let signals: Vec<&str> = log.as_array().unwrap().iter().filter_map(|r| r["signal"].as_str()).collect();
    assert_eq!(signals[signals.len() - 2..], ["G5", "G6"]);
    assert!(landed.ends_with(&git(&repo, &["rev-parse", "refs/interlock/export-retry/head"])));
}
