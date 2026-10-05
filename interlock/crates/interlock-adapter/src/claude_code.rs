//! Claude Code.

use interlock_core::capability::{CONTRACT_VERSION, Capability as C};

use crate::{AuthStatus, CapabilityEvidence, Host, HostReport, Probe, Source, flag_capability, process_capability};

pub struct ClaudeCode;

const BINARY: &str = "claude";

impl ClaudeCode {
    /// Builds a report from captured output, so detection is testable without the binary.
    pub fn report(binary: Option<String>, version_out: &str, help: &str) -> HostReport {
        let version = version_out.split_whitespace().next().map(str::to_string);
        let headless = help.contains("--print");
        let settings = help.contains("--settings");
        let hook = |capability, detail: &str| CapabilityEvidence {
            capability,
            available: settings,
            source: if settings { Source::Documented } else { Source::Unverified },
            detail: if settings { detail.into() } else { "help output lacks --settings for hooks".into() },
        };
        let capabilities = vec![
            flag_capability(C::SessionStart, help, &["--print"], "headless print mode"),
            flag_capability(C::SessionCollect, help, &["--output-format"], "JSON result"),
            flag_capability(C::EventStream, help, &["stream-json"], "stream-json events"),
            process_capability(C::SessionCancel, headless, "one OS process per session; cancel by signal"),
            flag_capability(C::ToolRestriction, help, &["--allowedTools", "--disallowedTools"], "allow and deny lists"),
            hook(C::PerCallPolicy, "PreToolUse hooks passed with --settings can deny a call"),
            hook(C::StopGuard, "Stop and SubagentStop hooks can hold an agent from finishing"),
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

    const HELP: &str = "  -p, --print\n  --output-format <format> (choices: \"text\", \"json\", \"stream-json\")\n  --allowedTools, --allowed-tools <tools...>\n  --disallowedTools, --disallowed-tools <tools...>\n  --model <model>\n  --agents <json-or-file>\n  --settings <file-or-json>\n";

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
}
