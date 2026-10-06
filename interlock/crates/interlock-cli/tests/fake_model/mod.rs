//! A scripted OpenAI-compatible model. Copilot CLI runs against it offline
//! (`COPILOT_OFFLINE`, `COPILOT_PROVIDER_BASE_URL`), so whole interlock runs
//! can be tested on the real host with no model calls.
//!
//! Each role has one script per session: a list of shell commands issued in
//! order, then a final message. When a stop hook holds the agent, the next
//! command comes from `on_block`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Clone, Default)]
pub struct Script {
    pub worker: Vec<Vec<String>>,
    pub verifier: Vec<Vec<String>>,
    pub on_block: Vec<String>,
}

#[derive(Default)]
struct State {
    /// First user message of each session seen, per role, in order.
    sessions: std::collections::HashMap<&'static str, Vec<String>>,
    requests: Vec<Value>,
}

pub struct FakeModel {
    pub port: u16,
    state: Arc<Mutex<State>>,
}

impl FakeModel {
    pub fn start(script: Script) -> FakeModel {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (script, state) = (script.clone(), shared.clone());
                std::thread::spawn(move || serve(stream, &script, &state));
            }
        });
        FakeModel { port, state }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().requests.clone()
    }
}

fn serve(mut stream: TcpStream, script: &Script, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    if request_line.starts_with("GET") {
        let models = json!({"object": "list", "data": [{"id": "gpt-4.1", "object": "model"}]}).to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{models}",
            models.len()
        );
        return;
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let message = respond(&req, script, state);
    let mut delta = message.clone();
    if let Some(calls) = delta.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for (i, c) in calls.iter_mut().enumerate() {
            c["index"] = json!(i);
        }
    }
    let finish = if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
    for chunk in [
        json!({"id": "x", "object": "chat.completion.chunk", "model": "gpt-4.1",
               "choices": [{"index": 0, "delta": delta, "finish_reason": null}]}),
        json!({"id": "x", "object": "chat.completion.chunk", "model": "gpt-4.1",
               "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
               "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
    ] {
        let _ = write!(stream, "data: {chunk}\n\n");
    }
    let _ = write!(stream, "data: [DONE]\n\n");
}

fn text_of(m: &Value) -> String {
    match &m["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn respond(req: &Value, script: &Script, state: &Mutex<State>) -> Value {
    let msgs = req["messages"].as_array().cloned().unwrap_or_default();
    let all_text: String = msgs.iter().map(text_of).collect::<Vec<_>>().join("\n");
    // The verifier's role instruction leads its prompt; a worker's brief may mention verifiers too.
    let role = if all_text.contains("You are the independent verifier") { "verifier" } else { "worker" };
    let tools_done = msgs.iter().filter(|m| m["role"] == "tool").count();
    let blocked = msgs.iter().filter(|m| m["role"] == "user" && text_of(m).contains("Before you finish")).count();

    let mut st = state.lock().unwrap();
    st.requests.push(json!({"role": role, "tools_done": tools_done, "last": msgs.last()}));
    // Every request replays the session's first user message, so it names the session.
    let key = msgs.iter().find(|m| m["role"] == "user").map(text_of).unwrap_or_default();
    let seen = st.sessions.entry(role).or_default();
    let session_index = match seen.iter().position(|k| *k == key) {
        Some(i) => i,
        None => {
            seen.push(key);
            seen.len() - 1
        }
    };
    drop(st);

    let scripts = if role == "verifier" { &script.verifier } else { &script.worker };
    let steps = scripts.get(session_index).or_else(|| scripts.last()).cloned().unwrap_or_default();
    let command = if tools_done < steps.len() {
        Some(steps[tools_done].clone())
    } else if blocked > 0 {
        script.on_block.get(tools_done - steps.len()).cloned()
    } else {
        None
    };
    let shell = req["tools"]
        .as_array()
        .and_then(|ts| ts.iter().find(|t| matches!(t["function"]["name"].as_str(), Some("bash" | "shell"))))
        .cloned();
    match (command, shell) {
        (Some(cmd), Some(tool)) => {
            let args = fill_args(&tool, &cmd);
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": format!("call_{tools_done}"), "type": "function",
                "function": {"name": tool["function"]["name"], "arguments": args.to_string()}}]})
        }
        _ => json!({"role": "assistant", "content": format!("{role} finished after {tools_done} commands.")}),
    }
}

fn fill_args(tool: &Value, command: &str) -> Value {
    let params = &tool["function"]["parameters"];
    let mut args = json!({"command": command});
    for name in params["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let p = &params["properties"][name];
        args[name] = match name {
            "command" => json!(command),
            _ if p["enum"].is_array() => p["enum"][0].clone(),
            _ if p["type"] == "integer" || p["type"] == "number" => json!(60),
            _ if p["type"] == "boolean" => json!(false),
            _ => json!("interlock test step"),
        };
    }
    args
}
