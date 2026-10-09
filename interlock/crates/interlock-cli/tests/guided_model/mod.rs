//! A scripted OpenAI-compatible model for guided sessions on Copilot CLI
//! (offline mode). Unlike the headless fake model, it can call any tool the
//! host offers (`skill`, `task`, `edit`, `bash`), and it keeps one script per
//! conversation, so a main session and the custom agent it delegates to each
//! follow their own.
//!
//! A step's arguments may use placeholders that the model fills from the
//! JSON of the latest `interlock attempt start` in the same conversation, as
//! an agent would read it: `{attempt}`, `{token}`, `{epoch}`, `{worktree}`;
//! and `{model_url}`, this model's own address.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Step {
    pub tool: String,
    pub args: Value,
}

/// A shell command. It waits up to two minutes for output, as an agent would
/// for a command it expects to take a while.
pub fn bash(command: &str) -> Step {
    Step {
        tool: "bash".into(),
        args: json!({ "command": command, "description": "interlock step", "initial_wait": 120 }),
    }
}

pub fn tool(name: &str, args: Value) -> Step {
    Step { tool: name.into(), args }
}

/// A conversation's script, chosen by text in its system or first user message.
#[derive(Clone, Debug)]
pub struct Conversation {
    pub marker: String,
    pub steps: Vec<Step>,
    pub reply: String,
}

#[derive(Default)]
struct State {
    log: Vec<Value>,
    port: u16,
}

pub struct GuidedModel {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl GuidedModel {
    pub fn start(conversations: Vec<Conversation>) -> GuidedModel {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State { port, ..State::default() }));
        let shared = state.clone();
        let conversations = Arc::new(conversations);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (c, s) = (conversations.clone(), shared.clone());
                std::thread::spawn(move || serve(stream, &c, &s));
            }
        });
        GuidedModel { port, state }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// One entry per request: the conversation it matched, how many tool
    /// results it carried, the last message, and the tool names offered.
    pub fn log(&self) -> Vec<Value> {
        self.state.lock().unwrap().log.clone()
    }
}

fn text_of(m: &Value) -> String {
    match &m["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The values of the latest `attempt start` output among the tool results.
fn attempt_values(msgs: &[Value]) -> Vec<(&'static str, String)> {
    for m in msgs.iter().rev().filter(|m| m["role"] == "tool") {
        let text = text_of(m);
        let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text[start..=end]) else { continue };
        if v["started"] != "yes" {
            continue;
        }
        let a = &v["attempt"];
        return vec![
            ("{attempt}", a["id"].as_str().unwrap_or_default().to_string()),
            ("{token}", v["token"].as_str().unwrap_or_default().to_string()),
            ("{epoch}", a["epoch"].to_string()),
            ("{worktree}", a["worktree"].as_str().unwrap_or_default().to_string()),
        ];
    }
    vec![]
}

fn fill(v: &Value, values: &[(&str, String)]) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in values {
                s = s.replace(k, val);
            }
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(x, values)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), fill(x, values))).collect()),
        other => other.clone(),
    }
}

fn respond(req: &Value, conversations: &[Conversation], state: &Mutex<State>) -> Value {
    let msgs = req["messages"].as_array().cloned().unwrap_or_default();
    let system: String = msgs.iter().filter(|m| m["role"] == "system").map(text_of).collect::<Vec<_>>().join("\n");
    let first_user = msgs.iter().find(|m| m["role"] == "user").map(text_of).unwrap_or_default();
    let conversation = conversations.iter().find(|c| system.contains(&c.marker) || first_user.contains(&c.marker));
    let done = msgs.iter().filter(|m| m["role"] == "tool").count();
    let offered: Vec<String> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str().map(String::from))
        .collect();
    // Everything the host added since the model's last turn: tool results, and any skill text it injected.
    let since = msgs.iter().rposition(|m| m["role"] == "assistant").map_or(0, |i| i + 1);
    let new_text: Vec<String> = msgs[since..].iter().filter(|m| m["role"] != "system").map(text_of).collect();
    // The skills the host lists for the model, with their descriptions (Copilot puts them in the system
    // prompt), logged on a conversation's first request only.
    let available_skills = (done == 0)
        .then(|| system.split("<available_skills>").nth(1).and_then(|s| s.split("</available_skills>").next()))
        .flatten();
    state.lock().unwrap().log.push(json!({
        "conversation": conversation.map(|c| c.marker.clone()),
        "tools_done": done,
        "last": msgs.last(),
        "new_text": new_text.join("\n"),
        "offered": offered,
        "available_skills": available_skills,
    }));
    let Some(conversation) = conversation else {
        return json!({"role": "assistant", "content": "No script matches this conversation."});
    };
    match conversation.steps.get(done) {
        Some(step) if offered.contains(&step.tool) => {
            let mut values = attempt_values(&msgs);
            values.push(("{model_url}", format!("http://127.0.0.1:{}/v1", state.lock().unwrap().port)));
            let mut args = fill(&step.args, &values);
            let schema = req["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|t| t["function"]["name"] == step.tool.as_str())
                .map(|t| t["function"]["parameters"].clone())
                .unwrap_or_default();
            for name in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if args.get(name).is_none() {
                    let p = &schema["properties"][name];
                    args[name] = match p["type"].as_str() {
                        _ if p["enum"].is_array() => p["enum"][0].clone(),
                        Some("integer" | "number") => json!(60),
                        Some("boolean") => json!(false),
                        _ => json!("interlock step"),
                    };
                }
            }
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": format!("call_{}_{done}", conversation.marker), "type": "function",
                "function": {"name": step.tool, "arguments": args.to_string()}}]})
        }
        Some(step) => {
            json!({"role": "assistant", "content": format!("The host did not offer the {} tool.", step.tool)})
        }
        None => json!({"role": "assistant", "content": conversation.reply}),
    }
}

fn serve(mut stream: TcpStream, conversations: &[Conversation], state: &Mutex<State>) {
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
    let message = respond(&req, conversations, state);
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
