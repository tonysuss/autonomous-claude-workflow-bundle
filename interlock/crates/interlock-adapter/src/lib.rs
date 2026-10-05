//! The host adapter contract. The core never names a host: it reads a
//! `HostReport` of versioned capabilities, and adapters translate the core's
//! host-neutral tool names into each host's own permission patterns.
//!
//! Copilot CLI and Claude Code are both first-class adapters behind this
//! contract. Neither one's features shape the core.

mod claude_code;
mod copilot;
pub mod hooks;
mod probe;
mod session;

use interlock_core::capability::{CONTRACT_VERSION, Capability, CapabilitySet};
use interlock_schema::ToolPolicy;
use serde::{Deserialize, Serialize};

pub use claude_code::ClaudeCode;
pub use copilot::Copilot;
pub use probe::Probe;
pub use session::{
    CommandPlan, Exit, SessionOutcome, SessionSpec, SessionSummary, run, shell_quote, write_hooks_plugin,
};

/// How a capability's availability was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Seen in this binary's own help or version output.
    Detected,
    /// Implied by how the adapter runs the host, such as one OS process per session.
    Process,
    /// Present in the host's documentation but not visible in its help output.
    Documented,
    /// Not established; treated as unavailable.
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEvidence {
    pub capability: Capability,
    pub available: bool,
    pub source: Source,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    /// Not checked. Checking costs a model call, so it is opt-in.
    Unknown,
    Ok,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReport {
    pub host: String,
    pub contract_version: u32,
    pub installed: bool,
    pub binary: Option<String>,
    pub version: Option<String>,
    pub auth: AuthStatus,
    pub capabilities: Vec<CapabilityEvidence>,
    pub notes: Vec<String>,
}

impl HostReport {
    pub fn capability_set(&self) -> CapabilitySet {
        self.capabilities.iter().filter(|c| c.available).map(|c| c.capability).collect()
    }

    fn not_installed(host: &str, binary: &str) -> HostReport {
        HostReport {
            host: host.into(),
            contract_version: CONTRACT_VERSION,
            installed: false,
            binary: None,
            version: None,
            auth: AuthStatus::Unknown,
            capabilities: vec![],
            notes: vec![format!("`{binary}` was not found on PATH")],
        }
    }
}

/// One host behind the contract.
pub trait Host {
    /// Stable name used in records and on the command line.
    fn name(&self) -> &'static str;

    /// Reports the installed version and its capabilities, without a model call.
    fn inspect(&self, probe: &Probe) -> HostReport;

    /// Checks that the host can run a session, which costs one small model call.
    fn check_auth(&self, probe: &Probe) -> AuthStatus;

    /// Translates one host-neutral tool name into this host's permission patterns.
    /// An empty result means the host does not gate that tool.
    fn tool_patterns(&self, tool: &str) -> Vec<String>;

    /// Builds the command for one headless session.
    fn plan(&self, probe: &Probe, spec: &SessionSpec) -> Result<CommandPlan, String>;

    /// Reads a finished session's output stream.
    fn summarize(&self, lines: &[String]) -> SessionSummary;

    /// Runs one headless session to completion, timeout or cancellation.
    fn run_session(&self, probe: &Probe, spec: &SessionSpec, cancel: &std::sync::atomic::AtomicBool) -> SessionOutcome {
        match self.plan(probe, spec) {
            Ok(plan) => session::run(&plan, spec, cancel, |lines| self.summarize(lines)),
            Err(reason) => SessionOutcome {
                exit: Exit::Failed { reason },
                exit_code: None,
                duration_ms: 0,
                summary: SessionSummary::default(),
                transcript: spec.transcript.clone(),
            },
        }
    }

    /// Translates a whole policy. Deny still wins on the host side.
    fn translate(&self, policy: &ToolPolicy) -> ToolPolicy {
        let map = |list: &[String]| {
            let mut out: Vec<String> = Vec::new();
            for t in list {
                for p in self.tool_patterns(t) {
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            out
        };
        ToolPolicy { allow: map(&policy.allow), deny: map(&policy.deny) }
    }
}

pub fn hosts() -> Vec<Box<dyn Host>> {
    vec![Box::new(Copilot), Box::new(ClaudeCode)]
}

pub fn host(name: &str) -> Option<Box<dyn Host>> {
    hosts().into_iter().find(|h| h.name() == name)
}

/// Builds evidence for a flag-backed capability from help text.
fn flag_capability(capability: Capability, help: &str, flags: &[&str], what: &str) -> CapabilityEvidence {
    let missing: Vec<&str> = flags.iter().copied().filter(|f| !help.contains(f)).collect();
    CapabilityEvidence {
        capability,
        available: missing.is_empty(),
        source: if missing.is_empty() { Source::Detected } else { Source::Unverified },
        detail: if missing.is_empty() {
            format!("{what} ({})", flags.join(", "))
        } else {
            format!("help output lacks {}", missing.join(", "))
        },
    }
}

/// Per-call policy and the stop guard both come from interlock's hooks plugin,
/// which the host must be able to load with `--plugin-dir`.
fn plugin_hook_capability(capability: Capability, help: &str, what: &str) -> CapabilityEvidence {
    let ok = help.contains("--plugin-dir");
    CapabilityEvidence {
        capability,
        available: ok,
        source: if ok { Source::Detected } else { Source::Unverified },
        detail: if ok {
            format!("{what}, from interlock's hooks plugin (--plugin-dir)")
        } else {
            "help output lacks --plugin-dir".into()
        },
    }
}

fn json_lines(lines: &[String]) -> impl Iterator<Item = serde_json::Value> + '_ {
    lines.iter().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
}

fn process_capability(capability: Capability, needs: bool, detail: &str) -> CapabilityEvidence {
    CapabilityEvidence {
        capability,
        available: needs,
        source: if needs { Source::Process } else { Source::Unverified },
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_hosts_are_registered_and_copilot_comes_first() {
        let names: Vec<&str> = hosts().iter().map(|h| h.name()).collect();
        assert_eq!(names, vec!["copilot", "claude-code"]);
    }

    #[test]
    fn one_policy_translates_to_each_host() {
        let policy = ToolPolicy {
            allow: vec!["read".into(), "edit".into(), "shell:cargo test".into(), "mcp:github/get_pr".into()],
            deny: vec!["shell:git push".into()],
        };
        let claude = ClaudeCode.translate(&policy);
        assert_eq!(
            claude.allow,
            vec!["Read", "Grep", "Glob", "Edit", "Write", "NotebookEdit", "Bash(cargo test:*)", "mcp__github__get_pr"]
        );
        assert_eq!(claude.deny, vec!["Bash(git push:*)"]);
        let copilot = Copilot.translate(&policy);
        assert_eq!(copilot.allow, vec!["write", "shell(cargo test:*)", "github(get_pr)"]);
        assert_eq!(copilot.deny, vec!["shell(git push:*)"]);
    }
}
