//! A host-neutral tool vocabulary. Adapters translate these names into each
//! host's own patterns (Claude Code `Bash(git:*)`, Copilot `shell(git:*)`).
//!
//! - `read`: read, search and list files
//! - `edit`: create or change files in the worktree
//! - `shell`, or `shell:<command prefix>`: run commands
//! - `web`: fetch or search the web
//! - `mcp:<server>`, or `mcp:<server>/<tool>`: MCP tools
//! - `agent`: start sub-agents

use interlock_schema::ToolPolicy;

pub const READ: &str = "read";
pub const EDIT: &str = "edit";
pub const SHELL: &str = "shell";
pub const WEB: &str = "web";
pub const AGENT: &str = "agent";

/// Whether `pattern` covers `tool`. A family name covers its members
/// (`shell` covers `shell:ls`, `mcp:github` covers `mcp:github/create_pr`),
/// and a command prefix covers longer commands at a word boundary
/// (`shell:git` covers `shell:git status` but not `shell:gitk`).
pub fn pattern_matches(pattern: &str, tool: &str) -> bool {
    if pattern == tool {
        return true;
    }
    match tool.strip_prefix(pattern) {
        Some(rest) => {
            rest.starts_with(':') || rest.starts_with('/') || (pattern.contains(':') && rest.starts_with(' '))
        }
        None => false,
    }
}

/// Deny wins over allow; anything not allowed is denied.
pub fn permits(policy: &ToolPolicy, tool: &str) -> bool {
    !policy.deny.iter().any(|p| pattern_matches(p, tool)) && policy.allow.iter().any(|p| pattern_matches(p, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allow: &[&str], deny: &[&str]) -> ToolPolicy {
        ToolPolicy {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn families_and_prefixes() {
        assert!(pattern_matches("shell", "shell:ls -la"));
        assert!(pattern_matches("shell:git", "shell:git status"));
        assert!(!pattern_matches("shell:git", "shell:gitk"));
        assert!(pattern_matches("mcp:github", "mcp:github/create_pr"));
        assert!(!pattern_matches("read", "readonly"));
    }

    #[test]
    fn deny_wins() {
        let p = policy(&["shell"], &["shell:git push"]);
        assert!(permits(&p, "shell:git status"));
        assert!(!permits(&p, "shell:git push origin main"));
        assert!(!permits(&p, "edit"), "not allowed means denied");
    }
}
