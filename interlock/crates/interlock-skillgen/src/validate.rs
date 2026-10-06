//! Static checks on generated skills: the Agent Skills rules for names,
//! descriptions and fields, plus each host's extra fields, and that every
//! `references/` file a body mentions exists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::frontmatter::{Value, split};
use crate::{Output, Target, description_problem, valid_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub path: PathBuf,
    pub severity: Severity,
    pub message: String,
}

/// Frontmatter fields the Agent Skills specification defines.
pub const AGENT_SKILLS_FIELDS: &[&str] =
    &["name", "description", "license", "compatibility", "metadata", "allowed-tools"];

/// Fields a target accepts beyond the Agent Skills ones.
fn extra_fields(target: Target) -> &'static [&'static str] {
    match target {
        Target::AgentSkills => &[],
        Target::Copilot => &["disable-model-invocation", "user-invocable", "argument-hint"],
        Target::ClaudeCode => &["disable-model-invocation", "user-invocable", "argument-hint", "model"],
    }
}

/// Bodies longer than this load slowly; the Agent Skills guidance is to split them.
pub const MAX_BODY_LINES: usize = 500;

/// Checks one SKILL.md. `dir_name` is its folder's name; `exists` answers
/// whether a path relative to the skill folder exists.
pub fn check_skill(
    path: &Path,
    dir_name: &str,
    text: &str,
    target: Target,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut err = |m: String| out.push(Problem { path: path.to_path_buf(), severity: Severity::Error, message: m });
    let (fm, body) = match split(text) {
        Ok(Some(v)) => v,
        Ok(None) => {
            err("no frontmatter: SKILL.md must start with --- and name and description fields".into());
            return out;
        }
        Err(e) => {
            err(format!("unreadable frontmatter: {e}"));
            return out;
        }
    };
    for (key, _) in &fm.0 {
        if !AGENT_SKILLS_FIELDS.contains(&key.as_str()) && !extra_fields(target).contains(&key.as_str()) {
            err(format!("unknown field {key} for {}", target.name()));
        }
    }
    match fm.get("name") {
        None => err("missing required field name".into()),
        Some(Value::Str(name)) => {
            if !valid_name(name) {
                err(format!("name {name:?} must be 1 to 64 lowercase letters, digits and single hyphens"));
            }
            if name != dir_name {
                err(format!("name {name:?} does not match its folder {dir_name:?}"));
            }
        }
        Some(_) => err("name must be a string".into()),
    }
    match fm.get("description") {
        None => err("missing required field description".into()),
        Some(Value::Str(d)) => {
            if let Some(p) = description_problem(d) {
                err(p);
            }
        }
        Some(_) => err("description must be a string".into()),
    }
    match fm.get("compatibility") {
        Some(Value::Str(c)) if c.chars().count() > 500 => err("compatibility is longer than 500 characters".into()),
        Some(Value::Str(_)) | None => {}
        Some(_) => err("compatibility must be a string".into()),
    }
    if let Some(v) = fm.get("metadata") {
        if !matches!(v, Value::Map(_)) {
            err("metadata must be a mapping of strings".into());
        }
    }
    for flag in ["disable-model-invocation", "user-invocable"] {
        if fm.get(flag).is_some_and(|v| !matches!(v, Value::Bool(_))) {
            err(format!("{flag} must be true or false"));
        }
    }
    if body.trim().is_empty() {
        err("the body is empty".into());
    }
    if body.contains("{{") {
        err("the body has an unexpanded {{template}}".into());
    }
    for reference in referenced_files(body) {
        if !exists(&reference) {
            err(format!("the body mentions {reference}, which does not exist"));
        }
    }
    if body.lines().count() > MAX_BODY_LINES {
        out.push(Problem {
            path: path.to_path_buf(),
            severity: Severity::Warning,
            message: format!("the body is longer than {MAX_BODY_LINES} lines; move detail into references/"),
        });
    }
    out
}

/// `references/<file>` paths a body mentions.
fn referenced_files(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in body.match_indices("references/") {
        let rest = &body[i..];
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || "/._-".contains(c))).unwrap_or(rest.len());
        let path = rest[..end].trim_end_matches(['.', '/']);
        if path.len() > "references/".len() && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

/// Checks a generated output in memory.
pub fn check_output(output: &Output) -> Vec<Problem> {
    let mut out = Vec::new();
    let root = output.target.skills_root();
    for (path, text) in &output.files {
        if path.file_name().is_none_or(|f| f != "SKILL.md") {
            continue;
        }
        let Some(dir) = path.parent() else { continue };
        let dir_name = dir.file_name().and_then(|f| f.to_str()).unwrap_or_default();
        if dir.parent() != Some(root.as_path()) {
            out.push(Problem {
                path: path.clone(),
                severity: Severity::Error,
                message: format!("skills sit one level under {}", root.display()),
            });
        }
        let exists = |rel: &str| output.files.contains_key(&dir.join(rel));
        out.extend(check_skill(path, dir_name, text, output.target, &exists));
    }
    out.extend(check_agents(output));
    out
}

/// Agent files: a name that matches the file, a description, and tools.
fn check_agents(output: &Output) -> Vec<Problem> {
    let dir = PathBuf::from(if output.target == Target::Copilot { ".github/agents" } else { "agents" });
    output
        .files
        .iter()
        .filter(|(p, _)| output.target != Target::AgentSkills && p.parent() == Some(dir.as_path()))
        .flat_map(|(path, text)| check_agent(path, text))
        .collect()
}

/// One agent file (`<name>.md`, or Copilot's `<name>.agent.md`).
pub fn check_agent(path: &Path, text: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut err = |m: String| out.push(Problem { path: path.to_path_buf(), severity: Severity::Error, message: m });
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or_default();
    let stem = file.strip_suffix(".agent.md").or_else(|| file.strip_suffix(".md")).unwrap_or(file);
    match split(text) {
        Ok(Some((fm, body))) => {
            if fm.str("name") != Some(stem) {
                err(format!("the agent's name must match its file name {stem:?}"));
            }
            if fm.str("description").is_none_or(|d| description_problem(d).is_some()) {
                err("the agent needs a description of at most 1024 characters".into());
            }
            if fm.get("tools").is_none() {
                err("the agent lists no tools; it would get every tool".into());
            }
            if body.trim().is_empty() {
                err("the agent's instructions are empty".into());
            }
        }
        Ok(None) => err("the agent file needs frontmatter".into()),
        Err(e) => err(format!("unreadable frontmatter: {e}")),
    }
    out
}

/// Checks skills on disk. `root` is a folder of skill folders, a Claude Code
/// plugin (its `skills/` and `agents/`), or a repository with `.github/skills`
/// (and `.github/agents`). Agent files beside the skills are checked too.
pub fn check_dir(root: &Path, target: Target) -> std::io::Result<Vec<Problem>> {
    let (skills, agents) = if root.join(".claude-plugin").is_dir() {
        (root.join("skills"), root.join("agents"))
    } else if root.join(".github/skills").is_dir() {
        (root.join(".github/skills"), root.join(".github/agents"))
    } else {
        (root.to_path_buf(), root.parent().map(|p| p.join("agents")).unwrap_or_default())
    };
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&skills)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in entries.into_iter().filter(|p| p.is_dir()) {
        let skill = dir.join("SKILL.md");
        if !skill.exists() {
            continue;
        }
        let dir_name = dir.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_string();
        let text = std::fs::read_to_string(&skill)?;
        let exists = |rel: &str| dir.join(rel).exists();
        out.extend(check_skill(&skill, &dir_name, &text, target, &exists));
        seen.insert(dir_name, skill);
    }
    if seen.is_empty() {
        out.push(Problem {
            path: skills.clone(),
            severity: Severity::Error,
            message: "no skill folders (a folder holding SKILL.md) here".into(),
        });
    }
    if target != Target::AgentSkills && agents.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&agents)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        for f in files.iter().filter(|f| f.extension().is_some_and(|e| e == "md")) {
            out.extend(check_agent(f, &std::fs::read_to_string(f)?));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problems(text: &str, dir: &str, target: Target) -> Vec<String> {
        check_skill(Path::new("x/SKILL.md"), dir, text, target, &|r: &str| r == "references/ok.md")
            .into_iter()
            .filter(|p| p.severity == Severity::Error)
            .map(|p| p.message)
            .collect()
    }

    #[test]
    fn a_good_skill_passes() {
        let text = "---\nname: verify\ndescription: Checks the work.\nmetadata:\n  interlock-invocation: model\n---\nRead references/ok.md.\n";
        assert_eq!(problems(text, "verify", Target::AgentSkills), Vec::<String>::new());
    }

    #[test]
    fn names_descriptions_and_fields_are_enforced() {
        let p = problems("---\nname: Poteto Mode\ndescription: x\n---\nbody\n", "Poteto Mode", Target::AgentSkills);
        assert!(p.iter().any(|m| m.contains("lowercase")), "{p:?}");
        let p = problems("---\nname: make-bot-ui\ndescription: x\n---\nbody\n", "make-bot", Target::AgentSkills);
        assert!(p.iter().any(|m| m.contains("does not match its folder")), "{p:?}");
        let p = problems("---\nname: a--b\ndescription: x\n---\nbody\n", "a--b", Target::AgentSkills);
        assert!(p.iter().any(|m| m.contains("lowercase")), "consecutive hyphens: {p:?}");
        let p = problems("---\nname: a\n---\nbody\n", "a", Target::AgentSkills);
        assert!(p.iter().any(|m| m.contains("missing required field description")), "{p:?}");
        let long = "d".repeat(1025);
        let p = problems(&format!("---\nname: a\ndescription: {long}\n---\nbody\n"), "a", Target::AgentSkills);
        assert!(p.iter().any(|m| m.contains("1024")), "{p:?}");
        let p = problems(
            "---\nname: a\ndescription: x\ndisable-model-invocation: true\n---\nb\n",
            "a",
            Target::AgentSkills,
        );
        assert!(p.iter().any(|m| m.contains("unknown field disable-model-invocation")), "{p:?}");
        let ok =
            problems("---\nname: a\ndescription: x\ndisable-model-invocation: true\n---\nb\n", "a", Target::Copilot);
        assert!(ok.is_empty(), "Copilot accepts the flag: {ok:?}");
    }

    #[test]
    fn missing_references_and_templates_are_caught() {
        let p =
            problems("---\nname: a\ndescription: x\n---\nSee references/gone.md and {{host}}.\n", "a", Target::Copilot);
        assert!(p.iter().any(|m| m.contains("references/gone.md")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("template")), "{p:?}");
        assert_eq!(problems("no frontmatter\n", "a", Target::Copilot).len(), 1);
    }
}
