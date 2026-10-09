//! The hook protocol shared by both hosts. Claude Code and Copilot CLI send
//! the same Claude-format payloads to interlock's hooks plugin; Copilot's own
//! hook config sends camelCase payloads, which are accepted too.

use std::path::PathBuf;

use interlock_core::classify::classify_shell;
use interlock_schema::ActionClass;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    PreToolUse { tool_name: String, input: Value, cwd: Option<PathBuf> },
    Stop { already_continued: bool, cwd: Option<PathBuf> },
    Other(String),
}

pub fn parse_event(payload: &Value) -> HookEvent {
    let cwd = payload["cwd"].as_str().map(PathBuf::from);
    let event = payload["hook_event_name"].as_str();
    let tool = payload["tool_name"].as_str().or_else(|| payload["toolName"].as_str());
    match (event, tool) {
        (Some("PreToolUse"), Some(t)) | (None, Some(t)) => {
            let raw = if payload.get("tool_input").is_some() { &payload["tool_input"] } else { &payload["toolArgs"] };
            // Copilot's native payloads may carry arguments as a JSON string.
            let input = match raw {
                Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({ "raw": s })),
                v => v.clone(),
            };
            HookEvent::PreToolUse { tool_name: t.to_string(), input, cwd }
        }
        (Some("Stop" | "SubagentStop"), _) | (None, None)
            if payload.get("stop_hook_active").is_some() || payload.get("stopReason").is_some() =>
        {
            HookEvent::Stop { already_continued: payload["stop_hook_active"].as_bool().unwrap_or(false), cwd }
        }
        (Some(e), _) => HookEvent::Other(e.to_string()),
        (None, None) => HookEvent::Other("unknown".into()),
    }
}

/// A tool call in host-neutral terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// Host-neutral tool name, for example `shell:cargo test` or `edit`.
    pub tool: String,
    pub class: ActionClass,
    /// Files the call would change.
    pub writes: Vec<PathBuf>,
    pub command: Option<String>,
    /// Whether interlock's per-call policy should judge it. Host-internal
    /// tools (todo lists, reading shell output) are left to the host's filters.
    pub governed: bool,
}

pub fn action_of(tool_name: &str, input: &Value) -> Action {
    let path_fields = ["file_path", "path", "notebook_path"];
    let writes: Vec<PathBuf> = path_fields.iter().filter_map(|f| input[f].as_str()).map(PathBuf::from).collect();
    let action = |tool: &str, class, writes: Vec<PathBuf>, command: Option<String>, governed| Action {
        tool: tool.to_string(),
        class,
        writes,
        command,
        governed,
    };
    match tool_name {
        "Bash" | "bash" | "shell" | "powershell" => {
            let command = input["command"].as_str().unwrap_or("").to_string();
            Action {
                tool: format!("shell:{}", command.trim()),
                class: classify_shell(&command),
                writes: vec![],
                command: Some(command),
                governed: true,
            }
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "create" | "edit" | "str_replace_editor" => {
            action("edit", ActionClass::LocalReversible, writes, None, true)
        }
        "Read" | "Grep" | "Glob" | "LS" | "view" | "grep" | "glob" => {
            action("read", ActionClass::Read, vec![], None, true)
        }
        "WebFetch" | "WebSearch" | "fetch" | "web_fetch" => action("web", ActionClass::Read, vec![], None, true),
        "Agent" | "Task" | "task" => action("agent", ActionClass::LocalReversible, vec![], None, false),
        t if t.starts_with("mcp__") => {
            let rest = t.trim_start_matches("mcp__").replacen("__", "/", 1);
            action(&format!("mcp:{rest}"), ActionClass::ExternalReversible, vec![], None, true)
        }
        t => action(t, ActionClass::LocalReversible, vec![], None, false),
    }
}

/// What a hook prints and how it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl HookResponse {
    pub fn allow() -> Self {
        HookResponse { stdout: String::new(), stderr: String::new(), exit_code: 0 }
    }

    /// Exit 2 denies on both hosts. Claude Code shows stderr to the agent;
    /// Copilot shows the reason from the JSON on stdout.
    pub fn deny(reason: &str) -> Self {
        let body = json!({ "permissionDecision": "deny", "permissionDecisionReason": reason });
        HookResponse { stdout: body.to_string(), stderr: reason.to_string(), exit_code: 2 }
    }

    /// Hands the decision to the person at the keyboard, for interactive sessions.
    pub fn ask(reason: &str, host: &str) -> Self {
        let body = if host == "copilot" {
            json!({ "permissionDecision": "ask", "permissionDecisionReason": reason })
        } else {
            json!({ "hookSpecificOutput": {
                "hookEventName": "PreToolUse", "permissionDecision": "ask", "permissionDecisionReason": reason } })
        };
        HookResponse { stdout: body.to_string(), stderr: String::new(), exit_code: 0 }
    }

    /// Holds the agent from stopping; the reason reaches it on both hosts.
    pub fn block_stop(reason: &str) -> Self {
        HookResponse {
            stdout: json!({ "decision": "block", "reason": reason }).to_string(),
            stderr: String::new(),
            exit_code: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_format_payloads_from_either_host() {
        let claude =
            json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/w"});
        assert_eq!(
            parse_event(&claude),
            HookEvent::PreToolUse { tool_name: "Bash".into(), input: json!({"command": "ls"}), cwd: Some("/w".into()) }
        );
        let stop = json!({"hook_event_name": "Stop", "stop_hook_active": true, "stop_reason": "end_turn"});
        assert_eq!(parse_event(&stop), HookEvent::Stop { already_continued: true, cwd: None });
    }

    #[test]
    fn parses_copilot_native_payloads() {
        let pre =
            json!({"sessionId": "s", "cwd": "/w", "toolName": "bash", "toolArgs": "{\"command\":\"git status\"}"});
        let HookEvent::PreToolUse { tool_name, input, .. } = parse_event(&pre) else { panic!() };
        assert_eq!((tool_name.as_str(), input["command"].as_str()), ("bash", Some("git status")));
        let stop = json!({"sessionId": "s", "stopReason": "end_turn", "stop_hook_active": false});
        assert_eq!(parse_event(&stop), HookEvent::Stop { already_continued: false, cwd: None });
    }

    #[test]
    fn maps_tools_to_host_neutral_actions() {
        let a = action_of("Bash", &json!({"command": "git push --force"}));
        assert_eq!((a.tool.as_str(), a.class), ("shell:git push --force", ActionClass::Irreversible));
        let a = action_of("Write", &json!({"file_path": "/w/src/a.rs", "content": "x"}));
        assert_eq!((a.tool.as_str(), a.writes.len()), ("edit", 1));
        let a = action_of("create", &json!({"path": "src/b.rs"}));
        assert_eq!(a.writes, vec![PathBuf::from("src/b.rs")]);
        let a = action_of("mcp__github__merge_pull_request", &json!({}));
        assert_eq!((a.tool.as_str(), a.class), ("mcp:github/merge_pull_request", ActionClass::ExternalReversible));
        assert!(!action_of("TodoWrite", &json!({})).governed);
    }

    #[test]
    fn deny_carries_the_reason_for_both_hosts() {
        let r = HookResponse::deny("not granted");
        assert_eq!(r.exit_code, 2);
        assert_eq!(r.stderr, "not granted");
        assert!(r.stdout.contains("\"permissionDecisionReason\":\"not granted\""));
    }
}
