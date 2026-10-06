"""Trims a JSONL transcript for the evidence folder: long strings are cut, attempt tokens and
e-mail addresses are redacted, and the host's bulky startup inventory is summarized.

usage: trim.py IN OUT [max_string]
"""
import json, re, sys

src, dst = sys.argv[1], sys.argv[2]
limit = int(sys.argv[3]) if len(sys.argv) > 3 else 3000
TOKEN = re.compile(r'(--token[ =]|token\\?"\s*:\s*\\?"|INTERLOCK_TOKEN=)([0-9a-f]{32,})')
EMAIL = re.compile(r'[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+\.[A-Za-z0-9.-]+')
# Streaming fragments and bookkeeping events that repeat what the kept events say.
DROP = {"stream_event", "rate_limit_event", "session.background_tasks_changed", "tool.execution_partial_result",
        "assistant.tool_call_delta", "assistant.message_delta", "model.call_start", "model.call_finished",
        "model.call_final_result", "assistant.turn_start"}
KEEP_EMAILS = {"sam@example.com", "ana@example.com", "t@t", "noreply@anthropic.com", "interlock@localhost"}


def clean(s):
    s = TOKEN.sub(lambda m: m.group(1) + "<redacted>", s)
    s = EMAIL.sub(lambda m: m.group(0) if m.group(0) in KEEP_EMAILS else "<email>", s)
    if len(s) > limit:
        s = s[:limit] + f"... [{len(s) - limit} more characters trimmed]"
    return s


def walk(v):
    if isinstance(v, str):
        return clean(v)
    if isinstance(v, list):
        return [walk(x) for x in v]
    if isinstance(v, dict):
        return {k: walk(x) for k, x in v.items()}
    return v


out = []
with open(src) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            e = json.loads(line)
        except ValueError:
            out.append(json.dumps({"unparsed": clean(line)}))
            continue
        if isinstance(e, dict) and e.get("type") == "system" and e.get("subtype") == "init":
            e = {k: e.get(k) for k in ("type", "subtype", "cwd", "model", "permissionMode", "skills", "agents",
                                       "plugins", "claude_code_version")}
            e["tools_note"] = "the tool inventory and startup timings are left out"
        if isinstance(e, dict) and e.get("type") in DROP:
            continue
        out.append(json.dumps(walk(e)))
with open(dst, "w") as f:
    f.write("\n".join(out) + "\n")
print(dst, sum(len(l) + 1 for l in out), "bytes")
