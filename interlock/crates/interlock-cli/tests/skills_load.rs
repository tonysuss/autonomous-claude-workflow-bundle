//! Every generated skill loads on each host and reaches the model the way it
//! is meant to: model-invoked skills through the host's skill tool, the
//! people-only design skill as a slash command typed in the interactive
//! session. No model call is made: Copilot CLI runs offline against a scripted
//! model, and Claude Code reports what it loaded in its init event before its
//! first (unreachable) model request. A test skips when its host is missing,
//! unless INTERLOCK_REQUIRE_HOSTS=1.

mod guided_model;
mod hosts;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use guided_model::{Conversation, GuidedModel, tool};
use serde_json::{Value, json};

const SKILLS: [&str; 6] = ["design", "implement", "investigate", "review", "route", "verify"];
const MODEL_INVOKED: [&str; 5] = ["route", "investigate", "implement", "review", "verify"];

fn generate(target: &str, out: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_interlock"))
        .args(["skills", "generate", "--target", target, "--out", out.to_str().unwrap()])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "generating {target}");
}

/// The first line of a generated skill's body: text the model must receive when the skill is invoked.
fn first_line(skill_md: &Path) -> String {
    let text = std::fs::read_to_string(skill_md).unwrap();
    let body = text.splitn(3, "---\n").nth(2).unwrap();
    let line = body.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#')).unwrap();
    line.chars().take(60).collect()
}

/// Copilot offline, with a private home.
fn copilot_env(home: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("COPILOT_HOME", home.display().to_string()),
        ("COPILOT_OFFLINE", "true".into()),
        ("COPILOT_MODEL", "gpt-4.1".into()),
        ("COPILOT_AUTO_UPDATE", "false".into()),
        ("NO_PROXY", "127.0.0.1,localhost".into()),
        ("no_proxy", "127.0.0.1,localhost".into()),
    ]
}

fn copilot_cmd(copilot: &Path, dir: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(copilot);
    cmd.current_dir(dir).envs(copilot_env(home));
    cmd
}

fn git_repo(dir: &Path) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"],
    ] {
        assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success());
    }
}

#[test]
fn every_generated_skill_loads_on_copilot() {
    let Some(copilot) = hosts::copilot() else { return };
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    generate("copilot", repo.path());
    let out = copilot_cmd(&copilot, repo.path(), home.path()).args(["skill", "list", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listed: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let project: Vec<&Value> = listed.iter().filter(|s| s["source"] == "project").collect();
    let mut names: Vec<&str> = project.iter().filter_map(|s| s["name"].as_str()).collect();
    names.sort();
    let want: Vec<String> = SKILLS.iter().map(|s| format!("interlock-{s}")).collect();
    assert_eq!(names, want, "{listed:#?}");
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
fn every_model_invoked_skill_reaches_copilots_model_through_the_skill_tool() {
    let Some(copilot) = hosts::copilot() else { return };
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    generate("copilot", repo.path());
    git_repo(repo.path());
    let mut steps: Vec<_> =
        MODEL_INVOKED.iter().map(|s| tool("skill", json!({"skill": format!("interlock-{s}")}))).collect();
    steps.push(tool("skill", json!({"skill": "interlock-design"})));
    let model = GuidedModel::start(vec![Conversation {
        marker: "SKILL-TOOL-PROBE".into(),
        steps,
        reply: "Loaded each skill.".into(),
    }]);
    let out = copilot_cmd(&copilot, repo.path(), home.path())
        .args(["-p", "SKILL-TOOL-PROBE load each interlock skill", "--output-format", "json"])
        .args(["--allow-all-tools", "--no-ask-user", "--no-auto-update"])
        .env("COPILOT_PROVIDER_BASE_URL", model.base_url())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let events: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let results = hosts::copilot_tool_results(&events);
    let log = model.log();
    let kept = [("copilot-transcript.jsonl", hosts::jsonl(&events)), ("model-requests.jsonl", hosts::jsonl(&log))];
    hosts::keep("copilot-skill-tool", &kept);
    assert_eq!(results.len(), 6, "{results:#?}");
    for (i, s) in MODEL_INVOKED.iter().enumerate() {
        assert!(results[i].1, "interlock-{s}: {results:#?}");
        let line = first_line(&repo.path().join(format!(".github/skills/interlock-{s}/SKILL.md")));
        // What the model received after this call: the skill's own text.
        let next = log.iter().find(|r| r["tools_done"] == i + 1).unwrap_or_else(|| panic!("no request after {s}"));
        let seen = next["new_text"].as_str().unwrap_or_default();
        assert!(seen.contains(&line), "interlock-{s}'s body never reached the model; it saw: {seen}");
    }
    // The people-only skill is hidden from the agent (S1).
    assert!(!results[5].1 && results[5].2.contains("Skill not found"), "{:?}", results[5]);
}

#[test]
fn a_person_can_type_the_design_skill_in_copilots_interactive_session() {
    let Some(copilot) = hosts::copilot() else { return };
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    generate("copilot", repo.path());
    git_repo(repo.path());
    let model = GuidedModel::start(vec![Conversation {
        marker: "TUI-DESIGN-PROBE".into(),
        steps: vec![],
        reply: "Design noted.".into(),
    }]);
    let line = first_line(&repo.path().join(".github/skills/interlock-design/SKILL.md"));
    let stop = scratch.path().join("stop");
    let screen = scratch.path().join("screen.txt");
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets/pty_drive.py");
    let mut drive = Command::new("python3");
    drive
        .arg(&driver)
        .args(["90", screen.to_str().unwrap(), "--until", stop.to_str().unwrap()])
        // Enter at 6 s answers the folder-trust prompt; the -i prompt then runs.
        .args(["--type", "6", "\\r", "--"])
        .arg(&copilot)
        .args(["--allow-all-tools", "--no-auto-update", "-i", "/interlock-design fix-add TUI-DESIGN-PROBE"]);
    let mut child = drive
        .current_dir(repo.path())
        .envs(copilot_env(home.path()))
        .env("COPILOT_PROVIDER_BASE_URL", model.base_url())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let found = loop {
        let hit = model.log().into_iter().find(|r| {
            let t = r["new_text"].as_str().unwrap_or_default();
            t.contains("TUI-DESIGN-PROBE") && t.contains(&line)
        });
        if hit.is_some() || started.elapsed() > Duration::from_secs(85) || child.try_wait().unwrap().is_some() {
            break hit;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    std::fs::write(&stop, "").unwrap();
    let _ = child.wait();
    let found = found.unwrap_or_else(|| {
        let raw = std::fs::read(&screen).unwrap_or_default();
        panic!("the typed /interlock-design never reached the model; screen: {}", String::from_utf8_lossy(&raw))
    });
    let raw = String::from_utf8_lossy(&std::fs::read(&screen).unwrap_or_default()).into_owned();
    hosts::keep("copilot-tui-design", &[("model-requests.jsonl", hosts::jsonl(&model.log())), ("screen.txt", raw)]);
    let seen = found["new_text"].as_str().unwrap();
    assert!(seen.contains("/interlock-design"), "{seen}");
}

/// Skills without the runtime (the evaluation's skills condition): a plain session, in a repository with
/// no interlock store and no interlock attempt, gets a bug report that never mentions interlock. Copilot
/// lists the implement skill with a description that matches such a request, the skill tool invokes it,
/// and the model receives the steps for working without interlock. The scripted model decides to invoke
/// it, so this checks the plumbing and the text, not a model's choice.
#[test]
fn a_standalone_skill_reaches_copilots_model_in_a_plain_session_with_no_interlock() {
    let Some(copilot) = hosts::copilot() else { return };
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    generate("copilot", repo.path());
    git_repo(repo.path());
    assert!(!repo.path().join(".interlock/state.db").exists(), "generating skills makes no store");
    let marker = "PLAIN-BUG-REPORT";
    let model = GuidedModel::start(vec![Conversation {
        marker: marker.into(),
        steps: vec![tool("skill", json!({"skill": "interlock-implement"}))],
        reply: "Fixed, without interlock.".into(),
    }]);
    let mut cmd = copilot_cmd(&copilot, repo.path(), home.path());
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("INTERLOCK_")) {
        cmd.env_remove(k);
    }
    let out = cmd
        .args(["-p", &format!("{marker}: `total` counts the last line twice. Please fix this bug.")])
        .args(["--output-format", "json", "--allow-all-tools", "--no-ask-user", "--no-auto-update"])
        .env("COPILOT_PROVIDER_BASE_URL", model.base_url())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let events: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let results = hosts::copilot_tool_results(&events);
    let log = model.log();
    hosts::keep(
        "copilot-standalone-skill",
        &[("copilot-transcript.jsonl", hosts::jsonl(&events)), ("model-requests.jsonl", hosts::jsonl(&log))],
    );
    // What the model was offered: the implement skill, described by the requests it serves.
    let listed = log[0]["available_skills"].as_str().unwrap_or_else(|| panic!("no skill list: {:#?}", log[0]));
    let implement = listed.split("<skill>").find(|s| s.contains("<name>interlock-implement</name>")).unwrap();
    assert!(
        implement.contains("whenever you are asked to fix a bug, add or change a feature, or refactor code"),
        "{implement}"
    );
    assert!(!implement.contains("an interlock task&apos;s criteria"), "{implement}");
    // The skill tool loaded it, and its standalone steps reached the model.
    assert_eq!(results.len(), 1, "{results:#?}");
    assert!(results[0].1, "{results:#?}");
    let seen = log.iter().find(|r| r["tools_done"] == 1).expect("a request after the skill call")["new_text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    for want in [
        "## Without interlock",
        "nothing is recorded",
        "Run each check yourself",
        "`interlock where` exits 0",
        "`INTERLOCK_DB`",
    ] {
        assert!(seen.contains(want), "{want:?} never reached the model; it saw: {seen}");
    }
}

#[test]
fn every_generated_skill_and_the_verifier_load_on_claude_code() {
    let Some(claude) = hosts::claude() else { return };
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
    let slash = names("slash_commands");
    for s in SKILLS {
        assert!(skills.contains(&format!("interlock:{s}")), "interlock:{s} not loaded: {skills:?}");
    }
    // A person can type review and the people-only design skill.
    for s in ["design", "review"] {
        assert!(slash.contains(&format!("interlock:{s}")), "/interlock:{s} is not a command: {slash:?}");
    }
    assert!(names("agents").contains(&"interlock:verifier".to_string()), "{:?}", names("agents"));
    let plugins: Vec<&str> = init["plugins"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(plugins.contains(&"interlock"), "{plugins:?}");
}
