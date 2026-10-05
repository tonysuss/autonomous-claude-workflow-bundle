//! Claude Code.

use interlock_core::capability::{CONTRACT_VERSION, Capability as C};

use crate::{
    AuthStatus, CommandPlan, Host, HostReport, Probe, SessionSpec, SessionSummary, flag_capability, json_lines,
    plugin_hook_capability, process_capability,
};

pub struct ClaudeCode;

const BINARY: &str = "claude";

impl ClaudeCode {
    /// Builds a report from captured output, so detection is testable without the binary.
    pub fn report(binary: Option<String>, version_out: &str, help: &str) -> HostReport {
        let version = version_out.split_whitespace().next().map(str::to_string);
        let headless = help.contains("--print");
        let capabilities = vec![
            flag_capability(C::SessionStart, help, &["--print"], "headless print mode"),
            flag_capability(C::SessionCollect, help, &["--output-format"], "JSON result"),
            flag_capability(C::EventStream, help, &["stream-json"], "stream-json events"),
            process_capability(C::SessionCancel, headless, "one OS process per session; cancel by signal"),
            flag_capability(C::ToolRestriction, help, &["--allowedTools", "--disallowedTools"], "allow and deny lists"),
            plugin_hook_capability(C::PerCallPolicy, help, "PreToolUse can deny a call"),
            plugin_hook_capability(C::StopGuard, help, "Stop can hold the agent from finishing"),
            flag_capability(C::ModelSelection, help, &["--model"], "per session"),
            flag_capability(C::CustomAgents, help, &["--agents"], "custom agents defined per session"),
            process_capability(C::Parallel, headless, "independent processes"),
        ];
        HostReport {
            host: "claude-code".into(),
            contract_version: CONTRACT_VERSION,
            installed: true,
            binary,
            version,
            auth: AuthStatus::Unknown,
            capabilities,
            notes: vec!["the test host for this repository; pin its version for evaluation runs".into()],
        }
    }

    /// The command for a session, given the binary's location. The prompt goes
    /// on stdin, so no variadic flag can swallow it.
    pub fn plan_with(&self, bin: std::path::PathBuf, spec: &SessionSpec) -> CommandPlan {
        let tools = self.translate(&spec.tools);
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            // Anything not allowed is denied instead of prompting.
            "--permission-mode",
            "dontAsk",
            // Keep the user's and the project's own settings out of the session.
            "--setting-sources",
            "",
            "--no-session-persistence",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if !tools.allow.is_empty() {
            args.push("--allowedTools".into());
            args.extend(tools.allow);
        }
        if !tools.deny.is_empty() {
            args.push("--disallowedTools".into());
            args.extend(tools.deny);
        }
        if let Some(dir) = &spec.plugin_dir {
            args.extend(["--plugin-dir".into(), dir.display().to_string()]);
        }
        if let Some(m) = &spec.model {
            args.extend(["--model".into(), m.clone()]);
        }
        if let Some(n) = spec.max_turns {
            args.extend(["--max-turns".into(), n.to_string()]);
        }
        if let Some(sys) = &spec.append_system {
            args.extend(["--append-system-prompt".into(), sys.clone()]);
        }
        if let Some(id) = &spec.session_id {
            args.extend(["--session-id".into(), id.clone()]);
        }
        // Claude Code stops itself at a dollar cap, ending with `error_max_budget_usd`.
        if let Some(usd) = spec.max_cost_usd {
            args.extend(["--max-budget-usd".into(), format!("{usd:.4}")]);
        }
        CommandPlan { program: bin, args, stdin: Some(spec.prompt.clone()), env: spec.env.clone() }
    }
}

impl Host for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn inspect(&self, probe: &Probe) -> HostReport {
        let Some(bin) = probe.locate(self.name(), BINARY) else {
            return HostReport::not_installed(self.name(), BINARY);
        };
        let version = probe.run(&bin, &["--version"]).map(|(_, o)| o).unwrap_or_default();
        let help = probe.run(&bin, &["--help"]).map(|(_, o)| o).unwrap_or_default();
        ClaudeCode::report(Some(bin.display().to_string()), &version, &help)
    }

    fn check_auth(&self, probe: &Probe) -> AuthStatus {
        let Some(bin) = probe.locate(self.name(), BINARY) else { return AuthStatus::Failed };
        let args = ["-p", "Reply with the single word ok.", "--output-format", "json", "--max-turns", "1"];
        match probe.run(&bin, &args) {
            Some((true, out)) => match serde_json::from_str::<serde_json::Value>(out.trim()) {
                Ok(v) if v["is_error"] == false => AuthStatus::Ok,
                _ => AuthStatus::Failed,
            },
            _ => AuthStatus::Failed,
        }
    }

    fn plan(&self, probe: &Probe, spec: &SessionSpec) -> Result<CommandPlan, String> {
        let bin = probe.locate(self.name(), BINARY).ok_or("claude was not found on PATH")?;
        Ok(self.plan_with(bin, spec))
    }

    fn summarize(&self, lines: &[String]) -> SessionSummary {
        let mut s = SessionSummary { events: lines.len() as u64, is_error: true, ..Default::default() };
        for e in json_lines(lines).filter(|e| e["type"] == "result") {
            s.is_error = e["is_error"].as_bool().unwrap_or(true);
            s.final_text = e["result"].as_str().map(str::to_string);
            s.session_id = e["session_id"].as_str().map(str::to_string);
            s.turns = e["num_turns"].as_u64();
            s.cost_usd = e["total_cost_usd"].as_f64();
            s.denials = e["permission_denials"].as_array().map_or(0, |a| a.len() as u64);
            let subtype = e["subtype"].as_str().filter(|t| *t != "success");
            s.stop_reason = e["terminal_reason"].as_str().or(subtype).map(str::to_string);
            if s.final_text.is_none() {
                // Error results carry their messages in `errors` instead.
                let errors: Vec<&str> =
                    e["errors"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_str).collect();
                s.final_text = (!errors.is_empty()).then(|| errors.join("; "));
            }
        }
        s
    }

    fn tool_patterns(&self, tool: &str) -> Vec<String> {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
        match tool {
            "read" => names(&["Read", "Grep", "Glob"]),
            "edit" => names(&["Edit", "Write", "NotebookEdit"]),
            "shell" => names(&["Bash"]),
            "web" => names(&["WebFetch", "WebSearch"]),
            "agent" => names(&["Agent"]),
            _ => {
                if let Some(cmd) = tool.strip_prefix("shell:") {
                    vec![format!("Bash({cmd}:*)")]
                } else if let Some(rest) = tool.strip_prefix("mcp:") {
                    vec![format!("mcp__{}", rest.replace('/', "__"))]
                } else {
                    vec![tool.to_string()]
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interlock_schema::ToolPolicy;

    const HELP: &str = "  -p, --print\n  --output-format <format> (choices: \"text\", \"json\", \"stream-json\")\n  --allowedTools, --allowed-tools <tools...>\n  --disallowedTools, --disallowed-tools <tools...>\n  --model <model>\n  --agents <json-or-file>\n  --plugin-dir <path>\n";

    #[test]
    fn detects_capabilities_from_help() {
        let r = ClaudeCode::report(None, "2.1.289 (Claude Code)\n", HELP);
        assert_eq!(r.version.as_deref(), Some("2.1.289"));
        let caps = r.capability_set();
        for c in [C::SessionStart, C::EventStream, C::ToolRestriction, C::PerCallPolicy, C::StopGuard, C::CustomAgents]
        {
            assert!(caps.has(c), "{c:?}");
        }
    }

    #[test]
    fn plans_a_session_with_the_prompt_on_stdin() {
        let spec = SessionSpec {
            workdir: "/w".into(),
            prompt: "Fix it".into(),
            append_system: Some("You are the verifier.".into()),
            tools: ToolPolicy { allow: vec!["read".into(), "shell".into()], deny: vec!["edit".into()] },
            model: None,
            max_turns: Some(30),
            plugin_dir: Some("/p".into()),
            env: vec![],
            timeout: std::time::Duration::from_secs(60),
            transcript: "/t.jsonl".into(),
            session_id: Some("6f1c0b8e-3a2d-4c5e-9f00-1a2b3c4d5e6f".into()),
            max_cost_usd: Some(0.25),
        };
        let plan = ClaudeCode.plan_with("/bin/claude".into(), &spec);
        assert_eq!(plan.stdin.as_deref(), Some("Fix it"));
        let joined = plan.args.join(" ");
        assert!(
            joined.contains("--allowedTools Read Grep Glob Bash --disallowedTools Edit Write NotebookEdit"),
            "{joined}"
        );
        assert!(joined.contains("--permission-mode dontAsk"));
        assert!(plan.args.windows(2).any(|w| w == ["--max-turns", "30"]));
        assert!(plan.args.windows(2).any(|w| w == ["--session-id", "6f1c0b8e-3a2d-4c5e-9f00-1a2b3c4d5e6f"]));
        assert!(plan.args.windows(2).any(|w| w == ["--max-budget-usd", "0.2500"]));
    }

    #[test]
    fn a_session_stopped_at_its_cost_cap_says_so() {
        // Captured from Claude Code 2.1.289 with --max-budget-usd 0.000001, trimmed.
        let line = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"num_turns":1,"total_cost_usd":0.000944,"terminal_reason":"budget_exhausted","errors":["Reached maximum budget ($0.000001)"],"permission_denials":[],"session_id":"6f1c0b8e-3a2d-4c5e-9f00-1a2b3c4d5e6f"}"#;
        let s = ClaudeCode.summarize(&[line.to_string()]);
        assert!(s.is_error);
        assert_eq!(s.stop_reason.as_deref(), Some("budget_exhausted"));
        assert_eq!(s.cost_usd, Some(0.000944));
        assert_eq!(s.final_text.as_deref(), Some("Reached maximum budget ($0.000001)"));
    }

    #[test]
    fn summarizes_the_result_event() {
        let lines = vec![
            r#"{"type":"system","subtype":"init"}"#.to_string(),
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","num_turns":4,"total_cost_usd":0.12,"permission_denials":[{"tool_name":"Write"}],"session_id":"s"}"#.to_string(),
        ];
        let s = ClaudeCode.summarize(&lines);
        assert_eq!((s.final_text.as_deref(), s.turns, s.denials, s.is_error), (Some("Done."), Some(4), 1, false));
        assert!(ClaudeCode.summarize(&lines[..1]).is_error, "no result event means the session did not finish");
    }
}
