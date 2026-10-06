//! Opt-in live delivery on real GitHub, through the real `gh`. It needs a
//! GitHub token and a separate test repository, and it opens and merges a
//! pull request there, so it is ignored by default and refuses to run
//! against anything but the repository it is given:
//!
//! ```bash
//! INTERLOCK_LIVE_GITHUB_REPO=<owner>/<test-repo> INTERLOCK_LIVE_OUT=evidence/forge/live-github \
//!   cargo test -p interlock-cli --test live_github -- --ignored --nocapture
//! ```
//!
//! `GH_TOKEN` must be a token `gh` accepts, with Contents and Pull requests
//! write on that repository. The task is verified by hand, as the forge
//! tests do, so no model is called. Delivery goes into a scratch base branch
//! the test creates, `interlock-live-<time>`, never the repository's default
//! branch. `INTERLOCK_FORGE_METHOD` picks squash or rebase where the
//! repository forbids merge commits.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// The repository this code lives in; a live test never delivers into it.
const THIS_REPOSITORY: &str = "tonysuss/autonomous-claude-workflow-bundle";

fn run(dir: &Path, program: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env_remove("INTERLOCK_ATTEMPT")
        .env_remove("INTERLOCK_TOKEN")
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text.trim().to_string())
}

fn ok(dir: &Path, program: &str, args: &[&str]) -> String {
    let (success, text) = run(dir, program, args);
    assert!(success, "{program} {args:?}: {text}");
    text
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut all =
        vec!["-c", "commit.gpgsign=false", "-c", "user.name=interlock-live", "-c", "user.email=live@interlock"];
    all.extend(args);
    ok(dir, "git", &all)
}

const TASK: &str = r#"
id = "{ID}"
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

#[test]
#[ignore = "live: opens and merges a pull request on GitHub"]
fn a_verified_task_is_delivered_to_real_github_at_its_pinned_head() {
    let Ok(target) = std::env::var("INTERLOCK_LIVE_GITHUB_REPO") else {
        eprintln!("skipping: set INTERLOCK_LIVE_GITHUB_REPO=<owner>/<test-repo> to run");
        return;
    };
    assert!(
        target.contains('/') && !target.eq_ignore_ascii_case(THIS_REPOSITORY),
        "INTERLOCK_LIVE_GITHUB_REPO must name a separate test repository, not {THIS_REPOSITORY}"
    );
    let here = std::env::temp_dir();
    let (authed, why) = run(&here, "gh", &["auth", "status"]);
    assert!(authed, "gh is not signed in: {why}");

    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let task_id = format!("live-{stamp}");
    let base_branch = format!("interlock-live-{stamp}");
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    ok(dir.path(), "gh", &["repo", "clone", &target, "repo", "--", "-q"]);

    // A scratch base branch holding the bug, so nothing lands on the default branch.
    git(&repo, &["checkout", "-q", "-b", &base_branch]);
    std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
    std::fs::write(repo.join("task.toml"), TASK.replace("{ID}", &task_id)).unwrap();
    git(&repo, &["add", "calc.py", "task.toml"]);
    git(&repo, &["commit", "-q", "-m", "interlock live test: add() subtracts"]);
    git(&repo, &["push", "-q", "-u", "origin", &base_branch]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    let db = repo.join(".interlock/state.db");
    let interlock = |args: &[&str]| -> (Option<i32>, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_interlock"))
            .args(args)
            .current_dir(&repo)
            .env("INTERLOCK_DB", &db)
            .env_remove("INTERLOCK_ATTEMPT")
            .env_remove("INTERLOCK_TOKEN")
            .env_remove("INTERLOCK_GH_BIN")
            .output()
            .unwrap();
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&out.stderr).into_owned()));
        (out.status.code(), v)
    };
    let must = |args: &[&str]| -> Value {
        let (code, v) = interlock(args);
        assert_eq!(code, Some(0), "interlock {args:?}: {v:#}");
        v
    };
    must(&["init"]);
    must(&["task", "create", "task.toml"]);
    must(&["task", "ready", &task_id, "--base", &base]);
    must(&[
        "grant",
        "create",
        "--principal",
        "operator",
        "--tasks",
        &task_id,
        "--classes",
        "landing",
        "--landing",
        "coordinator",
        "--origin",
        "live delivery test",
    ]);

    // The worker's output: the fix, as a tree built through a temporary index.
    let index = dir.path().join("index");
    let index_env = index.display().to_string();
    let with_index = |args: &[&str]| -> String {
        let out = Command::new("git").args(args).current_dir(&repo).env("GIT_INDEX_FILE", &index_env).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    with_index(&["read-tree", &base]);
    let fixed = dir.path().join("fixed.py");
    std::fs::write(&fixed, "def add(a, b):\n    return a + b\n").unwrap();
    let blob = git(&repo, &["hash-object", "-w", &fixed.display().to_string()]);
    with_index(&["update-index", "--cacheinfo", &format!("100644,{blob},calc.py")]);
    let tree = with_index(&["write-tree"]);

    let start = |role: &str, extra: &[&str]| -> (String, String) {
        let mut args = vec!["attempt", "start", &task_id, "--role", role, "--host", "test", "--mode", "headless"];
        args.extend(["--capabilities", CAPS]);
        args.extend(extra);
        let v = must(&args);
        (v["attempt"]["id"].as_str().unwrap().into(), v["token"].as_str().unwrap().into())
    };
    let (w, wt) = start("worker", &[]);
    must(&[
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
        "fix add",
    ]);
    must(&[
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
    let (v, vt) = start("verifier", &["--worktree", "auto"]);
    let check = must(&["check", "run", "--criterion", "verified", "--attempt", &v, "--token", &vt]);
    assert_eq!(check["passed"], true, "{check:#}");
    must(&[
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
    let advanced = must(&["advance", &task_id]);
    assert_eq!(advanced["status"]["task"]["state"], "verified", "{advanced:#}");

    // G5, the push, the pull request, readiness, the pinned merge, and G6, on GitHub.
    let (code, delivery) =
        interlock(&["integrate", "run", &task_id, "--base", &base_branch, "--wait", "15m", "--poll", "10s"]);
    let (_, log) = interlock(&["task", "log", &task_id]);
    let (_, operations) = interlock(&["integrate", "operations", &task_id]);
    let head = operations
        .as_array()
        .and_then(|ops| ops.iter().rev().find(|o| o["kind"] == "merge"))
        .and_then(|o| o["intent"]["expected_head_sha"].as_str())
        .unwrap_or_default()
        .to_string();
    git(&repo, &["fetch", "-q", "origin", &base_branch]);
    let landed = git(&repo, &["rev-parse", &format!("origin/{base_branch}")]);
    let landed_tree = git(&repo, &["rev-parse", &format!("origin/{base_branch}^{{tree}}")]);
    let (_, pr) = run(
        &repo,
        "gh",
        &[
            "pr",
            "list",
            "-R",
            &target,
            "--head",
            &format!("interlock/{task_id}"),
            "--state",
            "all",
            "--json",
            "number,state,headRefOid,baseRefName,mergeCommit",
        ],
    );
    println!("{delivery:#}\n{log:#}\n{pr}");

    if let Some(out) = std::env::var_os("INTERLOCK_LIVE_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        let write = |name: &str, v: &Value| std::fs::write(out.join(name), serde_json::to_string_pretty(v).unwrap());
        write("delivery.json", &delivery).unwrap();
        write("transitions.json", &log).unwrap();
        write("operations.json", &operations).unwrap();
        let pr: Value = serde_json::from_str(&pr).unwrap_or(Value::String(pr.clone()));
        let summary = json!({
            "repository": target, "base_branch": base_branch, "base": base, "verified_tree": tree,
            "pinned_head": head, "landed": landed, "landed_tree": landed_tree, "exit_code": code, "pull_request": pr,
        });
        write("summary.json", &summary).unwrap();
    }

    assert_eq!(code, Some(0), "{delivery:#}");
    assert_eq!(delivery["final_state"], "done", "{delivery:#}");
    let signals: Vec<&str> = log.as_array().unwrap().iter().filter_map(|r| r["signal"].as_str()).collect();
    assert_eq!(signals[signals.len() - 2..], ["G5", "G6"], "{log:#}");
    assert_eq!(landed_tree, tree, "the base branch now holds exactly the verified tree");
    assert!(!head.is_empty(), "the merge was pinned to a head: {operations:#}");

    // Leave the test repository as it was, apart from the closed pull request.
    let _ = run(&repo, "git", &["push", "-q", "origin", "--delete", &base_branch, &format!("interlock/{task_id}")]);
}
