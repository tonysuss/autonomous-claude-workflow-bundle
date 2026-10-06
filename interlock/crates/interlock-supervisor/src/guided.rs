//! Guidance mode: a person drives a host session and the generated skills call
//! interlock at each step. The hooks learn who is calling from the host's own
//! payload (its session id, and for a Claude Code subagent its agent id and
//! type), and interlock keeps, in the store, which caller each guided attempt
//! belongs to:
//!
//! - an attempt is bound to the caller whose `interlock attempt start` the
//!   hooks saw, and governs only that caller (and that caller's subagents);
//! - a verifier attempt counts as independent only when the host named the
//!   caller as interlock's verifier subagent; the main agent of a session
//!   holding the task's worker attempt may not open one at all;
//! - a verifier's credentials work only for the caller it is bound to;
//! - governance ends when the attempt is submitted, ended or superseded.
//!
//! Where a host does not name its subagents, `interlock verify` launches the
//! verifier itself (see `Supervisor::verify`).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use interlock_adapter::hooks::{HookEvent, HookResponse, action_of, parse_event};
use interlock_schema::{Attempt, AttemptStatus, Binding, BoundVia, Role, State, Timestamp};
use interlock_store::Store;
use serde::Serialize;
use serde_json::Value;

use crate::git;
use crate::hook::{self, HookContext};
use crate::run::{Result, RunError};

/// The custom agents interlock generates for verification, by host.
pub const VERIFIER_AGENTS: &[&str] = &["interlock:verifier", "interlock-verifier"];

/// How long a hook's record of an `attempt start` waits for the command to run.
const INTENT_WINDOW_SECS: i64 = 120;

/// Who is calling, as the host reports it in a hook payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Identity {
    pub session: Option<String>,
    /// The host's id for a subagent; absent for the session's main agent.
    pub agent: Option<String>,
    pub agent_type: Option<String>,
}

impl Identity {
    pub fn from_payload(payload: &Value) -> Identity {
        let s = |k: &str| payload[k].as_str().filter(|v| !v.is_empty()).map(str::to_string);
        Identity {
            session: s("session_id").or_else(|| s("sessionId")),
            agent: s("agent_id"),
            agent_type: s("agent_type"),
        }
    }

    fn is_main(&self) -> bool {
        self.agent.is_none()
    }

    fn matches(&self, b: &Binding) -> bool {
        self.session.is_some() && b.host_session == self.session && b.host_agent == self.agent
    }
}

fn store_err(e: impl std::fmt::Display) -> RunError {
    RunError::Other(e.to_string())
}

/// Guided bookkeeping lives beside the store's own tables, created on first use.
fn ensure_tables(store: &Store) -> Result<()> {
    store
        .connection()
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS guided_intents (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                session    TEXT,
                agent      TEXT,
                agent_type TEXT,
                task_id    TEXT,
                role       TEXT,
                at         TEXT NOT NULL,
                consumed   INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS notes (
                seq     INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                kind    TEXT NOT NULL,
                body    TEXT NOT NULL,
                at      TEXT NOT NULL
            );",
        )
        .map_err(store_err)
}

/// The repository that owns the store: git's top level for the store's
/// directory. Never taken from the caller's directory or git environment.
pub fn repo_of_store(db: &Path) -> Result<PathBuf> {
    let dir = db.parent().ok_or_else(|| RunError::Other(format!("{} has no directory", db.display())))?;
    // Before setup the store's directory may not exist yet: ask from its nearest existing ancestor.
    let existing = dir.ancestors().find(|d| d.is_dir()).unwrap_or(dir);
    Ok(git::toplevel(existing)?)
}

/// Words of an `interlock attempt start` command: (task, role).
fn parse_attempt_start(command: &str) -> Option<(Option<String>, Option<String>)> {
    let words: Vec<&str> = command.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\'')).collect();
    let at = words.windows(3).position(|w| w[0].ends_with("interlock") && w[1] == "attempt" && w[2] == "start")?;
    let rest = &words[at + 3..];
    let task = rest
        .iter()
        .take_while(|w| !matches!(**w, "&&" | ";" | "|"))
        .find(|w| !w.starts_with('-'))
        .map(|s| s.to_string());
    let role = rest
        .windows(2)
        .find(|w| w[0] == "--role")
        .map(|w| w[1].to_string())
        .or_else(|| rest.iter().find_map(|w| w.strip_prefix("--role=")).map(str::to_string));
    Some((task, role))
}

fn record_intent(store: &Store, who: &Identity, task: Option<&str>, role: Option<&str>, now: Timestamp) -> Result<()> {
    store
        .connection()
        .execute(
            "INSERT INTO guided_intents (session, agent, agent_type, task_id, role, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![who.session, who.agent, who.agent_type, task, role, now.to_rfc3339()],
        )
        .map_err(store_err)?;
    Ok(())
}

/// The caller whose `attempt start` for this task and role the hooks saw last,
/// within the last two minutes. Consumed once read.
pub fn take_intent(store: &Store, task: &str, role: Role, now: Timestamp) -> Result<Option<Identity>> {
    ensure_tables(store)?;
    let role = role_name(role);
    let c = store.connection();
    let row = c
        .query_row(
            "SELECT seq, session, agent, agent_type, at FROM guided_intents
             WHERE consumed = 0 AND (task_id = ?1 OR task_id IS NULL) AND (role = ?2 OR role IS NULL)
             ORDER BY seq DESC LIMIT 1",
            rusqlite::params![task, role],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .ok();
    let Some((seq, session, agent, agent_type, at)) = row else { return Ok(None) };
    c.execute("UPDATE guided_intents SET consumed = 1 WHERE seq = ?1", [seq]).map_err(store_err)?;
    let fresh = DateTime::parse_from_rfc3339(&at)
        .is_ok_and(|t| now - t.with_timezone(&Utc) < Duration::seconds(INTENT_WINDOW_SECS));
    Ok(fresh.then_some(Identity { session, agent, agent_type }))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Worker => "worker",
        Role::Verifier => "verifier",
        Role::Reviewer => "reviewer",
    }
}

/// How a guided attempt is bound, given who opened it.
pub fn binding_for(role: Role, interactive: bool, opener: Option<&Identity>) -> Binding {
    let Some(who) = opener.filter(|_| interactive) else { return Binding::unbound(interactive) };
    let via = match role {
        Role::Worker => BoundVia::Session,
        Role::Verifier | Role::Reviewer
            if who.agent.is_some() && who.agent_type.as_deref().is_some_and(|t| VERIFIER_AGENTS.contains(&t)) =>
        {
            BoundVia::Subagent
        }
        Role::Verifier | Role::Reviewer => BoundVia::Unbound,
    };
    Binding {
        via,
        interactive: true,
        host_session: who.session.clone(),
        host_agent: who.agent.clone(),
        agent_type: who.agent_type.clone(),
    }
}

/// A verifier may not be the agent that did the work: refuses the main agent
/// of a session that holds an open worker attempt on the task.
pub fn may_open_verifier(store: &Store, who: &Identity, task: Option<&str>) -> std::result::Result<(), String> {
    if !who.is_main() || who.session.is_none() {
        return Ok(());
    }
    let open = store.open_attempts().map_err(|e| format!("interlock could not read its attempts: {e}"))?;
    let holds = open.iter().any(|a| {
        a.role == Role::Worker
            && task.is_none_or(|t| a.task_id == t)
            && a.binding.as_ref().is_some_and(|b| who.matches(b))
    });
    if holds {
        return Err("this session did the work, so it cannot verify it: hand the task to the independent verifier \
                    (the verify skill says how)"
            .into());
    }
    Ok(())
}

/// The running guided attempt that governs a call from `who`: one bound to
/// exactly this caller; for a subagent, else the one bound to its session's
/// main agent; else an interactive attempt nobody has claimed yet, which this
/// caller now claims. Submitted, ended and superseded attempts govern nothing.
pub fn governing(store: &mut Store, who: &Identity) -> Result<Option<Attempt>> {
    let open: Vec<Attempt> = store
        .open_attempts()?
        .into_iter()
        .filter(|a| a.status == AttemptStatus::Running && a.binding.as_ref().is_some_and(|b| b.interactive))
        .collect();
    if open.is_empty() {
        return Ok(None);
    }
    if who.session.is_none() {
        return Err(RunError::Other(
            "the host did not say which session is calling, and a guided attempt is open".into(),
        ));
    }
    if let Some(a) = open.iter().find(|a| a.binding.as_ref().is_some_and(|b| who.matches(b))) {
        return Ok(Some(a.clone()));
    }
    if who.agent.is_some() {
        let main = Identity { agent: None, agent_type: None, ..who.clone() };
        if let Some(a) = open.iter().find(|a| a.binding.as_ref().is_some_and(|b| main.matches(b))) {
            return Ok(Some(a.clone()));
        }
    }
    // Only a worker attempt is claimed this way: a verifier's credentials must
    // never pass to whichever session happens to call next.
    let unclaimed =
        |a: &&Attempt| a.role == Role::Worker && a.binding.as_ref().is_some_and(|b| b.host_session.is_none());
    if let Some(a) = open.iter().find(unclaimed) {
        let mut binding = a.binding.clone().unwrap_or_else(|| Binding::unbound(true));
        binding.via = BoundVia::Session;
        binding.host_session = who.session.clone();
        binding.host_agent = who.agent.clone();
        binding.agent_type = who.agent_type.clone();
        return Ok(Some(store.bind_attempt(&a.id, binding)?));
    }
    Ok(None)
}

/// The attempt id a shell command passes to interlock: `--attempt X`,
/// `--attempt=X`, `attempt end X`, or `INTERLOCK_ATTEMPT=X`, with `$NAME`
/// and `${NAME}` taken from assignments earlier in the same command.
fn attempt_in_command(command: &str) -> Option<String> {
    let words: Vec<String> = command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|w| !w.is_empty())
        .collect();
    let lookup = |v: &str| -> Option<String> {
        let name = v.strip_prefix("${").and_then(|n| n.strip_suffix('}')).or_else(|| v.strip_prefix('$'))?;
        let prefix = format!("{name}=");
        words
            .iter()
            .rev()
            .find_map(|w| w.strip_prefix("export ").unwrap_or(w).strip_prefix(&prefix).map(str::to_string))
    };
    let resolve = |v: &str| if v.starts_with('$') { lookup(v) } else { Some(v.to_string()) };
    for (i, w) in words.iter().enumerate() {
        if w == "--attempt" {
            return words.get(i + 1).and_then(|v| resolve(v));
        }
        if let Some(v) = w.strip_prefix("--attempt=").or_else(|| w.strip_prefix("INTERLOCK_ATTEMPT=")) {
            return resolve(v);
        }
        if w == "end" && i > 0 && words[i - 1] == "attempt" {
            return words.get(i + 1).filter(|v| !v.starts_with('-')).and_then(|v| resolve(v));
        }
    }
    None
}

/// Whether a command runs an interlock subcommand that reports with an
/// attempt's credentials, and which.
fn credential_command(command: &str) -> Option<&'static str> {
    let flat = command.split_whitespace().collect::<Vec<_>>().join(" ");
    ["assess add", "check run", "attempt end", "claim add", "result submit"]
        .into_iter()
        .find(|sub| flat.contains(&format!("interlock {sub}")))
}

/// A verifier's credentials work only for the caller the verifier attempt is
/// bound to; a verifier interlock launched keeps them inside its own session.
pub fn check_credentials(store: &Store, who: &Identity, command: &str) -> std::result::Result<(), String> {
    let Some(sub) = credential_command(command) else { return Ok(()) };
    let open = store.open_attempts().map_err(|e| format!("interlock could not read its attempts: {e}"))?;
    let verifiers: Vec<&Attempt> = open.iter().filter(|a| a.role != Role::Worker).collect();
    let mine = |a: &Attempt| a.binding.as_ref().is_some_and(|b| b.host_session.is_some() && who.matches(b));
    match attempt_in_command(command) {
        Some(id) => {
            let Ok(attempt) = store.attempt(&id) else { return Ok(()) };
            if attempt.role != Role::Worker && !mine(&attempt) {
                return Err(format!(
                    "attempt {id} belongs to another agent: only the verifier it is bound to may use its credentials"
                ));
            }
            Ok(())
        }
        None if sub == "assess add" && !verifiers.iter().any(|a| mine(a)) => {
            Err("only a bound verifier records assessments; pass --attempt <id> from your own verifier attempt".into())
        }
        None if verifiers.iter().any(|a| !mine(a)) => {
            Err(format!("pass --attempt <id> to `interlock {sub}`, so interlock can tell whose credentials these are"))
        }
        None => Ok(()),
    }
}

/// Whether the store holds an open guided attempt, for failing closed when a
/// hook cannot read its payload.
pub fn guided_attempt_open(db: &Path) -> bool {
    if !db.exists() {
        return false;
    }
    match Store::open(db).and_then(|s| s.open_attempts()) {
        Ok(open) => open.iter().any(|a| a.binding.as_ref().is_some_and(|b| b.interactive)),
        Err(_) => true,
    }
}

/// One hook call in a guided session (interactive mode, no attempt in the
/// environment). Fails closed: a store that exists but cannot be read denies.
pub fn handle(ctx: &HookContext, payload: &Value, now: Timestamp) -> HookResponse {
    if !ctx.db.exists() {
        return HookResponse::allow();
    }
    let mut store = match Store::open(&ctx.db) {
        Ok(s) => s,
        Err(e) => return HookResponse::deny(&format!("interlock could not open its store: {e}")),
    };
    if let Err(e) = ensure_tables(&store) {
        return HookResponse::deny(&format!("interlock could not read its guided sessions: {e}"));
    }
    let who = Identity::from_payload(payload);
    let governed = match parse_event(payload) {
        HookEvent::PreToolUse { tool_name, input, cwd } => {
            let action = action_of(&tool_name, &input);
            if let Some(command) = action.command.as_deref() {
                if let Some((task, role)) = parse_attempt_start(command) {
                    // A reviewer records no evidence, so review may run in the working session.
                    if role.as_deref() == Some("verifier") {
                        if let Err(reason) = may_open_verifier(&store, &who, task.as_deref()) {
                            return HookResponse::deny(&reason);
                        }
                    }
                    if let Err(e) = record_intent(&store, &who, task.as_deref(), role.as_deref(), now) {
                        return HookResponse::deny(&format!("interlock could not record the attempt's caller: {e}"));
                    }
                }
                if let Err(reason) = check_credentials(&store, &who, command) {
                    return HookResponse::deny(&reason);
                }
            }
            match governing(&mut store, &who) {
                // A call no attempt governs still keeps out of interlock's state.
                Ok(None) if action.governed => match hook::state_guard(&ctx.db, &action, cwd.as_deref(), None) {
                    Some(denied) => return denied,
                    None => Ok(None),
                },
                other => other,
            }
        }
        HookEvent::Stop { .. } => stopping(&store, &who),
        HookEvent::Other(_) => Ok(None),
    };
    match governed {
        Ok(Some(attempt)) => {
            let ctx = HookContext { attempt_id: Some(attempt.id), ..ctx.clone() };
            drop(store);
            hook::handle(&ctx, payload, now)
        }
        Ok(None) => HookResponse::allow(),
        Err(e) => HookResponse::deny(&format!("interlock cannot tell which attempt governs this call: {e}")),
    }
}

/// The attempt a stop event holds to: only one bound to exactly the agent that
/// is stopping. A Copilot subagent's stop names its session as `agent_id`.
fn stopping(store: &Store, who: &Identity) -> Result<Option<Attempt>> {
    let mut candidates = vec![who.clone()];
    if let Some(agent) = &who.agent {
        candidates.push(Identity { session: Some(agent.clone()), agent: None, agent_type: None });
    }
    Ok(store
        .open_attempts()?
        .into_iter()
        .filter(|a| a.status == AttemptStatus::Running)
        .find(|a| a.binding.as_ref().is_some_and(|b| b.interactive && candidates.iter().any(|c| c.matches(b)))))
}

/// A task id that is safe as part of a worktree's directory name.
fn safe_component(task_id: &str) -> Result<&str> {
    let ok = !task_id.is_empty()
        && !task_id.starts_with('.')
        && task_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok { Ok(task_id) } else { Err(RunError::Other(format!("task id {task_id:?} cannot name a worktree directory"))) }
}

/// Creates a fresh worktree for the next `role` attempt on a task, under the
/// store's directory, and returns its path. Workers start from the input
/// snapshot, or from the last accepted output when reworking; verifiers and
/// reviewers start from the output under verification. No branch moves.
pub fn prepare_worktree(store: &Store, repo: &Path, db: &Path, task_id: &str, role: Role) -> Result<PathBuf> {
    let task = store.task(task_id)?;
    let name = safe_component(task_id)?;
    let base =
        task.input_snapshot.as_ref().map(|s| s.base_commit.clone()).ok_or_else(|| {
            RunError::Other(format!("task {task_id} has no input snapshot; run `interlock task ready`"))
        })?;
    let (dir_name, commit) = match role {
        Role::Worker => {
            let commit = match &task.current_tree {
                Some(tree) => {
                    git::commit_tree(repo, tree, Some(&base), &format!("interlock: {task_id} previous output"))?
                }
                None => base,
            };
            (format!("{name}-worker-{}", task.attempts_used + 1), commit)
        }
        Role::Verifier | Role::Reviewer => {
            let tree = task
                .current_tree
                .clone()
                .ok_or_else(|| RunError::Other(format!("task {task_id} has no output to verify yet")))?;
            let n = store.attempts(task_id)?.iter().filter(|a| a.role == role).count() + 1;
            let role_name = role_name(role);
            let commit =
                git::commit_tree(repo, &tree, Some(&base), &format!("interlock: {task_id} output to {role_name}"))?;
            (format!("{name}-{role_name}-{n}"), commit)
        }
    };
    let dir = db.parent().unwrap_or(Path::new(".interlock")).join("worktrees").join(dir_name);
    let _ = git::worktree_remove(repo, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    git::worktree_add(repo, &dir, &commit)?;
    Ok(dir)
}

/// Removes a worktree made by `prepare_worktree`, for an attempt that did not open.
pub fn discard_worktree(repo: &Path, dir: &Path) {
    let _ = git::worktree_remove(repo, dir);
    let _ = std::fs::remove_dir_all(dir);
}

/// Removes the worktrees of a task's attempts once the task is done, failed
/// or cancelled. Returns the ones removed.
pub fn cleanup_worktrees(store: &Store, repo: &Path, db: &Path, task_id: &str) -> Result<Vec<PathBuf>> {
    let task = store.task(task_id)?;
    if !matches!(task.state, State::Done | State::Failed | State::Cancelled) {
        return Ok(vec![]);
    }
    let root = db.parent().unwrap_or(Path::new(".interlock")).join("worktrees");
    let mut removed = Vec::new();
    for a in store.attempts(task_id)? {
        let Some(dir) = a.worktree.map(PathBuf::from) else { continue };
        if dir.starts_with(&root) && dir.exists() {
            discard_worktree(repo, &dir);
            removed.push(dir);
        }
    }
    Ok(removed)
}

/// The paths a result changes relative to the task's input snapshot, read from
/// the output tree in the repository that owns the store. An error when they
/// cannot be read: G3 never falls back to a list the caller supplies.
pub fn changed_paths(store: &Store, repo: &Path, task_id: &str, tree: &str) -> Result<Vec<String>> {
    let task = store.task(task_id)?;
    let base = task
        .input_snapshot
        .as_ref()
        .map(|s| s.base_commit.clone())
        .ok_or_else(|| RunError::Other(format!("task {task_id} has no input snapshot")))?;
    let base_tree = git::tree_of(repo, &base)?;
    if git::object_type(repo, tree).as_deref() != Some("tree") {
        return Err(RunError::Other(format!("{tree} is not a tree in {}", repo.display())));
    }
    Ok(git::changed_paths(repo, &base_tree, tree)?)
}

/// A note kept on a task: a design, a review, an answer.
#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub seq: i64,
    pub task_id: String,
    pub kind: String,
    pub body: String,
    pub at: String,
}

pub fn add_note(store: &Store, task_id: &str, kind: &str, body: &str, now: Timestamp) -> Result<Note> {
    store.task(task_id)?;
    ensure_tables(store)?;
    let c = store.connection();
    c.execute(
        "INSERT INTO notes (task_id, kind, body, at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![task_id, kind, body, now.to_rfc3339()],
    )
    .map_err(store_err)?;
    let seq = c.last_insert_rowid();
    Ok(Note { seq, task_id: task_id.into(), kind: kind.into(), body: body.into(), at: now.to_rfc3339() })
}

pub fn notes(store: &Store, task_id: &str) -> Result<Vec<Note>> {
    ensure_tables(store)?;
    let mut stmt = store
        .connection()
        .prepare("SELECT seq, task_id, kind, body, at FROM notes WHERE task_id = ?1 ORDER BY seq")
        .map_err(store_err)?;
    let rows = stmt
        .query_map([task_id], |r| {
            Ok(Note { seq: r.get(0)?, task_id: r.get(1)?, kind: r.get(2)?, body: r.get(3)?, at: r.get(4)? })
        })
        .map_err(store_err)?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(store_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use interlock_core::capability::{Capability, CapabilitySet};
    use interlock_core::grants::{HostPolicy, Profile};
    use interlock_core::lifecycle::TaskSpec;
    use interlock_core::workflow::Mode;
    use interlock_schema::{HostRef, Snapshot};
    use interlock_store::{StartAttempt, Started, SubmitResult};
    use serde_json::json;

    fn who(session: &str, agent: Option<&str>, agent_type: Option<&str>) -> Identity {
        Identity {
            session: Some(session.into()),
            agent: agent.map(str::to_string),
            agent_type: agent_type.map(str::to_string),
        }
    }

    struct Fixture {
        repo: tempfile::TempDir,
        db: PathBuf,
        store: Store,
        base: String,
    }

    fn fixture() -> Fixture {
        let repo = git::tests::repo_with(&[("calc.py", "def add(a, b):\n    return a - b\n"), ("README.md", "r\n")]);
        let db = repo.path().join(".interlock/state.db");
        let mut store = Store::open(&db).unwrap();
        let spec: TaskSpec = serde_json::from_value(json!({
            "id": "t1", "repository": ".", "workflow": "bug-fix", "intent": "fix add",
            "scope": {"paths": ["calc.py"]},
            "criteria": [{"id": "c", "statement": "add adds", "min_strength": "observed", "producer": "independent"}]
        }))
        .unwrap();
        store.create_task(spec, Utc::now()).unwrap();
        let base = git::head(repo.path()).unwrap();
        let snapshot = Snapshot {
            repository: ".".into(),
            base_commit: base.clone(),
            untracked_hash: None,
            protected_paths: vec![],
        };
        store.ready("t1", snapshot, Utc::now()).unwrap();
        Fixture { repo, db, store, base }
    }

    fn start(f: &mut Fixture, role: Role, binding: Binding) -> (Attempt, String) {
        let caps: CapabilitySet = [
            Capability::SessionStart,
            Capability::SessionCollect,
            Capability::SessionCancel,
            Capability::ToolRestriction,
            Capability::PerCallPolicy,
            Capability::CustomAgents,
        ]
        .into_iter()
        .collect();
        let started = f
            .store
            .start_attempt(
                StartAttempt {
                    task_id: "t1".into(),
                    role,
                    mode: Mode::Interactive,
                    host: HostRef { host: "claude-code".into(), version: "0".into() },
                    capabilities: caps,
                    profile: Profile::Conservative,
                    host_policy: HostPolicy::open(),
                    agent: None,
                    model: None,
                    worktree: None,
                },
                Utc::now(),
            )
            .unwrap();
        let Started::Yes { attempt, token, .. } = started else { panic!("not started") };
        (f.store.bind_attempt(&attempt.id, binding).unwrap(), token)
    }

    /// A worker bound to session S that submitted a change to calc.py.
    fn submitted(f: &mut Fixture) -> Attempt {
        let (worker, token) = start(f, Role::Worker, binding_for(Role::Worker, true, Some(&who("S", None, None))));
        std::fs::write(f.repo.path().join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        let tree = git::worktree_tree(f.repo.path()).unwrap();
        let changed = changed_paths(&f.store, f.repo.path(), "t1", &tree).unwrap();
        assert_eq!(changed, ["calc.py"]);
        let req = SubmitResult {
            attempt_id: worker.id.clone(),
            token,
            epoch: worker.epoch,
            output_tree: tree,
            changed_paths: changed,
            summary: "fixed".into(),
            open_questions: vec![],
            event_id: None,
        };
        f.store.submit_result(req, Utc::now()).unwrap();
        f.store.attempt(&worker.id).unwrap()
    }

    #[test]
    fn a_verifier_counts_as_independent_only_as_interlocks_named_subagent() {
        let sub = who("S", Some("a1"), Some("interlock:verifier"));
        assert_eq!(binding_for(Role::Verifier, true, Some(&sub)).via, BoundVia::Subagent);
        let other = who("S", Some("a2"), Some("general-purpose"));
        assert_eq!(binding_for(Role::Verifier, true, Some(&other)).via, BoundVia::Unbound, "any other subagent");
        let main = who("S", None, Some("interlock:verifier"));
        assert_eq!(binding_for(Role::Verifier, true, Some(&main)).via, BoundVia::Unbound, "a type without an agent id");
        // A Copilot subagent's calls carry their own session and no agent fields.
        assert_eq!(binding_for(Role::Verifier, true, Some(&who("child", None, None))).via, BoundVia::Unbound);
        assert_eq!(binding_for(Role::Verifier, true, None), Binding::unbound(true), "nobody seen");
        assert_eq!(binding_for(Role::Verifier, false, Some(&sub)), Binding::unbound(false), "headless from the CLI");
        let w = binding_for(Role::Worker, true, Some(&who("S", None, None)));
        assert_eq!((w.via, w.host_session.as_deref()), (BoundVia::Session, Some("S")));
    }

    #[test]
    fn the_session_that_did_the_work_cannot_open_a_verifier() {
        let mut f = fixture();
        submitted(&mut f);
        let refused = may_open_verifier(&f.store, &who("S", None, None), Some("t1")).unwrap_err();
        assert!(refused.contains("did the work"), "{refused}");
        assert!(may_open_verifier(&f.store, &who("S", Some("a1"), Some("interlock:verifier")), Some("t1")).is_ok());
        assert!(may_open_verifier(&f.store, &who("other", None, None), Some("t1")).is_ok(), "another session");
    }

    #[test]
    fn a_verifiers_credentials_work_only_for_its_own_caller() {
        let mut f = fixture();
        submitted(&mut f);
        let sub = who("S", Some("a1"), Some("interlock:verifier"));
        let (v, _) = start(&mut f, Role::Verifier, binding_for(Role::Verifier, true, Some(&sub)));
        let main = who("S", None, None);
        let id = &v.id;
        for cmd in [
            format!("interlock assess add --criterion c --strength observed --tree auto --attempt {id} --token x"),
            format!("cd /r && interlock check run --criterion c --attempt={id} --token x"),
            format!("interlock attempt end {id} --token x --note done"),
            format!("A={id}; interlock assess add --criterion c --strength observed --attempt \"$A\" --token x"),
            format!("INTERLOCK_ATTEMPT={id} interlock assess add --criterion c --strength observed --tree auto"),
        ] {
            let refused = check_credentials(&f.store, &main, &cmd).unwrap_err();
            assert!(refused.contains("belongs to another agent"), "{cmd}: {refused}");
            assert_eq!(check_credentials(&f.store, &sub, &cmd), Ok(()), "{cmd}");
        }
        // Without an attempt id interlock cannot tell whose credentials they are.
        let bare = "interlock assess add --criterion c --strength observed --tree auto --token x";
        assert!(check_credentials(&f.store, &main, bare).is_err());
        let hidden = "interlock check run --criterion c --attempt \"$(cat /tmp/a)\" --token x";
        assert!(check_credentials(&f.store, &main, hidden).is_err(), "an id it cannot read is refused");
        assert_eq!(check_credentials(&f.store, &main, "interlock status t1"), Ok(()));
    }

    #[test]
    fn an_attempt_governs_only_its_own_session_until_ended() {
        let mut f = fixture();
        let (worker, token) = start(&mut f, Role::Worker, binding_for(Role::Worker, true, Some(&who("S", None, None))));
        assert_eq!(governing(&mut f.store, &who("S", None, None)).unwrap().map(|a| a.id), Some(worker.id.clone()));
        assert_eq!(
            governing(&mut f.store, &who("S", Some("a9"), Some("Explore"))).unwrap().map(|a| a.id),
            Some(worker.id.clone()),
            "its session's subagents too"
        );
        assert_eq!(governing(&mut f.store, &who("T", None, None)).unwrap(), None, "another session is free");
        assert!(governing(&mut f.store, &Identity::default()).is_err(), "a caller with no session fails closed");
        f.store.end_attempt(&worker.id, &token, false, Some("stop".into()), Utc::now()).unwrap();
        assert_eq!(governing(&mut f.store, &who("S", None, None)).unwrap(), None, "released once ended");
    }

    #[test]
    fn a_submitted_attempt_governs_nothing() {
        let mut f = fixture();
        submitted(&mut f);
        assert_eq!(governing(&mut f.store, &who("S", None, None)).unwrap(), None);
    }

    #[test]
    fn an_unclaimed_worker_goes_to_the_first_caller_but_a_verifier_never_does() {
        let mut f = fixture();
        let (worker, _) = start(&mut f, Role::Worker, Binding::unbound(true));
        let claimed = governing(&mut f.store, &who("S", None, None)).unwrap().unwrap();
        assert_eq!(claimed.id, worker.id);
        assert_eq!(claimed.binding.as_ref().unwrap().via, BoundVia::Session);
        assert_eq!(governing(&mut f.store, &who("T", None, None)).unwrap(), None, "now it is S's");

        let mut f = fixture();
        submitted(&mut f);
        let (v, _) = start(&mut f, Role::Verifier, Binding::unbound(true));
        assert_eq!(governing(&mut f.store, &who("T", None, None)).unwrap(), None);
        assert_eq!(f.store.attempt(&v.id).unwrap().binding, Some(Binding::unbound(true)), "still nobody's");
        let cmd = format!("interlock assess add --criterion c --strength observed --attempt {} --token x", v.id);
        assert!(check_credentials(&f.store, &who("T", None, None), &cmd).is_err());
    }

    #[test]
    fn intents_name_the_caller_once_and_expire() {
        let f = fixture();
        ensure_tables(&f.store).unwrap();
        let sub = who("S", Some("a1"), Some("interlock:verifier"));
        let now = Utc::now();
        let cmd = "cd /repo && interlock attempt start t1 --role verifier --host claude-code --worktree auto";
        let (task, role) = parse_attempt_start(cmd).unwrap();
        assert_eq!((task.as_deref(), role.as_deref()), (Some("t1"), Some("verifier")));
        let eq = parse_attempt_start("interlock attempt start t1 --role=worker").unwrap();
        assert_eq!(eq.1.as_deref(), Some("worker"));
        assert_eq!(parse_attempt_start("interlock status t1"), None);
        record_intent(&f.store, &sub, task.as_deref(), role.as_deref(), now).unwrap();
        assert_eq!(take_intent(&f.store, "t1", Role::Worker, now).unwrap(), None, "another role");
        assert_eq!(take_intent(&f.store, "t1", Role::Verifier, now).unwrap(), Some(sub.clone()));
        assert_eq!(take_intent(&f.store, "t1", Role::Verifier, now).unwrap(), None, "consumed");
        record_intent(&f.store, &sub, Some("t1"), Some("verifier"), now - Duration::seconds(600)).unwrap();
        assert_eq!(take_intent(&f.store, "t1", Role::Verifier, now).unwrap(), None, "too old");
    }

    #[test]
    fn task_ids_cannot_climb_out_of_the_worktrees_folder() {
        let f = fixture();
        for bad in ["../../../victim", "..", ".hidden", "a/b", ""] {
            assert!(safe_component(bad).is_err(), "{bad:?}");
        }
        assert!(safe_component("fix-add_2.v1").is_ok());
        let dir = prepare_worktree(&f.store, f.repo.path(), &f.db, "t1", Role::Worker).unwrap();
        assert!(dir.starts_with(f.repo.path().join(".interlock/worktrees")), "{}", dir.display());
    }

    #[test]
    fn changes_are_read_from_a_real_tree_or_not_at_all() {
        let f = fixture();
        assert!(changed_paths(&f.store, f.repo.path(), "t1", "not-a-tree").is_err());
        assert!(changed_paths(&f.store, f.repo.path(), "t1", &f.base).is_err(), "a commit id is not a tree");
        let elsewhere = tempfile::tempdir().unwrap();
        let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
        assert!(changed_paths(&f.store, elsewhere.path(), "t1", empty_tree).is_err(), "not a repository");
    }
}
