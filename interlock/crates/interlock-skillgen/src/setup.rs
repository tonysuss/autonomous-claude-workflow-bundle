//! `interlock setup`: installs what a guided session needs in a repository:
//! the generated skills, the independent verifier agent where the host uses
//! one, and interlock's hooks plugin in interactive mode, where the hooks ask
//! instead of deny. It writes only inside the repository, never through a
//! symlink, and never over a file interlock did not write (or that changed
//! since), unless forced; `.interlock/setup-manifest.json` records what it wrote.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::safe_write::{SafeWriter, WriteReport};
use crate::{Catalog, Result, Target, generate, invalid};

#[derive(Debug, Clone, Serialize)]
pub struct SetupReport {
    pub host: String,
    pub written: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    /// Files not written, with the reason. Setup fails when this is not empty.
    pub refused: Vec<(PathBuf, String)>,
    /// The plugin directory the host loads with `--plugin-dir`.
    pub plugin_dir: PathBuf,
    /// The command a person runs to start a guided session.
    pub session_command: String,
    pub skills: Vec<String>,
    /// How the verify skill reaches an independent verifier on this host.
    pub verification: String,
    pub notes: Vec<String>,
}

/// The hooks plugin's files: hooks that run interlock in interactive mode
/// against `db`. Both hosts load a Claude-format plugin with `--plugin-dir`.
pub fn guided_hooks(interlock_bin: &Path, host: &str, db: &Path) -> Vec<(PathBuf, String)> {
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
    vec![(PathBuf::from("hooks/hooks.json"), serde_json::to_string_pretty(&hooks).unwrap())]
}

fn guided_manifest() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": "interlock-guided",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "interlock hooks for guided sessions: per-call policy that asks, and the stop guard",
    }))
    .unwrap()
}

/// Installs the generated skills, the verifier agent and the hooks plugin for `host`.
///
/// - Copilot CLI: skills under `.github/skills/interlock-<name>/`; hooks in
///   `<store dir>/guided/copilot-plugin`. Verification runs as `interlock verify`.
/// - Claude Code: one plugin under `<store dir>/guided/claude-code-plugin` with
///   the skills, the verifier agent and the hooks, namespaced `interlock:`.
pub fn install(
    catalog: &Catalog,
    host: &str,
    repo: &Path,
    db: &Path,
    interlock_bin: &Path,
    force: bool,
) -> Result<SetupReport> {
    let store_dir = db.parent().map(Path::to_path_buf).unwrap_or_else(|| repo.join(".interlock"));
    let target = match host {
        "copilot" => Target::Copilot,
        "claude-code" => Target::ClaudeCode,
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
    let mut writer = SafeWriter::new(repo, Some(store_dir.join("setup-manifest.json")), force)?;
    let root = writer.root().to_path_buf();
    let store_rel = store_dir
        .canonicalize()
        .unwrap_or_else(|_| store_dir.clone())
        .strip_prefix(&root)
        .map(Path::to_path_buf)
        .map_err(|_| crate::Error::Invalid(format!("the store {} is not inside {}", db.display(), root.display())))?;
    let plugin_rel = store_rel.join("guided").join(format!("{host}-plugin"));
    match target {
        Target::Copilot => {
            output.write_with(&mut writer, Path::new(""))?;
            writer.write(&plugin_rel.join(".claude-plugin/plugin.json"), &guided_manifest())?;
        }
        _ => output.write_with(&mut writer, &plugin_rel)?,
    }
    for (rel, text) in guided_hooks(interlock_bin, host, db) {
        writer.write(&plugin_rel.join(rel), &text)?;
    }
    writer.write(&store_rel.join(".gitignore"), "*\n")?;
    let WriteReport { written, unchanged, refused } = writer.finish()?;
    let plugin = root.join(&plugin_rel);
    let plugin_arg = interlock_adapter::shell_quote(&plugin.display().to_string());
    let bin_dir = interlock_bin.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let (session_command, verification, mut notes) = match target {
        Target::Copilot => (
            format!("copilot --plugin-dir {plugin_arg}"),
            "interlock verify <task> --host copilot (an independent session interlock launches)".to_string(),
            vec![
                "Commit .github/skills/interlock-* to share the skills; the hooks plugin stays local.".to_string(),
                "Skills marked for people only are typed as /name in the interactive session; `copilot -p` \
                 does not expand them (S1)."
                    .to_string(),
            ],
        ),
        _ => (
            format!("claude --plugin-dir {plugin_arg}"),
            format!("the {} subagent, bound to its own identity by the hooks", target.agent_ref("verifier")),
            vec![
                "Skills load as interlock:<name>, which keeps them apart from built-in skills of the same name.".into(),
            ],
        ),
    };
    notes.push(format!("The skills call `interlock`; keep {bin_dir} on PATH in the session."));
    if !refused.is_empty() {
        notes.push(
            "Some files were not written; see refused. Pass --force to overwrite files that are not interlock's."
                .into(),
        );
    }
    let skills = output.skill_names();
    Ok(SetupReport {
        host: host.to_string(),
        written,
        unchanged,
        refused,
        plugin_dir: plugin,
        session_command,
        skills,
        verification,
        notes,
    })
}
