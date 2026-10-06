"""Collects the agent spike's results into one text file for the evidence folder."""
import glob
import json
import subprocess
import sys

S = "<scratch>/agentspike"
out = []


def add(line=""):
    out.append(line)


add("Copilot CLI 1.0.91, offline (COPILOT_OFFLINE=true) against scripts/spike_model.py, October 6, 2026.")
add("Each run: copilot -p 'AGENT-PROBE run the probe' --output-format json --no-ask-user --no-auto-update")
add("  --plugin-dir <plugin> [--agent <name>] [tool flags], with a private COPILOT_HOME.")
add()
add("== a1: --plugin-dir plugin-a --agent interlock-worker --allow-tool=shell")
add("exit: " + open(f"{S}/runs/a1/exit.txt").read().strip())
add("stderr: " + open(f"{S}/runs/a1/stderr.txt").read().strip())
add()
add("== a2: --plugin-dir plugin-a --agent interlock-hooks:interlock-worker --allow-tool=shell")
first = json.loads(open(f"{S}/runs/a2/model.jsonl").readline())
sysp = first["system"]
i = sysp.find("<agent_instructions>")
add("exit: " + open(f"{S}/runs/a2/exit.txt").read().strip())
add("tools offered: " + json.dumps(first["tool_names"]))
add("system prompt length: %d; it begins: %r" % (len(sysp), sysp[:120]))
add("agent block, at offset %d: %r" % (i, sysp[i:sysp.find("</agent_instructions>") + 21]))
add()
add("== a0, a3, a4: plugin-b (its PreToolUse hook denies any command containing DENY-ME),")
add("   --allow-tool=shell --allow-tool=write '--deny-tool=shell(git push:*)'; the model calls bash 'echo hello-from-agent',")
add("   bash 'git push origin main', bash 'echo DENY-ME', create b.txt")
add("   a0: no --agent; a3: --agent interlock-hooks:interlock-worker (tools read, search, edit, execute);")
add("   a4: --agent interlock-hooks:interlock-open (no tools line)")
r = subprocess.run([sys.executable, "-I", f"{S}/scripts/results.py", "a0", "a3", "a4"], capture_output=True, text=True)
add(r.stdout.rstrip())
add()
add("== aliases: one agent per tools list, in plugin-c, with --allow-all-tools; tools offered on the first request")
r = subprocess.run(["sh", f"{S}/scripts/run_aliases.sh"], capture_output=True, text=True) if "--rerun" in sys.argv else None
for n, tools in [("names", ["view", "grep", "glob", "bash", "create", "edit"]), ("search", ["search"]), ("shell", ["shell"]),
                 ("agent", ["agent", "read"]), ("web", ["web", "read"]), ("customagent", ["custom-agent"]),
                 ("todo", ["todo"]), ("empty", []), ("mcp", ["github-mcp-server/*"])]:
    try:
        offered = json.loads(open(f"{S}/runs/t-{n}/model.jsonl").readline())["tool_names"]
    except (OSError, ValueError):
        offered = "no request"
    add(f"tools: {json.dumps(tools)} -> offered {json.dumps(offered)}")
text = "\n".join(out) + "\n"
text = text.replace(S, "<scratch>/agentspike")
open(sys.argv[1], "w").write(text)
print(text)
