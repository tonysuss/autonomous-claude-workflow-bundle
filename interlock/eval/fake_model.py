"""A scripted OpenAI-compatible model for Copilot CLI's offline mode.

A port of crates/interlock-cli/tests/fake_model to the standard library, so
the harness can drive `copilot -p` and `interlock run --host copilot` end to
end with no model calls. It validates plumbing only: every command comes from
a script, so nothing it produces is evaluation evidence.

A script names, per kind of session, the shell commands to issue in order and
the final message:

    {"plain": {"steps": [...], "final": "..."},
     "worker": {"steps": [...], "final": "..."},
     "verifier": {"steps": [...], "final": "..."},
     "on_block": [...]}

A session is a verifier if its first user message mentions "independent
verifier", an interlock worker if it mentions "interlock", and a plain session
otherwise. (Copilot puts interlock's role instructions in that message.)

A kind may also name a "skill". When the host lists that skill for the model
(Copilot's `<available_skills>`) and offers its skill tool, the session first
invokes it, then runs its steps: the skills condition's plumbing.
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def _text(m):
    c = m.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return "\n".join(p.get("text", "") for p in c if isinstance(p, dict))
    return ""


class FakeModel:
    def __init__(self, script):
        self.script = script
        self.requests = []
        self.lock = threading.Lock()
        model = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):
                body = json.dumps({"object": "list", "data": [{"id": "gpt-4.1", "object": "model"}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                n = int(self.headers.get("Content-Length") or 0)
                try:
                    req = json.loads(self.rfile.read(n) or b"null")
                except ValueError:
                    req = {}
                message = model.respond(req or {})
                delta = dict(message)
                for i, call in enumerate(delta.get("tool_calls") or []):
                    call["index"] = i
                finish = "tool_calls" if message.get("tool_calls") else "stop"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                chunks = [
                    {"id": "x", "object": "chat.completion.chunk", "model": "gpt-4.1",
                     "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                    {"id": "x", "object": "chat.completion.chunk", "model": "gpt-4.1",
                     "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                     "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}},
                ]
                for c in chunks:
                    self.wfile.write(f"data: {json.dumps(c)}\n\n".encode())
                self.wfile.write(b"data: [DONE]\n\n")

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.port}/v1"

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    def respond(self, req):
        msgs = req.get("messages") or []
        # The session's prompt decides its kind. Other context (a loaded
        # skills plugin, for one) can mention interlock and verifiers too.
        prompt = next((_text(m) for m in msgs if m.get("role") == "user"), "")
        if "independent verifier" in prompt:
            kind = "verifier"
        elif "interlock" in prompt:
            kind = "worker"
        else:
            kind = "plain"
        tools_done = sum(1 for m in msgs if m.get("role") == "tool")
        blocked = sum(1 for m in msgs if m.get("role") == "user" and "Before you finish" in _text(m))
        with self.lock:
            self.requests.append({"kind": kind, "tools_done": tools_done})
        spec = self.script.get(kind) or {}
        steps = list(spec.get("steps") or [])
        tools = {t.get("function", {}).get("name"): t for t in req.get("tools") or []}
        system = "\n".join(_text(m) for m in msgs if m.get("role") == "system")
        skill = spec.get("skill")
        if skill and "skill" in tools and f"<name>{skill}</name>" in system:
            steps.insert(0, {"tool": "skill", "arguments": {"skill": skill}})
        on_block = self.script.get("on_block") or []
        command = None
        if tools_done < len(steps):
            command = steps[tools_done]
        elif blocked and tools_done - len(steps) < len(on_block):
            command = on_block[tools_done - len(steps)]
        if isinstance(command, dict):
            with self.lock:
                self.requests[-1]["tool"] = command["tool"]
            return {
                "role": "assistant",
                "content": None,
                "tool_calls": [{
                    "id": f"call_{tools_done}",
                    "type": "function",
                    "function": {"name": command["tool"], "arguments": json.dumps(command["arguments"])},
                }],
            }
        shell = tools.get("bash") or tools.get("shell")
        if command and shell:
            return {
                "role": "assistant",
                "content": None,
                "tool_calls": [{
                    "id": f"call_{tools_done}",
                    "type": "function",
                    "function": {"name": shell["function"]["name"], "arguments": json.dumps(_fill(shell, command))},
                }],
            }
        return {"role": "assistant", "content": spec.get("final") or f"{kind} finished after {tools_done} commands."}


def _fill(tool, command):
    params = tool.get("function", {}).get("parameters") or {}
    args = {"command": command}
    for name in params.get("required") or []:
        p = (params.get("properties") or {}).get(name) or {}
        if name == "command":
            args[name] = command
        elif isinstance(p.get("enum"), list):
            args[name] = p["enum"][0]
        elif p.get("type") in ("integer", "number"):
            args[name] = 60
        elif p.get("type") == "boolean":
            args[name] = False
        else:
            args[name] = "scripted step"
    return args
