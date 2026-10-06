//! Every generated skill loads on each host. Neither test makes a model call:
//! Copilot CLI lists skills offline, and Claude Code reports what it loaded in
//! its init event before its first (here unreachable) model request. Each test
//! skips when its host's binary is not available.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const SKILLS: [&str; 6] = ["design", "implement", "investigate", "review", "route", "verify"];

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

fn binary(var: &str, name: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.exists()).or_else(|| on_path(name))
}

fn generate(target: &str, out: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_interlock"))
        .args(["skills", "generate", "--target", target, "--out", out.to_str().unwrap()])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "generating {target}");
}

#[test]
fn every_generated_skill_loads_on_copilot() {
    let Some(copilot) = binary("INTERLOCK_COPILOT_BIN", "copilot") else {
        eprintln!("skipping: no Copilot CLI binary");
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    generate("copilot", repo.path());
    let out = Command::new(&copilot)
        .args(["skill", "list", "--json"])
        .current_dir(repo.path())
        .env("COPILOT_HOME", home.path())
        .env("COPILOT_OFFLINE", "true")
        .env("COPILOT_AUTO_UPDATE", "false")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listed: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let project: Vec<&Value> = listed.iter().filter(|s| s["source"] == "project").collect();
    let mut names: Vec<&str> = project.iter().filter_map(|s| s["name"].as_str()).collect();
    names.sort();
    assert_eq!(names, SKILLS, "{listed:#?}");
    for s in project {
        assert_eq!(s["enabled"], true, "{s}");
        let dir = Path::new(s["path"].as_str().unwrap());
        assert!(dir.ends_with(Path::new(".github/skills").join(s["name"].as_str().unwrap())), "{s}");
        for key in ["error", "errors", "warning", "warnings"] {
            assert!(s.get(key).is_none_or(|v| v.is_null() || v.as_array().is_some_and(|a| a.is_empty())), "{s}");
        }
    }
}

#[test]
fn every_generated_skill_and_the_verifier_load_on_claude_code() {
    let Some(claude) = binary("INTERLOCK_CLAUDE_BIN", "claude") else {
        eprintln!("skipping: no Claude Code binary");
        return;
    };
    let plugin = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    generate("claude-code", plugin.path());
    // A closed local port: Claude Code reports what it loaded, then its model request fails.
    let mut child = Command::new(&claude)
        .args(["-p", "--output-format", "stream-json", "--verbose", "--max-turns", "1", "--no-session-persistence"])
        .args(["--setting-sources", "", "--plugin-dir", plugin.path().to_str().unwrap()])
        .current_dir(work.path())
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .env("CLAUDE_CODE_MAX_RETRIES", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(b"interlock skill load probe");
    }
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() && started.elapsed() < Duration::from_secs(90) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let init: Value = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "system" && e["subtype"] == "init")
        .unwrap_or_else(|| panic!("no init event: {stdout}"));
    let names = |key: &str| -> Vec<String> {
        init[key].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect()
    };
    let skills = names("skills");
    for s in SKILLS {
        assert!(skills.contains(&format!("interlock:{s}")), "interlock:{s} not loaded: {skills:?}");
    }
    assert!(names("agents").contains(&"interlock:verifier".to_string()), "{:?}", names("agents"));
    let plugins: Vec<&str> = init["plugins"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(plugins.contains(&"interlock"), "{plugins:?}");
}
