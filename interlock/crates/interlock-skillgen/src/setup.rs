//! `interlock setup`: installs what a guided session needs in a repository:
//! the generated skills, the independent verifier agent, and interlock's hooks
//! plugin in interactive mode, where the hooks ask instead of deny.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{Catalog, Result, Target, generate, invalid};

#[derive(Debug, Clone, Serialize)]
pub struct SetupReport {
    pub host: String,
    /// Every file written.
    pub written: Vec<PathBuf>,
    /// The plugin directory the host loads with `--plugin-dir`.
    pub plugin_dir: PathBuf,
    /// The command a person runs to start a guided session.
    pub session_command: String,
    pub skills: Vec<String>,
    pub agent: String,
    pub notes: Vec<String>,
}

/// Writes a Claude-format hooks plugin whose hooks run interlock in
/// interactive mode against `db`. Both hosts load it with `--plugin-dir`.
pub fn write_guided_hooks(plugin: &Path, interlock_bin: &Path, host: &str, db: &Path) -> Result<Vec<PathBuf>> {
    let quote = |p: &Path| interlock_adapter::shell_quote(&p.display().to_string());
    let command = |event: &str| {
        format!(
            "INTERLOCK_MODE=interactive INTERLOCK_HOST={host} INTERLOCK_DB={} {} hook {event}",
            quote(db),
            quote(interlock_bin)
        )
    };
    let hook = |event: &str| serde_json::json!([{ "type": "command", "command": command(event), "timeout": 60 }]);
    let hooks = serde_json::json!({
        "hooks": {
            "PreToolUse": [{ "matcher": "*", "hooks": hook("pre-tool-use") }],
            "Stop": [{ "hooks": hook("stop") }],
            "SubagentStop": [{ "hooks": hook("stop") }],
        }
    });
    let mut written = Vec::new();
    let manifest = plugin.join(".claude-plugin/plugin.json");
    if !manifest.exists() {
        let body = serde_json::json!({
            "name": "interlock-guided",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "interlock hooks for guided sessions: per-call policy that asks, and the stop guard",
        });
        std::fs::create_dir_all(manifest.parent().unwrap_or(plugin))?;
        std::fs::write(&manifest, serde_json::to_string_pretty(&body).unwrap())?;
        written.push(manifest);
    }
    let path = plugin.join("hooks/hooks.json");
    std::fs::create_dir_all(path.parent().unwrap_or(plugin))?;
    std::fs::write(&path, serde_json::to_string_pretty(&hooks).unwrap())?;
    written.push(path);
    Ok(written)
}

/// Installs the generated skills, the verifier agent and the hooks plugin for `host`.
///
/// - Copilot CLI: skills under `.github/skills/` and the agent under
///   `.github/agents/` in the repository; hooks in `<store dir>/guided/copilot-plugin`.
/// - Claude Code: one plugin under `<store dir>/guided/claude-code-plugin` with
///   the skills, the agent and the hooks, namespaced `interlock:`.
pub fn install(catalog: &Catalog, host: &str, repo: &Path, db: &Path, interlock_bin: &Path) -> Result<SetupReport> {
    let store_dir = db.parent().map(Path::to_path_buf).unwrap_or_else(|| repo.join(".interlock"));
    let (target, out_dir, plugin) = match host {
        "copilot" => (Target::Copilot, repo.to_path_buf(), store_dir.join("guided/copilot-plugin")),
        "claude-code" => {
            let plugin = store_dir.join("guided/claude-code-plugin");
            (Target::ClaudeCode, plugin.clone(), plugin)
        }
        other => return invalid(format!("unknown host {other}; setup knows copilot and claude-code")),
    };
    let output = generate(catalog, target)?;
    let problems: Vec<String> = crate::validate::check_output(&output)
        .into_iter()
        .filter(|p| p.severity == crate::validate::Severity::Error)
        .map(|p| format!("{}: {}", p.path.display(), p.message))
        .collect();
    if !problems.is_empty() {
        return invalid(format!("the generated files do not validate: {}", problems.join("; ")));
    }
    let mut written = output.write(&out_dir)?;
    written.extend(write_guided_hooks(&plugin, interlock_bin, host, db)?);
    let ignore = store_dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::create_dir_all(&store_dir)?;
        std::fs::write(&ignore, "*\n")?;
    }
    let plugin_arg = interlock_adapter::shell_quote(&plugin.display().to_string());
    let bin_dir = interlock_bin.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let (session_command, agent, mut notes) = match target {
        Target::Copilot => (
            format!("copilot --plugin-dir {plugin_arg}"),
            target.agent_ref("verifier"),
            vec![
                "Commit .github/skills and .github/agents to share them; the hooks plugin stays local.".to_string(),
                "Skills marked for people only are typed as /name in the interactive session; `copilot -p` \
                 does not expand them (S1)."
                    .to_string(),
            ],
        ),
        _ => (
            format!("claude --plugin-dir {plugin_arg}"),
            target.agent_ref("verifier"),
            vec![
                "Skills load as interlock:<name>, which keeps them apart from built-in skills of the same name.".into(),
            ],
        ),
    };
    notes.push(format!("The skills call `interlock`; keep {bin_dir} on PATH in the session."));
    Ok(SetupReport {
        host: host.to_string(),
        written,
        plugin_dir: plugin,
        session_command,
        skills: output.skill_names(),
        agent,
        notes,
    })
}
