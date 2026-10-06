//! Each session's environment, built from an allowlist. Nothing else from
//! the environment interlock runs in reaches a session: launched from inside
//! another agent, interlock would otherwise hand its sessions the parent's
//! session ids, message sockets, tokens and settings.
//!
//! A session gets:
//! - the generic variables below (paths, locale, terminal, proxy and CA settings);
//! - what its host needs, from [`crate::Host::passes_env`]: `ANTHROPIC_*` and
//!   `CLAUDE_CODE_USE_*` for Claude Code; `COPILOT_*` except the variables that
//!   bind a process to a running Copilot session, plus the GitHub token
//!   variables, for Copilot CLI;
//! - interlock's own `INTERLOCK_*` variables, except the per-session ones the
//!   supervisor sets itself;
//! - anything the operator lists under `[env] pass` in `.interlock/config.toml`.
//!
//! The host's own shell commands, and anything they start, inherit what the
//! host was given, credentials included.

use std::ffi::OsString;

use crate::Host;

/// Passed to every session, when set.
pub const GENERIC: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TERM",
    "TZ",
    "TMPDIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
];

/// Prefixes passed to every session.
pub const GENERIC_PREFIXES: &[&str] = &["LC_"];

/// interlock's per-session variables. The supervisor sets them for each
/// session; a value inherited from the parent never passes.
pub const PER_SESSION: &[&str] = &[
    "INTERLOCK_DB",
    "INTERLOCK_ATTEMPT",
    "INTERLOCK_TOKEN",
    "INTERLOCK_MODE",
    "INTERLOCK_HOST",
    "INTERLOCK_TREE",
    "INTERLOCK_SESSION",
];

/// Whether a `[env] pass` entry matches: an exact name, or a prefix ending in `*`.
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Whether a variable inherited from the parent may reach a session on `host`.
pub fn allowed(host: &dyn Host, name: &str, pass: &[String]) -> bool {
    if PER_SESSION.contains(&name) {
        return false;
    }
    GENERIC.contains(&name)
        || GENERIC_PREFIXES.iter().any(|p| name.starts_with(p))
        || name.starts_with("INTERLOCK_")
        || host.passes_env(name)
        || pass.iter().any(|p| pattern_matches(p, name))
}

/// The variables from `parent` that a session on `host` may have. Names or
/// values that are not UTF-8 are left out.
pub fn session_env(
    host: &dyn Host,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    pass: &[String],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = parent
        .into_iter()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| allowed(host, k, pass))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaudeCode, Copilot};

    fn parent() -> Vec<(OsString, OsString)> {
        [
            ("PATH", "/usr/bin"),
            ("HOME", "/root"),
            ("LC_ALL", "C.UTF-8"),
            ("HTTPS_PROXY", "http://proxy"),
            ("ANTHROPIC_BASE_URL", "http://127.0.0.1:1"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "parent-session"),
            ("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/sock"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "secret"),
            ("CLAUDE_SESSION_INGRESS_TOKEN_FILE", "/tmp/tok"),
            ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
            ("GH_TOKEN", "gh-secret"),
            ("GITHUB_TOKEN", "gh-secret"),
            ("COPILOT_PROVIDER_BASE_URL", "http://127.0.0.1:2/v1"),
            ("COPILOT_OFFLINE", "true"),
            ("COPILOT_LOADER_PID", "42"),
            ("COPILOT_AGENT_SESSION_ID", "parent"),
            ("INTERLOCK_COPILOT_BIN", "/opt/copilot"),
            ("INTERLOCK_TOKEN", "a-parent-attempt"),
            ("INTERLOCK_TREE", "abc1234"),
            ("MY_TOOL_HOME", "/opt/tool"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    fn names(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn claude_code_sessions_get_the_allowlist_and_nothing_that_binds_them_to_a_parent() {
        let env = session_env(&ClaudeCode, parent(), &["MY_TOOL_*".into()]);
        assert_eq!(
            names(&env),
            [
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_USE_BEDROCK",
                "HOME",
                "HTTPS_PROXY",
                "INTERLOCK_COPILOT_BIN",
                "LC_ALL",
                "MY_TOOL_HOME",
                "PATH"
            ]
        );
    }

    #[test]
    fn copilot_sessions_get_their_provider_settings_and_github_tokens_only() {
        let env = session_env(&Copilot, parent(), &[]);
        assert_eq!(
            names(&env),
            [
                "COPILOT_OFFLINE",
                "COPILOT_PROVIDER_BASE_URL",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "HOME",
                "HTTPS_PROXY",
                "INTERLOCK_COPILOT_BIN",
                "LC_ALL",
                "PATH"
            ]
        );
    }

    #[test]
    fn pass_patterns_are_exact_names_or_prefixes() {
        assert!(pattern_matches("FOO", "FOO"));
        assert!(!pattern_matches("FOO", "FOOD"));
        assert!(pattern_matches("FOO_*", "FOO_BAR"));
        assert!(!pattern_matches("FOO_*", "FOO"));
    }
}
