//! `interlock skills` and `interlock setup`: host files from the canonical
//! skills, static validation, and the install for a guided session.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::Subcommand;
use interlock_adapter::HostReport;
use interlock_skillgen::validate::{self, Severity};
use interlock_skillgen::{Catalog, Target};
use serde_json::json;

#[derive(Subcommand)]
pub enum SkillsCmd {
    /// Write the skills (and the verifier agent) for one target.
    Generate {
        /// copilot, claude-code or agent-skills.
        #[arg(long)]
        target: String,
        #[arg(long)]
        out: PathBuf,
        /// Read canonical skills from this interlock/ directory instead of the built-in copy.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Statically check a directory of skill folders.
    Validate {
        dir: PathBuf,
        /// Which fields to accept: agent-skills (the standard only), copilot or claude-code.
        #[arg(long, default_value = "agent-skills")]
        target: String,
    },
    /// The canonical skills and what each one is for.
    List {
        #[arg(long)]
        from: Option<PathBuf>,
    },
}

fn target(name: &str) -> Result<Target> {
    Target::parse(name).ok_or_else(|| anyhow!("unknown target {name}; use copilot, claude-code or agent-skills"))
}

fn catalog(from: Option<&Path>) -> Result<Catalog> {
    Ok(match from {
        Some(dir) => Catalog::from_dir(dir)?,
        None => Catalog::embedded()?,
    })
}

fn print(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub fn skills(cmd: &SkillsCmd) -> Result<ExitCode> {
    match cmd {
        SkillsCmd::Generate { target: name, out, from } => {
            let target = target(name)?;
            let output = interlock_skillgen::generate(&catalog(from.as_deref())?, target)?;
            let problems = validate::check_output(&output);
            if problems.iter().any(|p| p.severity == Severity::Error) {
                print(&json!({"written": [], "problems": problems}));
                return Ok(ExitCode::from(1));
            }
            let written = output.write(out)?;
            print(&json!({
                "target": target.name(),
                "out": out,
                "skills": output.skill_names(),
                "written": written,
                "problems": problems,
            }));
            Ok(ExitCode::SUCCESS)
        }
        SkillsCmd::Validate { dir, target: name } => {
            let problems = validate::check_dir(dir, target(name)?)?;
            let valid = !problems.iter().any(|p| p.severity == Severity::Error);
            print(&json!({"valid": valid, "dir": dir, "target": name, "problems": problems}));
            Ok(if valid { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
        SkillsCmd::List { from } => {
            let catalog = catalog(from.as_deref())?;
            let skills: Vec<_> = catalog
                .skills
                .iter()
                .map(|s| {
                    json!({
                        "name": s.meta.name,
                        "invocation": s.meta.invocation,
                        "pack": s.meta.pack,
                        "routers": s.meta.routers,
                        "description": s.meta.description,
                    })
                })
                .collect();
            let agents: Vec<_> =
                catalog.agents.iter().map(|a| json!({"name": a.meta.name, "tools": a.meta.tools})).collect();
            print(&json!({"skills": skills, "agents": agents}));
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Installs the guided-session files for `host` in the repository at `repo`.
pub fn setup(host: &str, repo: &Path, db: &Path, from: Option<&Path>, inspected: &HostReport) -> Result<ExitCode> {
    let bin = std::env::current_exe()?;
    let report = interlock_skillgen::setup::install(&catalog(from)?, host, repo, db, &bin)?;
    let mut out = serde_json::to_value(&report)?;
    out["host_report"] = json!({
        "installed": inspected.installed,
        "version": inspected.version,
        "capabilities": inspected.capability_set(),
        "notes": inspected.notes,
    });
    print(&out);
    Ok(ExitCode::SUCCESS)
}
