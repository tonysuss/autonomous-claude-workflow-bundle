"""A scripted OpenAI-compatible model for the S1 spike: any tool, not just the shell.

Script file (FAKE_SCRIPT_FILE) is JSON:
  {"sessions": [{"match": "substring", "steps": [{"tool": "skill", "args": {...}}, ...], "final": "text"}]}
A request belongs to the first session whose `match` appears in its system or first user message.
The step to issue is the number of tool results already in the conversation.
Every request is logged to FAKE_LOG as one JSON line.
"""
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("FAKE_LOG", "/dev/null")
SCRIPT = json.load(open(os.environ["FAKE_SCRIPT_FILE"]))


def text_of(m):
    c = m.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return "\n".join(p.get("text", "") for p in c if isinstance(p, dict))
    return ""


def fill(tool, args):
    params = (tool.get("function", {}).get("parameters") or {})
    props = params.get("properties", {}) or {}
    out = dict(args)
    for name in params.get("required", []) or []:
        if name in out:
            continue
        p = props.get(name, {})
        if "enum" in p:
            out[name] = p["enum"][0]
        elif p.get("type") in ("integer", "number"):
            out[name] = 60
        elif p.get("type") == "boolean":
            out[name] = False
        else:
            out[name] = "spike step"
    return out


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _json(self, obj):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._json({"object": "list", "data": [{"id": "gpt-4.1", "object": "model"}]})

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        msgs = req.get("messages", [])
        tools = {t.get("function", {}).get("name"): t for t in req.get("tools", [])}
        head = "\n".join(text_of(m) for m in msgs if m.get("role") == "system")
        first_user = next((text_of(m) for m in msgs if m.get("role") == "user"), "")
        session = next((s for s in SCRIPT["sessions"] if s["match"] in head + "\n" + first_user), None)
        done = sum(1 for m in msgs if m.get("role") == "tool")
        steps = session["steps"] if session else []
        entry = {
            "session": session["match"] if session else None,
            "tools_done": done,
            "tool_names": sorted(tools),
            "first_user": first_user[:2000],
            "last": msgs[-1] if msgs else None,
            "system_len": len(head),
        }
        if os.environ.get("FAKE_LOG_SYSTEM"):
            entry["system"] = head
        if os.environ.get("FAKE_LOG_SCHEMAS"):
            entry["schemas"] = {n: tools[n] for n in tools if n in ("skill", "task", "bash", "view", "edit", "create")}
        with open(LOG, "a") as f:
            f.write(json.dumps(entry) + "\n")
        if done < len(steps) and steps[done]["tool"] in tools:
            step = steps[done]
            tool = tools[step["tool"]]
            call = {"id": f"call_{done}", "type": "function",
                    "function": {"name": step["tool"], "arguments": json.dumps(fill(tool, step.get("args", {})))}}
            message, finish = {"role": "assistant", "content": None, "tool_calls": [call]}, "tool_calls"
        else:
            missing = steps[done]["tool"] if done < len(steps) else None
            final = (session or {}).get("final", "done")
            if missing:
                final += f" (tool {missing} was not offered)"
            message, finish = {"role": "assistant", "content": final}, "stop"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        delta = dict(message)
        if "tool_calls" in delta:
            delta["tool_calls"] = [dict(c, index=i) for i, c in enumerate(delta["tool_calls"])]
        for chunk in ({"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                      {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                       "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}):
            chunk.update({"id": "x", "object": "chat.completion.chunk", "created": int(time.time()), "model": "gpt-4.1"})
            self.wfile.write(b"data: " + json.dumps(chunk).encode() + b"\n\n")
        self.wfile.write(b"data: [DONE]\n\n")


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
