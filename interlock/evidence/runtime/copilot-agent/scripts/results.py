"""Per run: tools offered, each tool call's outcome, hook payloads, selected agent."""
import glob
import json
import sys

S = "<scratch>/agentspike/runs"
for run in sys.argv[1:]:
    d = f"{S}/{run}"
    print(f"== {run}:", open(f"{d}/exit.txt").read().strip())
    try:
        first = json.loads(open(f"{d}/model.jsonl").readline())
        print("  tools offered:", first["tool_names"])
        sysp = first.get("system", "")
        i = sysp.find("<agent_instructions>")
        print("  agent_instructions:", sysp[i:i + 300].replace("\n", " ") if i >= 0 else None)
    except (FileNotFoundError, ValueError):
        print("  no model requests")
    events = [json.loads(l) for l in open(f"{d}/events.jsonl") if l.startswith("{")]
    starts = {e["data"]["toolCallId"]: e["data"] for e in events if e["type"] == "tool.execution_start"}
    for e in events:
        if e["type"] == "tool.execution_complete":
            dd = e["data"]
            st = starts.get(dd["toolCallId"], {})
            what = (st.get("toolName"), json.dumps(st.get("arguments"))[:80])
            res = dd.get("result", {}).get("content") if dd["success"] else dd.get("error")
            print("  call", what, "success" if dd["success"] else "FAILED", str(res)[:160].replace("\n", " "))
    finals = [e["data"]["content"] for e in events if e["type"] == "assistant.message" and e["data"].get("content")]
    print("  final:", finals[-1:] if finals else None)
    try:
        for l in open(f"{d}/hook.jsonl"):
            p = json.loads(l)
            print("  hook:", p.get("hook_event_name"), p.get("tool_name"), json.dumps(p.get("tool_input"))[:80],
                  {k: v for k, v in p.items() if "agent" in k.lower()})
    except FileNotFoundError:
        print("  no hook log")
    for f in glob.glob(f"{d}/home/session-state/*/events.jsonl"):
        for l in open(f):
            e = json.loads(l)
            if e["type"] == "subagent.selected":
                print("  session-state:", json.dumps(e["data"]))
