//! Canonical, host-neutral skills to each host's files.
//!
//! A canonical skill is a folder under `interlock/skills/` with `skill.toml`
//! (what it means: its invocation intent, requirements and pack) and `SKILL.md`
//! (a host-neutral body). Agents live under `interlock/agents/` the same way.
//! The generator decides how to say each one for Copilot CLI, Claude Code, or
//! plain Agent Skills, and `validate` checks what it wrote.

mod frontmatter;
pub mod safe_write;
pub mod setup;
pub mod validate;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use frontmatter::{Frontmatter, Value, split as split_frontmatter};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

/// Who starts a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Invocation {
    /// The agent picks it when the description matches.
    Model,
    /// Only a person, by name. Hidden from the agent.
    User,
    /// Never standalone: emitted as reference files under the skills that route to it.
    Routed,
}

impl Invocation {
    pub fn name(self) -> &'static str {
        match self {
            Invocation::Model => "model",
            Invocation::User => "user",
            Invocation::Routed => "routed",
        }
    }
}

/// What a skill needs to be present to work.
pub const KNOWN_REQUIREMENTS: &[&str] = &["interlock-cli"];

/// The section of a standalone skill's body that says how to work without
/// interlock: no task, no attempt, no store.
pub const STANDALONE_HEADING: &str = "\n## Without interlock\n";
pub const KNOWN_PACKS: &[&str] = &["core"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub invocation: Invocation,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Also works where its requirements are missing: it follows its
    /// playbook and records nothing. Its body says how, under
    /// [`STANDALONE_HEADING`].
    #[serde(default)]
    pub standalone: bool,
    pub pack: String,
    /// For routed skills: the skills whose `references/` carry this one.
    #[serde(default)]
    pub routers: Vec<String>,
    /// Shown by hosts that prompt for arguments to a user-invoked skill.
    pub argument_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub meta: SkillMeta,
    pub body: String,
    /// Files under the skill's own `references/`, by file name.
    pub references: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentMeta {
    pub name: String,
    pub description: String,
    /// Host-neutral tools: `read`, `shell`, `edit`, `web`.
    pub tools: Vec<String>,
    /// The skill that hands work to this agent. Hosts without custom agents
    /// get the agent's instructions as a reference of that skill.
    pub handoff_from: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub meta: AgentMeta,
    pub body: String,
}

/// Every canonical skill and agent.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub skills: Vec<Skill>,
    pub agents: Vec<Agent>,
    /// Attribution for adapted material, copied to every output.
    pub notice: Option<String>,
}

/// Lowercase letters, digits and single hyphens, at most 64 characters: the
/// Agent Skills rule, which Copilot CLI and Claude Code also accept.
pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// Problems with a description, by the strictest rule any target applies.
pub fn description_problem(description: &str) -> Option<String> {
    let len = description.chars().count();
    if description.trim().is_empty() {
        Some("the description is empty".into())
    } else if len > 1024 {
        Some(format!("the description has {len} characters; the limit is 1024"))
    } else if description.contains(['<', '>']) {
        Some("the description contains < or >, which some hosts reject".into())
    } else {
        None
    }
}

impl Catalog {
    /// The skills built into this binary.
    pub fn embedded() -> Result<Catalog> {
        Catalog::from_files(embedded::FILES.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect())
    }

    /// Reads `skills/` and `agents/` under `root` (the `interlock/` directory).
    pub fn from_dir(root: &Path) -> Result<Catalog> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
            if !dir.exists() {
                return Ok(());
            }
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    walk(root, &path, out)?;
                } else {
                    let rel = path.strip_prefix(root).map_err(|e| Error::Invalid(e.to_string()))?;
                    out.push((rel.to_string_lossy().replace('\\', "/"), std::fs::read_to_string(&path)?));
                }
            }
            Ok(())
        }
        let mut files = Vec::new();
        walk(root, &root.join("skills"), &mut files)?;
        walk(root, &root.join("agents"), &mut files)?;
        if files.is_empty() {
            return invalid(format!("no skills under {}", root.join("skills").display()));
        }
        files.sort();
        Catalog::from_files(files)
    }

    /// Builds and checks a catalog from `(path relative to interlock/, contents)` pairs.
    pub fn from_files(files: Vec<(String, String)>) -> Result<Catalog> {
        let mut skills: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut agents: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut notice = None;
        for (path, text) in files {
            let parts: Vec<&str> = path.splitn(3, '/').collect();
            match parts.as_slice() {
                ["skills", "NOTICE"] => notice = Some(text),
                ["skills", dir, rest] => {
                    skills.entry(dir.to_string()).or_default().insert(rest.to_string(), text);
                }
                ["agents", dir, rest] => {
                    agents.entry(dir.to_string()).or_default().insert(rest.to_string(), text);
                }
                _ => return invalid(format!("{path}: not a file in a skill or agent folder")),
            }
        }
        let mut catalog = Catalog { notice, ..Default::default() };
        for (dir, mut files) in skills {
            let ctx = format!("skills/{dir}");
            let meta: SkillMeta = toml::from_str(
                &files.remove("skill.toml").ok_or_else(|| Error::Invalid(format!("{ctx}: missing skill.toml")))?,
            )
            .map_err(|e| Error::Invalid(format!("{ctx}/skill.toml: {e}")))?;
            let body = files.remove("SKILL.md").ok_or_else(|| Error::Invalid(format!("{ctx}: missing SKILL.md")))?;
            let mut references = Vec::new();
            for (rel, text) in files {
                match rel.strip_prefix("references/") {
                    Some(name) if name.ends_with(".md") && !name.contains('/') => {
                        references.push((name.to_string(), text))
                    }
                    _ => return invalid(format!("{ctx}/{rel}: only references/*.md may sit beside SKILL.md")),
                }
            }
            if meta.name != dir {
                return invalid(format!("{ctx}: skill.toml names it {}; the name must match its folder", meta.name));
            }
            catalog.skills.push(Skill { meta, body, references });
        }
        for (dir, mut files) in agents {
            let ctx = format!("agents/{dir}");
            let meta: AgentMeta = toml::from_str(
                &files.remove("agent.toml").ok_or_else(|| Error::Invalid(format!("{ctx}: missing agent.toml")))?,
            )
            .map_err(|e| Error::Invalid(format!("{ctx}/agent.toml: {e}")))?;
            let body = files.remove("AGENT.md").ok_or_else(|| Error::Invalid(format!("{ctx}: missing AGENT.md")))?;
            if let Some(extra) = files.keys().next() {
                return invalid(format!("{ctx}/{extra}: agents have only agent.toml and AGENT.md"));
            }
            if meta.name != dir {
                return invalid(format!("{ctx}: agent.toml names it {}; the name must match its folder", meta.name));
            }
            catalog.agents.push(Agent { meta, body });
        }
        catalog.check()?;
        Ok(catalog)
    }

    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.meta.name == name)
    }

    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.meta.name == name)
    }

    fn check(&self) -> Result<()> {
        for s in &self.skills {
            let m = &s.meta;
            let ctx = format!("skills/{}", m.name);
            if !valid_name(&m.name) {
                return invalid(format!("{ctx}: names are lowercase letters, digits and single hyphens"));
            }
            if let Some(p) = description_problem(&m.description) {
                return invalid(format!("{ctx}: {p}"));
            }
            if s.body.starts_with("---") {
                return invalid(format!("{ctx}/SKILL.md: canonical bodies carry no frontmatter; skill.toml holds it"));
            }
            if s.body.trim().is_empty() {
                return invalid(format!("{ctx}/SKILL.md is empty"));
            }
            if let Some(r) = m.requires.iter().find(|r| !KNOWN_REQUIREMENTS.contains(&r.as_str())) {
                return invalid(format!("{ctx}: unknown requirement {r}"));
            }
            if !KNOWN_PACKS.contains(&m.pack.as_str()) {
                return invalid(format!("{ctx}: unknown pack {}", m.pack));
            }
            if m.standalone {
                if m.invocation != Invocation::Model {
                    return invalid(format!("{ctx}: only a model-invoked skill can be standalone"));
                }
                if m.requires.is_empty() {
                    return invalid(format!("{ctx}: standalone means it works without its requirements; it has none"));
                }
                if !s.body.contains(STANDALONE_HEADING) {
                    return invalid(format!(
                        "{ctx}/SKILL.md: a standalone skill says how to work without interlock under {:?}",
                        STANDALONE_HEADING.trim()
                    ));
                }
            }
            match m.invocation {
                Invocation::Routed => {
                    if m.routers.is_empty() {
                        return invalid(format!("{ctx}: a routed skill names at least one router"));
                    }
                    if !s.references.is_empty() {
                        return invalid(format!("{ctx}: a routed skill is itself a reference and has none of its own"));
                    }
                    for r in &m.routers {
                        match self.skill(r) {
                            Some(router) if router.meta.invocation != Invocation::Routed => {}
                            Some(_) => return invalid(format!("{ctx}: router {r} is itself routed")),
                            None => return invalid(format!("{ctx}: router {r} does not exist")),
                        }
                        let routed_ref = format!("references/{}.md", m.name);
                        if self.skill(r).is_some_and(|router| !router.body.contains(&routed_ref)) {
                            return invalid(format!(
                                "skills/{r}/SKILL.md routes to {} but never mentions {routed_ref}",
                                m.name
                            ));
                        }
                    }
                }
                _ if !m.routers.is_empty() => return invalid(format!("{ctx}: only routed skills name routers")),
                _ => {}
            }
        }
        for a in &self.agents {
            let ctx = format!("agents/{}", a.meta.name);
            if !valid_name(&a.meta.name) {
                return invalid(format!("{ctx}: names are lowercase letters, digits and single hyphens"));
            }
            if let Some(p) = description_problem(&a.meta.description) {
                return invalid(format!("{ctx}: {p}"));
            }
            if let Some(t) = a.meta.tools.iter().find(|t| !["read", "shell", "edit", "web"].contains(&t.as_str())) {
                return invalid(format!("{ctx}: unknown tool {t}; use read, shell, edit or web"));
            }
            if self.skill(&a.meta.handoff_from).is_none_or(|s| s.meta.invocation == Invocation::Routed) {
                return invalid(format!("{ctx}: handoff-from names no standalone skill"));
            }
        }
        // Every template in every body must resolve for every target.
        for target in Target::ALL {
            generate(self, target)?;
        }
        Ok(())
    }
}

/// Where generated files go and how they are phrased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    /// `.github/skills/` and `.github/agents/`, for a repository.
    Copilot,
    /// A Claude Code plugin: skills and agents under the `interlock:` namespace.
    ClaudeCode,
    /// Plain Agent Skills folders, with the invocation intent kept in metadata.
    AgentSkills,
}

impl Target {
    pub const ALL: [Target; 3] = [Target::Copilot, Target::ClaudeCode, Target::AgentSkills];

    pub fn name(self) -> &'static str {
        match self {
            Target::Copilot => "copilot",
            Target::ClaudeCode => "claude-code",
            Target::AgentSkills => "agent-skills",
        }
    }

    pub fn parse(s: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.name() == s)
    }

    /// A skill's emitted name. On Copilot, project skills share one namespace
    /// with a repository's own, so interlock's carry an `interlock-` prefix;
    /// Claude Code's plugin namespace does the same job.
    pub fn skill_name(self, name: &str) -> String {
        match self {
            Target::Copilot => format!("{PLUGIN}-{name}"),
            _ => name.to_string(),
        }
    }

    /// The folder holding one skill, relative to the output directory.
    pub fn skill_dir(self, name: &str) -> PathBuf {
        self.skills_root().join(self.skill_name(name))
    }

    /// The folder holding every skill, relative to the output directory.
    pub fn skills_root(self) -> PathBuf {
        match self {
            Target::Copilot => PathBuf::from(".github/skills"),
            Target::ClaudeCode => PathBuf::from("skills"),
            Target::AgentSkills => PathBuf::new(),
        }
    }

    /// How a body names another skill.
    pub fn skill_ref(self, name: &str) -> String {
        match self {
            Target::ClaudeCode => format!("{PLUGIN}:{name}"),
            _ => self.skill_name(name),
        }
    }

    /// How a body names an agent.
    pub fn agent_ref(self, name: &str) -> String {
        match self {
            Target::ClaudeCode => format!("{PLUGIN}:{name}"),
            _ => format!("{PLUGIN}-{name}"),
        }
    }
}

/// The Claude Code plugin's name, and the prefix of Copilot agent names.
pub const PLUGIN: &str = "interlock";

/// Generated files, by path relative to the output directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub target: Target,
    pub files: BTreeMap<PathBuf, String>,
}

impl Output {
    /// Writes every file under `dir`: never through a symlink, never outside
    /// `dir`, and never over a file that holds something else.
    pub fn write(&self, dir: &Path) -> Result<safe_write::WriteReport> {
        let mut writer = safe_write::SafeWriter::new(dir, None, false)?;
        self.write_with(&mut writer, Path::new(""))?;
        writer.finish()
    }

    /// Writes every file through `writer`, under `prefix` within its root.
    pub fn write_with(&self, writer: &mut safe_write::SafeWriter, prefix: &Path) -> Result<()> {
        for (rel, text) in &self.files {
            writer.write(&prefix.join(rel), text)?;
        }
        Ok(())
    }

    /// The names of the standalone skills this output holds.
    pub fn skill_names(&self) -> Vec<String> {
        let root = self.target.skills_root();
        self.files
            .keys()
            .filter(|p| p.file_name().is_some_and(|f| f == "SKILL.md"))
            .filter_map(|p| p.strip_prefix(&root).ok()?.parent()?.to_str().map(str::to_string))
            .collect()
    }
}

/// Expands `{{host}}`, `{{skill:NAME}}`, `{{agent:NAME}}` and `{{handoff:NAME}}`.
fn render(text: &str, target: Target, catalog: &Catalog, ctx: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| Error::Invalid(format!("{ctx}: unclosed {{{{")))?;
        let var = after[..end].trim();
        out.push_str(&expand(var, target, catalog).map_err(|e| Error::Invalid(format!("{ctx}: {e}")))?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn expand(var: &str, target: Target, catalog: &Catalog) -> std::result::Result<String, String> {
    let agent = |name: &str| catalog.agent(name).map(|_| ()).ok_or_else(|| format!("no agent named {name}"));
    match var.split_once(':') {
        None if var == "host" => Ok(match target {
            Target::Copilot => "copilot".into(),
            Target::ClaudeCode => "claude-code".into(),
            Target::AgentSkills => "<host>".into(),
        }),
        Some(("skill", name)) => match catalog.skill(name) {
            Some(s) if s.meta.invocation != Invocation::Routed => Ok(target.skill_ref(name)),
            Some(_) => Err(format!("{name} is routed; link its reference file instead")),
            None => Err(format!("no skill named {name}")),
        },
        Some(("agent", name)) => agent(name).map(|()| target.agent_ref(name)),
        Some(("handoff", name)) => agent(name).map(|()| handoff(target, name)),
        _ => Err(format!("unknown template {{{{{var}}}}}")),
    }
}

/// How the verify skill hands work to an agent, per host. Independence must be
/// something interlock can see: Claude Code's hooks name the subagent that is
/// calling, so the verifier subagent's attempt is bound to it. Copilot's hooks
/// do not name a subagent's type, so interlock launches the verifier itself.
fn handoff(target: Target, name: &str) -> String {
    const PROMPT: &str = "> Verify interlock task `<id>` in the repository at `<absolute path of the repository root>`. \
Host: `{host}`. Open your own verifier attempt, have interlock run every check on the submitted files, record one \
assessment per criterion, end your attempt, and reply with your attempt id and one line per criterion. If a review \
of this task listed findings to act on, check each one: `<findings, or \"none\">`.";
    match target {
        Target::Copilot => {
            "Run `interlock verify <id> --host copilot` from the repository root. Copilot does not tell \
interlock's hooks which subagent is calling, so interlock launches the independent verifier itself: a separate \
session with read and test tools only, on exactly the submitted files, which records its own evidence; interlock then \
applies what the evidence allows. Do not open a verifier attempt yourself: this session did the work, and the hooks \
refuse it."
                .into()
        }
        Target::ClaudeCode => format!(
            "Call the Agent tool with `subagent_type: \"{}\"` and this prompt, filled in:\n\n{}\n\nThe hooks see the \
verifier subagent's own identity and bind its attempt to it, so only it can use the attempt's credentials, and its \
assessments count. A verifier attempt opened any other way is unbound, and this session, which did the work, may not \
open one at all.",
            target.agent_ref(name),
            PROMPT.replace("{host}", "claude-code")
        ),
        Target::AgentSkills => format!(
            "Where interlock knows your host (`copilot` or `claude-code`), run `interlock verify <id> --host <host>`: \
interlock launches the independent verifier itself. Otherwise start a sub-agent that has read and shell tools only, \
with `references/{name}-agent.md` as its instructions and this prompt, filled in:\n\n{}\n\nA verifier interlock \
cannot bind is unbound: its assessments count only where interlock ran the criterion's check.",
            PROMPT.replace("{host}", "<host>")
        ),
    }
}

/// Host tool names for an agent's host-neutral tools.
fn agent_tools(tools: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tools {
        let names: &[&str] = match t.as_str() {
            "read" => &["Read", "Grep", "Glob"],
            "shell" => &["Bash"],
            "edit" => &["Edit", "Write"],
            _ => &["WebFetch"],
        };
        for n in names {
            if !out.iter().any(|o| o == n) {
                out.push(n.to_string());
            }
        }
    }
    out
}

/// Generates every file for one target.
pub fn generate(catalog: &Catalog, target: Target) -> Result<Output> {
    let mut files = BTreeMap::new();
    for skill in catalog.skills.iter().filter(|s| s.meta.invocation != Invocation::Routed) {
        let m = &skill.meta;
        let ctx = format!("skills/{}", m.name);
        let dir = target.skill_dir(&m.name);
        let mut fm = Frontmatter::default();
        fm.push("name", Value::Str(target.skill_name(&m.name)));
        fm.push("description", Value::Str(m.description.clone()));
        match target {
            Target::AgentSkills => {
                if !m.requires.is_empty() {
                    let needs = m.requires.join(", ");
                    fm.push(
                        "compatibility",
                        Value::Str(if m.standalone {
                            format!("Uses {needs} on PATH where the work is an interlock task; works without it")
                        } else {
                            format!("Requires {needs} on PATH")
                        }),
                    );
                }
                fm.push(
                    "metadata",
                    Value::Map(vec![
                        ("interlock-invocation".into(), m.invocation.name().into()),
                        ("interlock-pack".into(), m.pack.clone()),
                        ("interlock-requires".into(), m.requires.join(" ")),
                        ("interlock-standalone".into(), m.standalone.to_string()),
                    ]),
                );
            }
            Target::Copilot | Target::ClaudeCode => {
                // S1: on both hosts this hides the skill from the agent and keeps
                // it available to a person by name.
                if m.invocation == Invocation::User {
                    fm.push("disable-model-invocation", Value::Bool(true));
                    if let Some(hint) = &m.argument_hint {
                        fm.push("argument-hint", Value::Str(hint.clone()));
                    }
                }
            }
        }
        let body = render(&skill.body, target, catalog, &format!("{ctx}/SKILL.md"))?;
        files.insert(dir.join("SKILL.md"), format!("{}\n{}", fm.render(), body));
        for (name, text) in &skill.references {
            let text = render(text, target, catalog, &format!("{ctx}/references/{name}"))?;
            files.insert(dir.join("references").join(name), text);
        }
    }
    for routed in catalog.skills.iter().filter(|s| s.meta.invocation == Invocation::Routed) {
        let text = render(&routed.body, target, catalog, &format!("skills/{}/SKILL.md", routed.meta.name))?;
        for router in &routed.meta.routers {
            let path = target.skill_dir(router).join("references").join(format!("{}.md", routed.meta.name));
            if files.insert(path.clone(), text.clone()).is_some() {
                return invalid(format!("{} is generated twice", path.display()));
            }
        }
    }
    for agent in &catalog.agents {
        let m = &agent.meta;
        let body = render(&agent.body, target, catalog, &format!("agents/{}/AGENT.md", m.name))?;
        let tools = agent_tools(&m.tools);
        match target {
            // Copilot verifies through `interlock verify`, so it gets no custom agent.
            Target::Copilot => {}
            Target::ClaudeCode => {
                let mut fm = Frontmatter::default();
                fm.push("name", Value::Str(m.name.clone()));
                fm.push("description", Value::Str(m.description.clone()));
                fm.push("tools", Value::Str(tools.join(", ")));
                files.insert(
                    PathBuf::from("agents").join(format!("{}.md", m.name)),
                    format!("{}\n{}", fm.render(), body),
                );
            }
            Target::AgentSkills => {
                let path = target.skill_dir(&m.handoff_from).join("references").join(format!("{}-agent.md", m.name));
                files.insert(path, body);
            }
        }
    }
    if target == Target::ClaudeCode {
        let manifest = serde_json::json!({
            "name": PLUGIN,
            "version": env!("CARGO_PKG_VERSION"),
            "description": "interlock skills and the independent verifier agent",
        });
        files.insert(PathBuf::from(".claude-plugin/plugin.json"), serde_json::to_string_pretty(&manifest).unwrap());
    }
    if let Some(notice) = &catalog.notice {
        files.insert(target.skills_root().join("NOTICE"), notice.clone());
    }
    Ok(Output { target, files })
}
