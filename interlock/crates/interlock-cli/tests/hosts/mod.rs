//! Host binaries for the tests that drive a real host. A test skips when its
//! host is missing, unless INTERLOCK_REQUIRE_HOSTS=1: then a missing host
//! fails the test, so a run kept as evidence cannot pass by skipping.

#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;

fn find(var: &str, name: &str) -> Option<PathBuf> {
    let on_path = || std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file());
    let found = std::env::var_os(var).map(PathBuf::from).filter(|p| p.exists()).or_else(on_path);
    if found.is_none() {
        assert_ne!(
            std::env::var("INTERLOCK_REQUIRE_HOSTS").as_deref(),
            Ok("1"),
            "INTERLOCK_REQUIRE_HOSTS=1 and no {name} binary: set {var} or put {name} on PATH"
        );
        eprintln!("skipping: no {name} binary");
    }
    found
}

pub fn copilot() -> Option<PathBuf> {
    find("INTERLOCK_COPILOT_BIN", "copilot")
}

pub fn claude() -> Option<PathBuf> {
    find("INTERLOCK_CLAUDE_BIN", "claude")
}

/// Keeps text files for a run under $INTERLOCK_EVIDENCE_DIR/<name>/, when that is set.
pub fn keep(name: &str, files: &[(&str, String)]) {
    let Some(dir) = std::env::var_os("INTERLOCK_EVIDENCE_DIR").map(PathBuf::from) else { return };
    let dir = dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, text) in files {
        std::fs::write(dir.join(file), text).unwrap();
    }
}

/// One JSON value per line.
pub fn jsonl(values: &[Value]) -> String {
    values.iter().map(|v| v.to_string() + "\n").collect()
}

/// Tool results in a Copilot JSONL stream, in order: the tool's name, whether
/// it succeeded, and its result or error text.
pub fn copilot_tool_results(events: &[Value]) -> Vec<(String, bool, String)> {
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
