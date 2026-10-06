"""Writes plugin-c: one agent per candidate `tools` list, to see what each offers."""
import json
import os

S = "<scratch>/agentspike"
P = f"{S}/plugin-c"
os.makedirs(f"{P}/.claude-plugin", exist_ok=True)
os.makedirs(f"{P}/agents", exist_ok=True)
json.dump({"name": "interlock-hooks", "version": "0.1.0", "description": "alias probes"},
          open(f"{P}/.claude-plugin/plugin.json", "w"))
PROBES = {
    "names": ["view", "grep", "glob", "bash", "create", "edit"],
    "search": ["search"],
    "shell": ["shell"],
    "agent": ["agent", "read"],
    "web": ["web", "read"],
    "customagent": ["custom-agent"],
    "todo": ["todo"],
    "empty": [],
    "mcp": ["github-mcp-server/*"],
}
for name, tools in PROBES.items():
    with open(f"{P}/agents/t-{name}.agent.md", "w") as f:
        f.write(f"---\nname: t-{name}\ndescription: tools probe {name}.\ntools: {json.dumps(tools)}\n---\n\nMARKER-T-{name}\n")
print(sorted(os.listdir(f"{P}/agents")))
