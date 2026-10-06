//! GitHub Copilot CLI.

use interlock_core::capability::{CONTRACT_VERSION, Capability as C};
use interlock_schema::ToolPolicy;

use crate::{
    AuthStatus, CommandPlan, Host, HostReport, Probe, SessionSpec, SessionSummary, flag_capability, json_lines,
    plugin_hook_capability, process_capability,
};

pub struct Copilot;

const BINARY: &str = "copilot";

/// A custom agent profile for one headless session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    /// What `--agent` takes: `<plugin>:<agent>`.
    pub name: String,
    /// `agents/<agent>.agent.md` in the hooks plugin.
    pub path: std::path::PathBuf,
    pub text: String,
}

impl AgentProfile {
    pub fn write(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.path.parent().expect("an agent profile lives in a plugin"))?;
        std::fs::write(&self.path, &self.text)
    }
}

/// What Copilot CLI prints when it cannot sign in, lowercased. Captured from 1.0.91.
pub(crate) const AUTH_FAILURES: &[&str] = &["no authentication information found"];

/// Variables that tie a process to a running Copilot session. Copilot hides
/// them from its own children too.
pub const SESSION_BINDING: &[&str] = &[
    "COPILOT_LOADER_PID",
    "COPILOT_SUPERVISED",
    "COPILOT_RUN_APP",
    "COPILOT_AGENT_SESSION_ID",
    "COPILOT_CONNECTION_TOKEN",
    "COPILOT_DETACHED_SESSION",
    "COPILOT_DETACHED_PARENT_SESSION_ID",
    "COPILOT_DETACHED_PARENT_ENGAGEMENT_ID",
];

/// The GitHub token variables Copilot reads for its own sign-in.
pub const GITHUB_TOKENS: &[&str] = &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];

impl Copilot {
    /// Builds a report from captured output, so detection is testable without the binary.
    pub fn report(binary: Option<String>, version_out: &str, help: &str) -> HostReport {
        let version = version_out
            .split_whitespace()
            .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(|v| v.trim_end_matches('.').to_string());
        let headless = help.contains("--prompt");
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
            plugin_hook_capability(C::PerCallPolicy, help, "PreToolUse can deny a call"),
            plugin_hook_capability(C::StopGuard, help, "Stop can hold the agent from finishing"),
            flag_capability(C::ModelSelection, help, &["--model"], "per session"),
            flag_capability(C::CustomAgents, help, &["--agent"], "custom agents as isolated sub-agents"),
            flag_capability(C::EffortSelection, help, &["--reasoning-effort"], "reasoning effort per session"),
            process_capability(C::Parallel, headless, "independent processes; --fleet is optional"),
        ];
        HostReport {
            host: "copilot".into(),
            contract_version: CONTRACT_VERSION,
            installed: true,
            binary,
            version,
            auth: crate::AuthStatus::Unknown,
            capabilities,
            notes: vec!["pin the Copilot CLI version: it ships often and changes defaults".into()],
        }
    }

    /// The custom agent a session runs as, `copilot -p --agent`: interlock's
    /// profile for the session's role, kept in the hooks plugin, with the
    /// role's instructions and only the tools its grant leaves it. `None`
    /// when the session names no agent or loads no hooks plugin.
    pub fn agent_profile(&self, spec: &SessionSpec) -> Option<AgentProfile> {
        let (name, plugin) = (spec.agent.as_ref()?, spec.plugin_dir.as_ref()?);
        // JSON strings and arrays are valid YAML, so nothing in them needs escaping.
        let json = |v: serde_json::Value| v.to_string();
        let text = format!(
            "---\nname: {}\ndescription: {}\ntools: {}\n---\n\n{}\n",
            json(name.as_str().into()),
            json(format!("{name}, launched by interlock for one attempt").into()),
            json(self.agent_tools(&spec.tools).into()),
            spec.append_system.as_deref().unwrap_or_default(),
        );
        Some(AgentProfile {
            name: format!("{}:{name}", crate::HOOKS_PLUGIN),
            path: plugin.join("agents").join(format!("{name}.agent.md")),
            text,
        })
    }

    /// The tools an agent profile lists: every tool the policy allows and does
    /// not deny outright, by the names Copilot 1.0.91 offers. Copilot then
    /// offers nothing else but its `skill` and `sql` tools; narrower denials
    /// (`shell:git push`) are still `--deny-tool` rules.
    fn agent_tools(&self, policy: &ToolPolicy) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for tool in policy.allow.iter().filter(|t| !policy.deny.contains(t)) {
            let names: Vec<String> = match tool.as_str() {
                "read" => vec!["view".into(), "grep".into(), "glob".into()],
                "edit" => vec!["create".into(), "edit".into()],
                "shell" => vec!["bash".into()],
                "web" => vec!["web_fetch".into(), "web_search".into()],
                "agent" => vec!["agent".into()],
                t if t.starts_with("shell:") => vec!["bash".into()],
                // An MCP tool is `<server>/<tool>`; a whole server, `<server>/*`.
                t => match t.strip_prefix("mcp:") {
                    Some(tool) if tool.contains('/') => vec![tool.to_string()],
                    Some(server) => vec![format!("{server}/*")],
                    None => vec![t.to_string()],
                },
            };
            for n in names {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// The command for a session, given the binary's location.
    pub fn plan_with(&self, bin: std::path::PathBuf, spec: &SessionSpec) -> CommandPlan {
        let tools = self.translate(&spec.tools);
        let agent = self.agent_profile(spec);
        let prompt = match (&spec.append_system, &agent) {
            // The agent's profile carries the role's instructions into the system prompt.
            (_, Some(_)) | (None, None) => spec.prompt.clone(),
            // Without one, Copilot has no flag for extra system instructions, so they lead the prompt.
            (Some(sys), None) => format!("{sys}\n\n{}", spec.prompt),
        };
        let mut args: Vec<String> = vec![
            "-p".into(),
            prompt,
            "--output-format".into(),
            "json".into(),
            "--no-ask-user".into(),
            "--no-auto-update".into(),
        ];
        args.extend(tools.allow.iter().map(|p| format!("--allow-tool={p}")));
        args.extend(tools.deny.iter().map(|p| format!("--deny-tool={p}")));
        for dir in spec.plugin_dir.iter().chain(&spec.extra_plugin_dirs) {
            args.extend(["--plugin-dir".into(), dir.display().to_string()]);
        }
        if let Some(a) = &agent {
            args.extend(["--agent".into(), a.name.clone()]);
        }
        if let Some(m) = &spec.model {
            args.extend(["--model".into(), m.clone()]);
        }
        if let Some(id) = &spec.session_id {
            args.push(format!("--session-id={id}"));
        }
        if let Some(level) = &spec.effort {
            args.push(format!("--reasoning-effort={level}"));
        }
        let mut env = spec.env.clone();
        env.push(("COPILOT_AUTO_UPDATE".into(), "false".into()));
        CommandPlan { program: bin, args, stdin: None, env }
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
        let version = probe.query(&bin, &["--version"]).map(|(_, o)| o).unwrap_or_default();
        let help = probe.query(&bin, &["--help"]).map(|(_, o)| o).unwrap_or_default();
        Copilot::report(Some(bin.display().to_string()), &version, &help)
    }

    fn check_auth(&self, probe: &Probe) -> AuthStatus {
        let Some(bin) = probe.locate(self.name(), BINARY) else { return AuthStatus::Failed };
        let args = ["-p", "Reply with the single word ok.", "--output-format", "json", "--no-ask-user"];
        match probe.run(&bin, &args) {
            Some((true, out)) => {
                let lines: Vec<String> = out.lines().map(str::to_string).collect();
                let s = self.summarize(&lines);
                if !s.is_error && s.final_text.is_some() { AuthStatus::Ok } else { AuthStatus::Failed }
            }
            _ => AuthStatus::Failed,
        }
    }

    fn plan(&self, probe: &Probe, spec: &SessionSpec) -> Result<CommandPlan, String> {
        let bin = probe.locate(self.name(), BINARY).ok_or("copilot was not found on PATH")?;
        if let Some(agent) = self.agent_profile(spec) {
            agent.write().map_err(|e| format!("cannot write the agent profile {}: {e}", agent.path.display()))?;
        }
        Ok(self.plan_with(bin, spec))
    }

    fn summarize(&self, lines: &[String]) -> SessionSummary {
        let mut s = SessionSummary { events: lines.len() as u64, ..Default::default() };
        let mut turns = 0;
        let mut saw_result = false;
        for e in json_lines(lines) {
            match e["type"].as_str() {
                Some("assistant.message") => {
                    if let Some(text) = e["data"]["content"].as_str().filter(|t| !t.trim().is_empty()) {
                        s.final_text = Some(text.to_string());
                    }
                }
                Some("assistant.turn_end") => turns += 1,
                // A denied call fails with error code "denied", whether a hook
                // or a --deny-tool rule refused it.
                Some("tool.execution_complete") if e["data"]["error"]["code"] == "denied" => s.denials += 1,
                Some("result") => {
                    saw_result = true;
                    s.session_id = e["sessionId"].as_str().map(str::to_string);
                    s.is_error = e["exitCode"].as_i64().is_some_and(|c| c != 0);
                    s.premium_requests = e["usage"]["premiumRequests"].as_f64();
                    s.finished = true;
                }
                _ => {}
            }
        }
        s.turns = Some(turns);
        if !saw_result {
            s.is_error = true;
        }
        s
    }

    fn passes_env(&self, name: &str) -> bool {
        (name.starts_with("COPILOT_") && !SESSION_BINDING.contains(&name)) || GITHUB_TOKENS.contains(&name)
    }

    fn auth_failures(&self) -> &'static [&'static str] {
        AUTH_FAILURES
    }

    fn effort_levels(&self) -> &'static [&'static str] {
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"]
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

    const HELP: &str = "  -p, --prompt <text>\n  --reasoning-effort <level>\n  --output-format <format>\n  --model <model>\n  --agent <agent>\n  --available-tools [<tools>...]\n  --allow-tool [<tools>...]\n  --deny-tool [<tools>...]\n  --plugin-dir <directory>\n  --fleet\n";

    #[test]
    fn detects_capabilities_from_help() {
        let r = Copilot::report(None, "GitHub Copilot CLI 1.0.91.\n", HELP);
        assert_eq!(r.version.as_deref(), Some("1.0.91"));
        let caps = r.capability_set();
        for c in [
            C::SessionStart,
            C::SessionCollect,
            C::ToolRestriction,
            C::PerCallPolicy,
            C::StopGuard,
            C::CustomAgents,
            C::ModelSelection,
            C::EffortSelection,
        ] {
            assert!(caps.has(c), "{c:?}");
        }
    }

    #[test]
    fn missing_flags_are_not_claimed() {
        let r = Copilot::report(None, "1.0.0", "  -p, --prompt <text>\n");
        assert!(!r.capability_set().has(C::ToolRestriction));
        assert!(!r.capability_set().has(C::PerCallPolicy));
    }

    #[test]
    fn plans_a_session() {
        let spec = SessionSpec {
            workdir: "/w".into(),
            prompt: "Fix it".into(),
            append_system: Some("You are the worker.".into()),
            agent: None,
            tools: ToolPolicy {
                allow: vec!["read".into(), "edit".into(), "shell".into()],
                deny: vec!["shell:git push".into()],
            },
            model: Some("gpt-5.4".into()),
            max_turns: Some(10),
            plugin_dir: Some("/p".into()),
            extra_plugin_dirs: vec!["/skills".into()],
            env: vec![("INTERLOCK_ATTEMPT".into(), "att-1".into())],
            timeout: std::time::Duration::from_secs(60),
            transcript: "/t.jsonl".into(),
            session_id: Some("0cb916db-26aa-40f2-86b5-1ba81b225fd2".into()),
            max_cost_usd: Some(1.0),
            effort: Some("high".into()),
            clear_env: true,
        };
        let plan = Copilot.plan_with("/bin/copilot".into(), &spec);
        assert_eq!(plan.args[1], "You are the worker.\n\nFix it");
        for a in ["--allow-tool=write", "--allow-tool=shell", "--deny-tool=shell(git push:*)", "--no-ask-user"] {
            assert!(plan.args.iter().any(|x| x == a), "{a} in {:?}", plan.args);
        }
        assert!(plan.args.windows(2).any(|w| w == ["--plugin-dir", "/p"]));
        assert!(plan.args.windows(2).any(|w| w == ["--plugin-dir", "/skills"]), "the skills plugin loads too");
        assert!(plan.args.windows(2).any(|w| w == ["--model", "gpt-5.4"]), "{:?}", plan.args);
        assert!(plan.env.iter().any(|(k, _)| k == "INTERLOCK_ATTEMPT"));
        assert!(plan.args.iter().any(|a| a == "--session-id=0cb916db-26aa-40f2-86b5-1ba81b225fd2"));
        // Copilot caps AI credits, not dollars, so a dollar budget is enforced by interlock alone.
        assert!(!plan.args.iter().any(|a| a.contains("credits")));
        assert!(plan.args.iter().any(|a| a == "--reasoning-effort=high"));
        assert!(!plan.args.iter().any(|a| a == "--agent"), "no agent was named");
    }

    fn policy(allow: &[&str], deny: &[&str]) -> ToolPolicy {
        ToolPolicy {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn plans_a_session_as_interlocks_agent_for_its_role() {
        let spec = SessionSpec {
            workdir: "/w".into(),
            prompt: "Check it".into(),
            append_system: Some("You are the independent verifier.".into()),
            agent: Some("interlock-verifier".into()),
            tools: policy(&["read", "shell"], &["edit", "shell:git push", "shell:git commit"]),
            model: None,
            max_turns: None,
            plugin_dir: Some("/p".into()),
            extra_plugin_dirs: vec![],
            env: vec![],
            timeout: std::time::Duration::from_secs(60),
            transcript: "/t.jsonl".into(),
            session_id: None,
            max_cost_usd: None,
            effort: None,
            clear_env: true,
        };
        let plan = Copilot.plan_with("/bin/copilot".into(), &spec);
        assert!(
            plan.args.windows(2).any(|w| w == ["--agent", "interlock-hooks:interlock-verifier"]),
            "{:?}",
            plan.args
        );
        assert_eq!(plan.args[1], "Check it", "the role's instructions go to the agent, not the prompt");
        // The tool filters stay as they were.
        for a in ["--allow-tool=shell", "--deny-tool=write", "--deny-tool=shell(git push:*)"] {
            assert!(plan.args.iter().any(|x| x == a), "{a} in {:?}", plan.args);
        }
        let agent = Copilot.agent_profile(&spec).unwrap();
        assert_eq!(agent.path, std::path::Path::new("/p/agents/interlock-verifier.agent.md"));
        assert_eq!(
            agent.text,
            "---\nname: \"interlock-verifier\"\ndescription: \"interlock-verifier, launched by interlock for one \
             attempt\"\ntools: [\"view\",\"grep\",\"glob\",\"bash\"]\n---\n\nYou are the independent verifier.\n"
        );

        // Without the hooks plugin there is nowhere to keep the profile: the instructions lead the prompt.
        let bare = SessionSpec { plugin_dir: None, ..spec };
        assert_eq!(Copilot.agent_profile(&bare), None);
        assert!(
            Copilot.plan_with("/bin/copilot".into(), &bare).args[1].starts_with("You are the independent verifier.")
        );
    }

    #[test]
    fn an_agent_lists_only_the_tools_its_grant_leaves_it() {
        let tools = |allow: &[&str], deny: &[&str]| Copilot.agent_tools(&policy(allow, deny));
        let worker = tools(&["read", "edit", "shell"], &["shell:git push", "shell:git commit"]);
        assert_eq!(worker, ["view", "grep", "glob", "create", "edit", "bash"]);
        assert!(!worker.contains(&"agent".to_string()), "no sub-agents unless the grant has `agent`");
        assert_eq!(tools(&["read", "edit"], &["edit"]), ["view", "grep", "glob"], "a whole denial removes the tool");
        assert_eq!(tools(&["shell:cargo test"], &[]), ["bash"], "a narrower allow still needs the shell");
        assert_eq!(tools(&["agent", "mcp:github/get_pr", "mcp:jira"], &[]), ["agent", "github/get_pr", "jira/*"]);
        assert_eq!(tools(&["web"], &[]), ["web_fetch", "web_search"]);
    }

    #[test]
    fn summarizes_jsonl_output() {
        let lines: Vec<String> = [
            r#"{"type":"assistant.message","data":{"content":"","toolRequests":[{"name":"bash"}]}}"#,
            r#"{"type":"tool.execution_complete","data":{"success":false,"error":{"message":"Denied by preToolUse hook: no","code":"denied"}}}"#,
            r#"{"type":"tool.execution_complete","data":{"success":true,"result":{"content":"ok"}}}"#,
            r#"{"type":"assistant.turn_end","data":{"turnId":"0"}}"#,
            r#"{"type":"assistant.message","data":{"content":"Fixed the retry."}}"#,
            r#"{"type":"assistant.turn_end","data":{"turnId":"1"}}"#,
            r#"{"type":"result","sessionId":"s-1","exitCode":0,"usage":{"premiumRequests":2,"sessionDurationMs":317}}"#,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let s = Copilot.summarize(&lines);
        assert_eq!(s.final_text.as_deref(), Some("Fixed the retry."));
        assert_eq!((s.denials, s.turns, s.is_error), (1, Some(2), false));
        assert_eq!(s.session_id.as_deref(), Some("s-1"));
        assert_eq!(s.premium_requests, Some(2.0));
        assert!(Copilot.summarize(&lines[..5]).is_error, "no result event means the session did not finish");
    }
}
