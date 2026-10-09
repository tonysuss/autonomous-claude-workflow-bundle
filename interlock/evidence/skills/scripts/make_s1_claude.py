"""Writes the S1 probe skills and agent for Claude Code: project layout and plugin layout."""
import json, os

ROOT = os.path.dirname(os.path.abspath(__file__))
SKILLS = {
    "s1-model": ("S1 probe, plain model-invoked skill. Use when asked for the s1 model probe.", "", "MARKER-MODEL"),
    "s1-user": ("S1 probe, a skill marked disable-model-invocation. Use when asked for the s1 user probe.",
                "disable-model-invocation: true\n", "MARKER-USER"),
    "s1-router": ("S1 probe, a router skill with routed references. Use when asked for the s1 router probe.", "",
                  "MARKER-ROUTER. For the routed material, read references/routed.md next to this file."),
}
AGENT = ("---\nname: s1-agent\ndescription: S1 probe custom agent with read and shell tools only.\n"
         "tools: Read, Grep, Glob, Bash\n---\n\nMARKER-AGENT: you are the s1 probe agent.\n")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


for base, skills_dir, agents_dir in [
    (os.path.join(ROOT, "s1repo"), ".claude/skills", ".claude/agents"),
    (os.path.join(ROOT, "s1plugin"), "skills", "agents"),
]:
    for name, (desc, extra, body) in SKILLS.items():
        write(os.path.join(base, skills_dir, name, "SKILL.md"),
              f"---\nname: {name}\ndescription: {desc}\n{extra}---\n\n{body}\n")
    write(os.path.join(base, skills_dir, "s1-router", "references", "routed.md"), "MARKER-ROUTED: routed reference content.\n")
    write(os.path.join(base, agents_dir, "s1-agent.md"), AGENT)
write(os.path.join(ROOT, "s1plugin", ".claude-plugin", "plugin.json"),
      json.dumps({"name": "s1probe", "version": "0.0.1", "description": "S1 probe plugin"}))
print("ok")
