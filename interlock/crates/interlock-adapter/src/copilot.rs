//! GitHub Copilot CLI.

use interlock_core::capability::{CONTRACT_VERSION, Capability as C};

use crate::{AuthStatus, CapabilityEvidence, Host, HostReport, Probe, Source, flag_capability, process_capability};

pub struct Copilot;

const BINARY: &str = "copilot";

impl Copilot {
    /// Builds a report from captured output, so detection is testable without the binary.
    pub fn report(binary: Option<String>, version_out: &str, help: &str, config_help: &str) -> HostReport {
        let version = version_out
            .split_whitespace()
            .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(|v| v.trim_end_matches('.').to_string());
        let headless = help.contains("--prompt");
        let hooks = config_help.contains("hooks");
        let capabilities = vec![
            flag_capability(C::SessionStart, help, &["-p, --prompt"], "headless one-shot"),
            flag_capability(C::SessionCollect, help, &["--output-format"], "JSONL output"),
            flag_capability(C::EventStream, help, &["--output-format"], "one JSON object per line while it runs"),
            process_capability(C::SessionCancel, headless, "one OS process per session; cancel by signal"),
            flag_capability(
                C::ToolRestriction,
                help,
                &["--available-tools", "--allow-tool", "--deny-tool"],
                "tool filters; deny rules win",
            ),
            CapabilityEvidence {
                capability: C::PerCallPolicy,
                available: hooks,
                source: if hooks { Source::Detected } else { Source::Unverified },
                detail: if hooks {
                    "repo hooks (.github/hooks/*.json); preToolUse can deny a call".into()
                } else {
                    "config help does not mention hooks".into()
                },
            },
            CapabilityEvidence {
                capability: C::StopGuard,
                available: false,
                source: Source::Unverified,
                detail: "no hook that can hold an agent from stopping is confirmed yet".into(),
            },
            flag_capability(C::ModelSelection, help, &["--model"], "per session"),
            flag_capability(C::CustomAgents, help, &["--agent"], "custom agents as isolated sub-agents"),
            process_capability(C::Parallel, headless, "independent processes; --fleet is optional"),
        ];
        HostReport {
            host: "copilot".into(),
            contract_version: CONTRACT_VERSION,
            installed: true,
            binary,
            version,
            auth: AuthStatus::Unknown,
            capabilities,
            notes: vec!["pin the Copilot CLI version: it ships often and changes defaults".into()],
        }
    }
}

impl Host for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn inspect(&self, probe: &Probe) -> HostReport {
        let Some(bin) = probe.locate(self.name(), BINARY) else {
            return HostReport::not_installed(self.name(), BINARY);
        };
        let version = probe.run(&bin, &["--version"]).map(|(_, o)| o).unwrap_or_default();
        let help = probe.run(&bin, &["--help"]).map(|(_, o)| o).unwrap_or_default();
        let config = probe.run(&bin, &["help", "config"]).map(|(_, o)| o).unwrap_or_default();
        Copilot::report(Some(bin.display().to_string()), &version, &help, &config)
    }

    fn check_auth(&self, probe: &Probe) -> AuthStatus {
        let Some(bin) = probe.locate(self.name(), BINARY) else { return AuthStatus::Failed };
        match probe.run(&bin, &["-p", "Reply with the single word ok.", "--output-format", "json", "-s"]) {
            Some((true, out)) if out.to_lowercase().contains("ok") => AuthStatus::Ok,
            _ => AuthStatus::Failed,
        }
    }

    fn tool_patterns(&self, tool: &str) -> Vec<String> {
        match tool {
            // Copilot does not gate reading; sub-agents are chosen with --agent.
            "read" | "agent" => vec![],
            "edit" => vec!["write".into()],
            "shell" => vec!["shell".into()],
            "web" => vec!["url".into()],
            _ => {
                if let Some(cmd) = tool.strip_prefix("shell:") {
                    vec![format!("shell({cmd}:*)")]
                } else if let Some(rest) = tool.strip_prefix("mcp:") {
                    match rest.split_once('/') {
                        Some((server, name)) => vec![format!("{server}({name})")],
                        None => vec![rest.to_string()],
                    }
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

    const HELP: &str = "  -p, --prompt <text>\n  --output-format <format>\n  --model <model>\n  --agent <agent>\n  --available-tools [<tools>...]\n  --allow-tool [<tools>...]\n  --deny-tool [<tools>...]\n  --fleet\n";

    #[test]
    fn detects_capabilities_from_help() {
        let r = Copilot::report(None, "GitHub Copilot CLI 1.0.91.\n", HELP, "`hooks`: inline hook definitions");
        assert_eq!(r.version.as_deref(), Some("1.0.91"));
        let caps = r.capability_set();
        for c in [
            C::SessionStart,
            C::SessionCollect,
            C::ToolRestriction,
            C::PerCallPolicy,
            C::CustomAgents,
            C::ModelSelection,
        ] {
            assert!(caps.has(c), "{c:?}");
        }
        assert!(!caps.has(C::StopGuard), "unconfirmed capabilities are not claimed");
    }

    #[test]
    fn missing_flags_are_not_claimed() {
        let r = Copilot::report(None, "1.0.0", "  -p, --prompt <text>\n", "");
        assert!(!r.capability_set().has(C::ToolRestriction));
        assert!(!r.capability_set().has(C::PerCallPolicy));
    }
}
