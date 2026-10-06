//! The canonical skills, the generator's output for each target, and the
//! static validator.

use std::path::{Path, PathBuf};

use interlock_skillgen::validate::{self, Severity};
use interlock_skillgen::{Catalog, Invocation, Target, Value, generate, split_frontmatter};

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn errors(problems: &[validate::Problem]) -> Vec<String> {
    problems
        .iter()
        .filter(|p| p.severity == Severity::Error)
        .map(|p| format!("{}: {}", p.path.display(), p.message))
        .collect()
}

fn file<'a>(out: &'a interlock_skillgen::Output, path: &str) -> &'a str {
    out.files.get(Path::new(path)).unwrap_or_else(|| panic!("{path} not generated: {:?}", out.files.keys()))
}

#[test]
fn the_built_in_catalog_is_the_source_tree() {
    let embedded = Catalog::embedded().unwrap();
    let source = Catalog::from_dir(&source_root()).unwrap();
    assert_eq!(embedded.skills, source.skills, "rebuild after editing interlock/skills");
    assert_eq!(embedded.agents, source.agents);
    assert!(embedded.notice.as_deref().is_some_and(|n| n.contains("Lauren Tan") && n.contains("MIT")));
}

#[test]
fn the_v1_set_and_its_intents() {
    let catalog = Catalog::embedded().unwrap();
    let standalone: Vec<(&str, Invocation)> = catalog
        .skills
        .iter()
        .filter(|s| s.meta.invocation != Invocation::Routed)
        .map(|s| (s.meta.name.as_str(), s.meta.invocation))
        .collect();
    assert_eq!(
        standalone,
        vec![
            ("design", Invocation::User),
            ("implement", Invocation::Model),
            ("investigate", Invocation::Model),
            ("review", Invocation::Model),
            ("route", Invocation::Model),
            ("verify", Invocation::Model),
        ]
    );
    for s in catalog.skills.iter().filter(|s| s.meta.invocation != Invocation::Routed) {
        assert_eq!(s.meta.requires, vec!["interlock-cli"], "{}", s.meta.name);
        assert!(s.body.contains("interlock "), "{} never calls interlock", s.meta.name);
    }
    // Each step of the loop is driven by some skill.
    let all: String = catalog.skills.iter().map(|s| s.body.as_str()).collect();
    let agent = &catalog.agent("verifier").unwrap().body;
    for step in [
        "interlock task create",
        "interlock task ready",
        "interlock attempt start",
        "interlock check run",
        "interlock claim add",
        "interlock result submit",
        "--tree auto",
        "interlock advance",
        "interlock status",
        "interlock brief",
        "interlock task log",
    ] {
        assert!(all.contains(step), "no skill runs {step}");
    }
    for step in ["--role verifier", "interlock assess add", "interlock attempt end"] {
        assert!(agent.contains(step), "the verifier agent never runs {step}");
    }
}

#[test]
fn every_target_validates() {
    let catalog = Catalog::embedded().unwrap();
    for target in Target::ALL {
        let out = generate(&catalog, target).unwrap();
        assert_eq!(errors(&validate::check_output(&out)), Vec::<String>::new(), "{}", target.name());
        let mut names = out.skill_names();
        names.sort();
        let base = ["design", "implement", "investigate", "review", "route", "verify"];
        // On Copilot, interlock's skills share the repository's namespace, so they carry a prefix.
        let want: Vec<String> = match target {
            Target::Copilot => base.iter().map(|n| format!("interlock-{n}")).collect(),
            _ => base.iter().map(|n| n.to_string()).collect(),
        };
        assert_eq!(names, want, "{}", target.name());
    }
}

#[test]
fn copilot_gets_prefixed_github_skills_routed_references_and_no_custom_agent() {
    let out = generate(&Catalog::embedded().unwrap(), Target::Copilot).unwrap();
    let (fm, body) = split_frontmatter(file(&out, ".github/skills/interlock-route/SKILL.md")).unwrap().unwrap();
    let keys: Vec<&str> = fm.0.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["name", "description"], "model-invoked skills carry name and description only");
    assert_eq!(fm.str("name"), Some("interlock-route"), "the name matches the prefixed folder");
    assert!(body.contains("interlock task create"), "{body}");
    assert!(body.contains("interlock-investigate"), "skills name each other with the prefix");
    assert!(file(&out, ".github/skills/interlock-implement/SKILL.md").contains("--host copilot"));

    let (fm, _) = split_frontmatter(file(&out, ".github/skills/interlock-design/SKILL.md")).unwrap().unwrap();
    assert_eq!(fm.get("disable-model-invocation"), Some(&Value::Bool(true)), "S1: hidden from the agent");
    assert_eq!(fm.str("argument-hint"), Some("<task id>"));

    // Routed skills are reference files under their routers, never standalone.
    assert!(!out.files.contains_key(Path::new(".github/skills/interlock-prove-it-works/SKILL.md")));
    assert!(file(&out, ".github/skills/interlock-verify/references/prove-it-works.md").starts_with("# Prove it works"));
    assert!(out.files.contains_key(Path::new(".github/skills/interlock-route/references/interlock-basics.md")));

    // Copilot's hooks cannot name a subagent's type, so interlock launches the verifier itself.
    assert!(
        !out.files.keys().any(|p| p.starts_with(".github/agents")),
        "no custom agent: {:?}",
        out.files.keys().collect::<Vec<_>>()
    );
    let verify = file(&out, ".github/skills/interlock-verify/SKILL.md");
    assert!(verify.contains("Run `interlock verify <id> --host copilot`"), "{verify}");
    assert!(file(&out, ".github/skills/NOTICE").contains("pstack"));
}

#[test]
fn claude_code_gets_a_namespaced_plugin() {
    let out = generate(&Catalog::embedded().unwrap(), Target::ClaudeCode).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(file(&out, ".claude-plugin/plugin.json")).unwrap();
    assert_eq!(manifest["name"], "interlock");
    let verify = file(&out, "skills/verify/SKILL.md");
    assert!(verify.contains("Call the Agent tool with `subagent_type: \"interlock:verifier\"`"), "{verify}");
    assert!(verify.contains("Host: `claude-code`"), "the prompt it hands over is filled for the host: {verify}");
    assert!(
        file(&out, "skills/route/SKILL.md").contains("interlock:investigate"),
        "skills name each other with the namespace"
    );
    let (fm, _) = split_frontmatter(file(&out, "agents/verifier.md")).unwrap().unwrap();
    assert_eq!(fm.str("tools"), Some("Read, Grep, Glob, Bash"));
    let (fm, _) = split_frontmatter(file(&out, "skills/design/SKILL.md")).unwrap().unwrap();
    assert_eq!(fm.get("disable-model-invocation"), Some(&Value::Bool(true)));
}

#[test]
fn plain_agent_skills_keep_the_intent_in_metadata_and_validate_on_disk() {
    let out = generate(&Catalog::embedded().unwrap(), Target::AgentSkills).unwrap();
    let (fm, _) = split_frontmatter(file(&out, "design/SKILL.md")).unwrap().unwrap();
    assert_eq!(fm.get("disable-model-invocation"), None, "not an Agent Skills field");
    let Some(Value::Map(meta)) = fm.get("metadata") else { panic!("no metadata") };
    assert!(meta.contains(&("interlock-invocation".into(), "user".into())), "{meta:?}");
    assert!(meta.contains(&("interlock-requires".into(), "interlock-cli".into())), "{meta:?}");
    assert_eq!(fm.str("compatibility"), Some("Requires interlock-cli on PATH"));
    assert!(file(&out, "verify/SKILL.md").contains("`references/verifier-agent.md`"));
    assert!(file(&out, "verify/references/verifier-agent.md").contains("--host <host>"));

    let dir = tempfile::tempdir().unwrap();
    out.write(dir.path()).unwrap();
    let problems = validate::check_dir(dir.path(), Target::AgentSkills).unwrap();
    assert_eq!(errors(&problems), Vec::<String>::new());
    // The same files under Copilot's rules pass too; Copilot-only fields would fail the strict ones.
    let copilot = generate(&Catalog::embedded().unwrap(), Target::Copilot).unwrap();
    let cdir = tempfile::tempdir().unwrap();
    copilot.write(cdir.path()).unwrap();
    let strict = validate::check_dir(&cdir.path().join(".github/skills"), Target::AgentSkills).unwrap();
    let design = cdir.path().join(".github/skills/interlock-design/SKILL.md");
    let expected = ["disable-model-invocation", "argument-hint"]
        .map(|f| format!("{}: unknown field {f} for agent-skills", design.display()));
    assert_eq!(errors(&strict), expected);
}

fn files(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![
        ("skills/route/skill.toml".into(), "name = \"route\"\ndescription = \"Routes.\"\ninvocation = \"model\"\npack = \"core\"\n".into()),
        ("skills/route/SKILL.md".into(), "Route. See references/tip.md.\n".into()),
        ("skills/tip/skill.toml".into(), "name = \"tip\"\ndescription = \"A tip.\"\ninvocation = \"routed\"\npack = \"core\"\nrouters = [\"route\"]\n".into()),
        ("skills/tip/SKILL.md".into(), "# Tip\n".into()),
    ];
    for (p, t) in extra {
        v.retain(|(path, _)| path != p);
        v.push((p.to_string(), t.to_string()));
    }
    v
}

fn load_err(extra: &[(&str, &str)]) -> String {
    Catalog::from_files(files(extra)).expect_err("should be refused").to_string()
}

#[test]
fn a_minimal_catalog_loads() {
    let c = Catalog::from_files(files(&[])).unwrap();
    let out = generate(&c, Target::AgentSkills).unwrap();
    assert_eq!(file(&out, "route/references/tip.md"), "# Tip\n");
}

#[test]
fn names_are_lowercase_hyphenated_and_match_their_folder() {
    let toml = "name = \"Poteto Mode\"\ndescription = \"x\"\ninvocation = \"model\"\npack = \"core\"\n";
    let e = load_err(&[("skills/Poteto Mode/skill.toml", toml), ("skills/Poteto Mode/SKILL.md", "x\n")]);
    assert!(e.contains("lowercase"), "{e}");
    let toml = "name = \"make-bot-ui\"\ndescription = \"x\"\ninvocation = \"model\"\npack = \"core\"\n";
    let e = load_err(&[("skills/make-bot/skill.toml", toml), ("skills/make-bot/SKILL.md", "x\n")]);
    assert!(e.contains("must match its folder"), "{e}");
}

#[test]
fn canonical_sources_are_checked() {
    let e = load_err(&[("skills/route/SKILL.md", "---\nname: route\n---\nbody\n")]);
    assert!(e.contains("no frontmatter"), "{e}");
    let e = load_err(&[("skills/route/SKILL.md", "Route without the reference.\n")]);
    assert!(e.contains("never mentions references/tip.md"), "{e}");
    let e = load_err(&[("skills/route/SKILL.md", "Route. See references/tip.md and {{skill:tip}}.\n")]);
    assert!(e.contains("tip is routed"), "{e}");
    let e = load_err(&[("skills/route/SKILL.md", "Route. See references/tip.md and {{nope}}.\n")]);
    assert!(e.contains("unknown template"), "{e}");
    let bad = "name = \"tip\"\ndescription = \"A tip.\"\ninvocation = \"sometimes\"\npack = \"core\"\n";
    let e = load_err(&[("skills/tip/skill.toml", bad)]);
    assert!(e.contains("skills/tip/skill.toml"), "{e}");
    let long =
        format!("name = \"route\"\ndescription = \"{}\"\ninvocation = \"model\"\npack = \"core\"\n", "d".repeat(1100));
    assert!(load_err(&[("skills/route/skill.toml", &long)]).contains("1024"));
    let e = load_err(&[("skills/route/notes.txt", "x")]);
    assert!(e.contains("only references/*.md"), "{e}");
}

/// The skills a person reaches with an ordinary request ("fix this bug",
/// "add this feature", "why does X happen") must trigger on it and work
/// where there is no interlock task, so that skills can be used, and
/// evaluated, without the runtime.
#[test]
fn the_entry_skills_trigger_on_ordinary_requests_and_work_without_interlock() {
    let catalog = Catalog::embedded().unwrap();
    let standalone: Vec<&str> =
        catalog.skills.iter().filter(|s| s.meta.standalone).map(|s| s.meta.name.as_str()).collect();
    assert_eq!(standalone, ["implement", "investigate"]);
    let implement = catalog.skill("implement").unwrap();
    for request in [
        "whenever you are asked to fix a bug",
        "add or change a feature",
        "refactor code",
        "before your first edit",
        "Works on its own",
    ] {
        assert!(implement.meta.description.contains(request), "{}", implement.meta.description);
    }
    let investigate = catalog.skill("investigate").unwrap();
    for request in ["whenever you are asked how something works", "why it was built that way", "Works on its own"] {
        assert!(investigate.meta.description.contains(request), "{}", investigate.meta.description);
    }
    for s in [implement, investigate] {
        assert!(!s.meta.description.contains("an interlock task's"), "{}", s.meta.description);
        let (with, without) = s.body.split_once("\n## Without interlock\n").expect("a standalone section");
        // The skill says when interlock applies: a task id, a headless attempt, or a store that
        // `interlock where` finds the way every command does (INTERLOCK_DB first). Not a path test:
        // a store can live elsewhere, and `skills generate` writes `.interlock/` with no store.
        for when in ["task id", "`INTERLOCK_ATTEMPT` is set", "`interlock where` exits 0", "`INTERLOCK_DB`"] {
            assert!(with.contains(when), "{}: {when}", s.meta.name);
        }
        // Without it, nothing is recorded: no interlock command at all.
        assert!(!without.contains("interlock "), "{}: {without}", s.meta.name);
        assert!(without.contains("nothing is recorded"), "{}", s.meta.name);
    }
    // Skills whose job exists only with interlock's records stay tied to it.
    let route = catalog.skill("route").unwrap();
    assert!(!route.meta.standalone && route.meta.description.contains("set up for interlock"));
    assert!(route.meta.description.contains("interlock where") && !route.meta.description.contains("state.db"));
    for name in ["verify", "review", "design"] {
        assert!(!catalog.skill(name).unwrap().meta.standalone, "{name}");
    }

    let out = generate(&catalog, Target::AgentSkills).unwrap();
    let (fm, _) = split_frontmatter(file(&out, "implement/SKILL.md")).unwrap().unwrap();
    assert_eq!(
        fm.str("compatibility"),
        Some("Uses interlock-cli on PATH where the work is an interlock task; works without it")
    );
    let Some(Value::Map(meta)) = fm.get("metadata") else { panic!("no metadata") };
    assert!(meta.contains(&("interlock-standalone".into(), "true".into())), "{meta:?}");
    // On the hosts, model-invoked skills still carry name and description only.
    let out = generate(&catalog, Target::ClaudeCode).unwrap();
    let (fm, body) = split_frontmatter(file(&out, "skills/implement/SKILL.md")).unwrap().unwrap();
    let keys: Vec<&str> = fm.0.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["name", "description"]);
    assert!(body.contains("## Without interlock") && body.contains("interlock:route"), "{body}");
}

#[test]
fn a_standalone_skill_must_say_how_it_works_without_interlock() {
    let standalone = "name = \"route\"\ndescription = \"Routes.\"\ninvocation = \"model\"\n\
                      requires = [\"interlock-cli\"]\nstandalone = true\npack = \"core\"\n";
    let e = load_err(&[("skills/route/skill.toml", standalone)]);
    assert!(e.contains("## Without interlock"), "{e}");
    let body = "Route. See references/tip.md.\n\n## Without interlock\n\nJust do it.\n";
    Catalog::from_files(files(&[("skills/route/skill.toml", standalone), ("skills/route/SKILL.md", body)])).unwrap();
    let no_requirement = "name = \"route\"\ndescription = \"Routes.\"\ninvocation = \"model\"\nstandalone = true\n\
                          pack = \"core\"\n";
    let e = load_err(&[("skills/route/skill.toml", no_requirement), ("skills/route/SKILL.md", body)]);
    assert!(e.contains("it has none"), "{e}");
    let routed = "name = \"tip\"\ndescription = \"A tip.\"\ninvocation = \"routed\"\nrequires = [\"interlock-cli\"]\n\
                  standalone = true\npack = \"core\"\nrouters = [\"route\"]\n";
    let e = load_err(&[("skills/tip/skill.toml", routed)]);
    assert!(e.contains("only a model-invoked skill"), "{e}");
}
