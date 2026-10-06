//! What interlock's hooks plugin runs on each tool call and each stop. The
//! same code serves Claude Code and Copilot CLI. It fails closed: a governed
//! action is denied when the attempt cannot be loaded.

use std::path::{Path, PathBuf};

use interlock_adapter::hooks::{Action, HookEvent, HookResponse, action_of, parse_event};
use interlock_core::evidence::{same_currency, same_tree};
use interlock_core::grants::{self, Verdict};
use interlock_core::scope::{Placement, place};
use interlock_schema::{AttemptStatus, Currency, Producer, Role, RunTarget, Timestamp};
use interlock_store::Store;
use serde_json::Value;

use crate::git;
use crate::state_paths::{is_interlock_state_path, normalize, state_paths_in_command};

/// Where the hook finds its attempt, from the session's environment.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub db: PathBuf,
    pub attempt_id: Option<String>,
    /// Headless sessions have nobody to ask, so "ask" becomes "deny".
    pub headless: bool,
    pub host: String,
    pub tree: Option<String>,
}

impl HookContext {
    pub fn from_env(db: PathBuf) -> HookContext {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        HookContext {
            db,
            attempt_id: var("INTERLOCK_ATTEMPT"),
            headless: var("INTERLOCK_MODE").as_deref() != Some("interactive"),
            host: var("INTERLOCK_HOST").unwrap_or_else(|| "claude-code".into()),
            tree: var("INTERLOCK_TREE"),
        }
    }
}

pub fn handle(ctx: &HookContext, payload: &Value, _now: Timestamp) -> HookResponse {
    // Outside an interlock attempt the hooks do nothing.
    let Some(attempt_id) = ctx.attempt_id.as_deref() else { return HookResponse::allow() };
    match parse_event(payload) {
        HookEvent::PreToolUse { tool_name, input, cwd } => pre_tool_use(ctx, attempt_id, &tool_name, &input, cwd),
        HookEvent::Stop { already_continued, cwd } => {
            if already_continued {
                // The agent was already held once; holding it again risks a loop.
                HookResponse::allow()
            } else {
                stop(ctx, attempt_id, cwd)
            }
        }
        HookEvent::Other(_) => HookResponse::allow(),
    }
}

/// Denies an action that would change, or a command that names, interlock's
/// state: everything under the store's directory except `own`, the caller's
/// own worktree. Paths resolve against `own` (edits) or `cwd` (commands).
pub fn state_guard(db: &Path, action: &Action, cwd: Option<&Path>, own: Option<&Path>) -> Option<HookResponse> {
    let interlock_dir = db.parent().map(PathBuf::from).unwrap_or_default();
    let edit_root = own.or(cwd).map(Path::to_path_buf).unwrap_or_default();
    let mut named: Vec<String> = action
        .writes
        .iter()
        .filter(|p| is_interlock_state_path(&normalize(p, &edit_root), &interlock_dir, own))
        .map(|p| p.display().to_string())
        .collect();
    if let Some(cmd) = &action.command {
        let shell_dir = cwd.map(Path::to_path_buf).unwrap_or_else(|| edit_root.clone());
        named.extend(state_paths_in_command(cmd, &shell_dir, &interlock_dir, own));
    }
    (!named.is_empty()).then(|| {
        HookResponse::deny(&format!(
            "{} is interlock's own state and off limits; report through the interlock command",
            named.join(", ")
        ))
    })
}

fn pre_tool_use(
    ctx: &HookContext,
    attempt_id: &str,
    tool_name: &str,
    input: &Value,
    cwd: Option<PathBuf>,
) -> HookResponse {
    let action = action_of(tool_name, input);
    let loaded = Store::open(&ctx.db).and_then(|s| {
        let attempt = s.attempt(attempt_id)?;
        let task = s.task(&attempt.task_id)?;
        Ok((attempt, task))
    });
    let worktree = loaded.as_ref().ok().and_then(|(a, _)| a.worktree.clone()).map(PathBuf::from);
    if let Some(reason) = forbidden(ctx, worktree.as_deref(), cwd.as_deref(), input) {
        return HookResponse::deny(&reason);
    }
    if !action.governed {
        return HookResponse::allow();
    }
    let (attempt, task) = match loaded {
        Ok(v) => v,
        Err(e) => return HookResponse::deny(&format!("interlock could not load attempt {attempt_id}: {e}")),
    };
    if !matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted) {
        return HookResponse::deny(&format!("attempt {attempt_id} is no longer active ({:?})", attempt.status));
    }
    if let Some(denied) = state_guard(&ctx.db, &action, cwd.as_deref(), attempt.worktree.as_deref().map(Path::new)) {
        return denied;
    }
    if !action.writes.is_empty() {
        let root = attempt.worktree.clone().map(PathBuf::from).or(cwd).unwrap_or_default();
        for path in &action.writes {
            match place(&root, path, &task.scope.paths) {
                Placement::InScope(_) => {}
                Placement::OutOfScope(rel) => {
                    return HookResponse::deny(&format!(
                        "{rel} is outside the task's scope ({})",
                        task.scope.paths.join(", ")
                    ));
                }
                Placement::Outside => {
                    return HookResponse::deny(&format!(
                        "{} is outside this attempt's worktree; edit files under {}",
                        path.display(),
                        root.display()
                    ));
                }
            }
        }
    }
    match grants::decide(&attempt.effective_grant, action.class, &action.tool) {
        Verdict::Allow => HookResponse::allow(),
        Verdict::Deny(reason) => HookResponse::deny(&reason),
        Verdict::Ask(reason) if ctx.headless => {
            HookResponse::deny(&format!("{reason}; this session has no operator to ask"))
        }
        Verdict::Ask(reason) => HookResponse::ask(&reason, &ctx.host),
    }
}

/// Input fields that name files, across both hosts' tools.
const PATH_FIELDS: &[&str] = &["file_path", "path", "notebook_path", "pattern", "glob", "directory", "dir", "cwd"];

/// Resolves `p` against `base` without touching the file: `.` and `..` are
/// folded, and the longest prefix that exists is canonicalized, so symlinks
/// cannot hide where a path really leads.
fn resolve(base: &Path, p: &str) -> PathBuf {
    let expanded = match p.strip_prefix('~') {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest.trim_start_matches('/')),
        None => PathBuf::from(p),
    };
    let joined = if expanded.is_absolute() { expanded } else { base.join(expanded) };
    let mut lexical = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            std::path::Component::CurDir => {}
            other => lexical.push(other),
        }
    }
    let mut existing = lexical.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent().map(Path::to_path_buf)) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return lexical,
        }
    }
    let mut out = std::fs::canonicalize(&existing).unwrap_or(existing);
    out.extend(rest.into_iter().rev());
    out
}

fn canonical(p: &Path) -> PathBuf {
    resolve(Path::new("/"), &p.display().to_string())
}

/// Why a tool call is refused for reaching interlock's own state: the store
/// directory (outside this attempt's own worktree, which lives there), the
/// directory holding attempt tokens, or another process's environment. Shell
/// commands are split on whitespace and shell punctuation and every word that
/// looks like a path is resolved; the split does not parse shell grammar, so
/// this narrows what an agent can reach rather than containing it.
pub fn forbidden(ctx: &HookContext, worktree: Option<&Path>, cwd: Option<&Path>, input: &Value) -> Option<String> {
    let store_dir = canonical(ctx.db.parent().unwrap_or(Path::new("/")));
    let tokens = canonical(&crate::sessions::token_dir(&store_dir));
    let worktree = worktree.map(canonical);
    let base = cwd.map(Path::to_path_buf).or_else(|| worktree.clone()).unwrap_or_else(|| PathBuf::from("/"));
    let mut texts: Vec<&str> = PATH_FIELDS.iter().filter_map(|f| input[f].as_str()).collect();
    let command = input["command"].as_str();
    texts.extend(command);
    for text in texts {
        if text.contains("/proc/") && text.contains("environ") {
            return Some("other processes' environments are off limits".into());
        }
        if text.contains("INTERLOCK_DB") || text.contains("state.db") {
            return Some("interlock's store is off limits; report through the interlock command".into());
        }
        let words = text
            .split(|c: char| c.is_whitespace() || ";|&<>()'\"`=,".contains(c))
            .filter(|w| w.contains('/') || w.starts_with('.') || w.starts_with('~'));
        for word in words {
            let path = resolve(&base, word);
            if path.starts_with(&tokens) {
                return Some("interlock's attempt tokens are off limits".into());
            }
            // The generated skills and hooks plugin for guided sessions may be read (skills
            // point the model at their own references); changing them is refused by state_guard.
            let guided_plugin = path.starts_with(store_dir.join("guided"));
            if path.starts_with(&store_dir) && !guided_plugin && !worktree.as_ref().is_some_and(|w| path.starts_with(w))
            {
                return Some(format!(
                    "{word} is inside interlock's own directory; work only in your worktree and report through the \
                     interlock command"
                ));
            }
        }
    }
    None
}

/// Holds an agent from finishing until it has recorded the evidence its role owes.
fn stop(ctx: &HookContext, attempt_id: &str, cwd: Option<PathBuf>) -> HookResponse {
    let Ok(store) = Store::open(&ctx.db) else { return HookResponse::allow() };
    let Ok(attempt) = store.attempt(attempt_id) else { return HookResponse::allow() };
    let Ok(task) = store.task(&attempt.task_id) else { return HookResponse::allow() };
    if !matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted) {
        return HookResponse::allow();
    }
    let dir = attempt.worktree.clone().map(PathBuf::from).or(cwd);
    let (records, tree, owed): (Vec<_>, Option<String>, Vec<_>) = match attempt.role {
        Role::Worker => {
            let tree = dir.as_deref().and_then(|d| git::worktree_tree(d).ok());
            let owed = task.criteria.iter().filter(|c| c.producer == Producer::SelfReport).collect();
            (store.claims(&task.id).unwrap_or_default(), tree, owed)
        }
        Role::Verifier => {
            let tree = ctx.tree.clone().or_else(|| task.current_tree.clone());
            (store.assessments(&task.id).unwrap_or_default(), tree, task.criteria.iter().collect())
        }
        Role::Reviewer => return HookResponse::allow(),
    };
    // Without a tree there is nothing to hold the agent to.
    let Some(tree) = tree else { return HookResponse::allow() };
    let runs = store.check_runs(&task.id).unwrap_or_default();
    let mut missing_records = Vec::new();
    let mut missing_runs = Vec::new();
    for c in &owed {
        let key = Currency {
            tree: tree.clone(),
            check_version: c.check_version.clone(),
            environment: task.environment.clone(),
            policy_digest: task.policy_digest.clone(),
        };
        if !records
            .iter()
            .any(|e| e.attempt_id == attempt.id && e.criterion_id == c.id && same_currency(&e.currency, &key))
        {
            missing_records.push(c.id.as_str());
        }
        let ran = runs.iter().any(|r| {
            r.attempt_id.as_deref() == Some(attempt.id.as_str())
                && r.criterion_id == c.id
                && r.target == RunTarget::Output
                && r.check_version == c.check_version
                && same_tree(&r.tree, &tree)
        });
        if c.check.is_some() && !ran {
            missing_runs.push(c.id.as_str());
        }
    }
    // A verifier's passing assessment below what its criterion needs counts
    // for nothing; say so while the verifier can still look again.
    let mut weak = Vec::new();
    if attempt.role == Role::Verifier {
        for c in &owed {
            let key = Currency {
                tree: tree.clone(),
                check_version: c.check_version.clone(),
                environment: task.environment.clone(),
                policy_digest: task.policy_digest.clone(),
            };
            let latest = records
                .iter()
                .rev()
                .find(|e| e.attempt_id == attempt.id && e.criterion_id == c.id && same_currency(&e.currency, &key));
            if let Some(e) = latest
                && e.strength.pass_rank().is_some()
                && !e.strength.satisfies(c.min_strength)
            {
                let had = serde_json::to_value(e.strength).ok().and_then(|v| v.as_str().map(str::to_string));
                let needs = serde_json::to_value(c.min_strength).ok().and_then(|v| v.as_str().map(str::to_string));
                let needs = needs.unwrap_or_default();
                weak.push(format!(
                    "{}: you recorded `{}`, but it needs `{needs}` or stronger ({}). Record `{needs}` only if that is \
                     what you saw; otherwise record `failed` or `blocked`",
                    c.id,
                    had.unwrap_or_default(),
                    crate::prompts::strength_meaning(c.min_strength)
                ));
            }
        }
    }
    if missing_records.is_empty() && missing_runs.is_empty() && weak.is_empty() {
        return HookResponse::allow();
    }
    // Every suggested command is complete; a headless session has its token in the environment.
    let token = if ctx.headless { "\"$INTERLOCK_TOKEN\"" } else { "<your token from attempt start>" };
    let auth = format!("--attempt {} --token {token}", attempt.id);
    let mut steps = Vec::new();
    if !weak.is_empty() {
        steps.push(format!("look again at what each criterion needs. {}", weak.join(". ")));
    }
    for c in &missing_runs {
        steps.push(format!(
            "have interlock run the check for {c} on your current files: `interlock check run --criterion {c} {auth}`"
        ));
    }
    let strengths = "<observed|tested|static|failed|blocked>";
    for c in &missing_records {
        // A worker is only ever asked for claims; only a verifier records assessments.
        steps.push(match attempt.role {
            Role::Worker => format!(
                "record a claim for {c}: `interlock claim add --criterion {c} --strength {strengths} --tree auto \
                 --ref \"<command you ran>\" --note \"<what you saw>\" {auth}`"
            ),
            _ => format!(
                "record an assessment for {c}: `interlock assess add --criterion {c} --strength {strengths} --tree auto \
                 --ref \"<command you ran>\" --note \"<what you saw>\" {auth}`"
            ),
        });
    }
    let reason =
        format!("Before you finish, {}. Evidence made before your last edit no longer counts.", steps.join("; then "));
    HookResponse::block_stop(&reason)
}

/// Resolves a `--tree` argument: `auto` means the tree of the worktree at `dir`.
pub fn resolve_tree(arg: &str, dir: &Path) -> Result<String, git::GitError> {
    if arg == "auto" { git::worktree_tree(dir) } else { Ok(arg.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use interlock_core::capability::{Capability, CapabilitySet};
    use interlock_core::grants::{HostPolicy, Profile};
    use interlock_core::lifecycle::{EvidenceKind, TaskSpec};
    use interlock_core::workflow::Mode;
    use interlock_schema::{HostRef, Snapshot, Strength};
    use interlock_store::{AddEvidence, StartAttempt, Started};
    use serde_json::json;

    struct Setup {
        _repo: tempfile::TempDir,
        worktree: PathBuf,
        ctx: HookContext,
        token: String,
    }

    fn setup(headless: bool) -> Setup {
        let repo = git::tests::repo_with(&[("src/a.rs", "fn a() {}\n"), ("README.md", "hi\n")]);
        let db = repo.path().join(".interlock/state.db");
        let mut store = Store::open(&db).unwrap();
        let spec: TaskSpec = serde_json::from_value(json!({
            "id": "t1", "repository": ".", "workflow": "bug-fix", "intent": "fix a",
            "scope": {"paths": ["src/**"]},
            "criteria": [{"id": "tests", "statement": "tests pass", "min_strength": "tested", "producer": "self"}]
        }))
        .unwrap();
        store.create_task(spec, Utc::now()).unwrap();
        let base = git::head(repo.path()).unwrap();
        store
            .ready(
                "t1",
                Snapshot {
                    repository: ".".into(),
                    base_commit: base.clone(),
                    untracked_hash: None,
                    protected_paths: vec![],
                },
                Utc::now(),
            )
            .unwrap();
        let worktree = repo.path().join(".interlock/worktrees/w1");
        git::worktree_add(repo.path(), &worktree, &base).unwrap();
        let caps: CapabilitySet = [
            Capability::SessionStart,
            Capability::SessionCollect,
            Capability::SessionCancel,
            Capability::PerCallPolicy,
        ]
        .into_iter()
        .collect();
        let started = store
            .start_attempt(
                StartAttempt {
                    task_id: "t1".into(),
                    role: Role::Worker,
                    mode: Mode::Headless,
                    host: HostRef { host: "test".into(), version: "0".into() },
                    capabilities: caps,
                    profile: Profile::Conservative,
                    host_policy: HostPolicy::open(),
                    agent: None,
                    model: None,
                    worktree: Some(worktree.display().to_string()),
                },
                Utc::now(),
            )
            .unwrap();
        let Started::Yes { attempt, token, .. } = started else { panic!("not started") };
        let ctx = HookContext { db, attempt_id: Some(attempt.id), headless, host: "claude-code".into(), tree: None };
        Setup { _repo: repo, worktree, ctx, token }
    }

    fn pre(s: &Setup, tool: &str, input: Value) -> HookResponse {
        let payload =
            json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input, "cwd": s.worktree});
        handle(&s.ctx, &payload, Utc::now())
    }

    fn stop(s: &Setup, already: bool) -> HookResponse {
        let payload = json!({"hook_event_name": "Stop", "stop_hook_active": already, "cwd": s.worktree});
        handle(&s.ctx, &payload, Utc::now())
    }

    #[test]
    fn outside_an_attempt_the_hooks_do_nothing() {
        let mut s = setup(true);
        s.ctx.attempt_id = None;
        assert_eq!(pre(&s, "Bash", json!({"command": "git push --force"})), HookResponse::allow());
    }

    #[test]
    fn edits_stay_inside_the_worktree_and_the_scope() {
        let s = setup(true);
        let inside = s.worktree.join("src/a.rs").display().to_string();
        assert_eq!(pre(&s, "Edit", json!({"file_path": inside})).exit_code, 0);
        let out_of_scope = pre(&s, "Write", json!({"file_path": s.worktree.join("README.md")}));
        assert_eq!(out_of_scope.exit_code, 2);
        assert!(out_of_scope.stderr.contains("outside the task's scope"), "{}", out_of_scope.stderr);
        let outside = pre(&s, "Write", json!({"file_path": "/etc/hosts"}));
        assert!(outside.stderr.contains("outside this attempt's worktree"), "{}", outside.stderr);
    }

    #[test]
    fn the_store_and_operator_commands_are_off_limits() {
        let s = setup(true);
        let db = s.ctx.db.display().to_string();
        assert_eq!(pre(&s, "Bash", json!({"command": format!("sqlite3 {db} 'delete from claims'")})).exit_code, 2);
        let grant = pre(&s, "Bash", json!({"command": "interlock grant create --classes landing --tasks '*'"}));
        assert_eq!(grant.exit_code, 2);
        assert!(grant.stderr.contains("no operator to ask"));
        assert_eq!(pre(&s, "Bash", json!({"command": "cargo test"})).exit_code, 0);
        assert_eq!(pre(&s, "Bash", json!({"command": "git push origin fix"})).exit_code, 2, "external is not granted");
    }

    #[test]
    fn interlocks_own_directory_tokens_and_other_environments_are_off_limits() {
        let s = setup(true);
        let store_dir = s.ctx.db.parent().unwrap().to_path_buf();
        let denied = |tool: &str, input: Value| {
            let r = pre(&s, tool, input.clone());
            assert_eq!(r.exit_code, 2, "{tool} {input} should be denied");
            r.stderr
        };
        // Reading or listing anything of interlock's outside this attempt's worktree.
        denied("Read", json!({"file_path": store_dir.join("config.toml")}));
        denied("Grep", json!({"pattern": "token", "path": "../"}));
        denied("Glob", json!({"pattern": "../*/src/*.rs"}));
        denied("Bash", json!({"command": "ls .."}));
        denied("Bash", json!({"command": "cat ../../transcripts/att-1.jsonl"}));
        denied("Bash", json!({"command": format!("cat {}/scratch/x", store_dir.display())}));
        assert!(denied("Bash", json!({"command": "sqlite3 \"$INTERLOCK_DB\" .dump"})).contains("store"));
        // Symlinks do not hide where a path leads.
        std::os::unix::fs::symlink(&store_dir, s.worktree.join("src/link")).unwrap();
        denied("Bash", json!({"command": "cat src/link/config.toml"}));
        // Attempt tokens, wherever they are kept.
        let tokens = crate::sessions::token_dir(&store_dir);
        assert!(denied("Read", json!({"file_path": tokens.join("att-x.token")})).contains("tokens"));
        // Other processes' environments, where a running session's token lives.
        assert!(denied("Bash", json!({"command": "tr '\\0' '\\n' < /proc/4242/environ"})).contains("environments"));
        denied("Read", json!({"file_path": "/proc/self/environ"}));
        // The attempt's own worktree, though it sits inside .interlock/, is open.
        assert_eq!(pre(&s, "Read", json!({"file_path": s.worktree.join("src/a.rs")})).exit_code, 0);
        assert_eq!(pre(&s, "Bash", json!({"command": "cat src/a.rs ./README.md"})).exit_code, 0);
        assert_eq!(pre(&s, "Grep", json!({"pattern": "fn", "path": "."})).exit_code, 0);
    }

    #[test]
    fn workers_may_not_commit() {
        let s = setup(true);
        let r = pre(&s, "Bash", json!({"command": "git commit -am 'my fix'"}));
        assert_eq!(r.exit_code, 2, "{}", r.stderr);
        assert_eq!(pre(&s, "Bash", json!({"command": "git status"})).exit_code, 0);
    }

    #[test]
    fn a_verifier_is_held_when_its_assessment_is_weaker_than_the_criterion_needs() {
        let s = setup(true);
        let mut store = Store::open(&s.ctx.db).unwrap();
        let mut task = store.task("t1").unwrap();
        task.criteria[0].min_strength = interlock_schema::MinStrength::Observed;
        store
            .connection()
            .execute("UPDATE tasks SET record = ?1 WHERE id = 't1'", [serde_json::to_string(&task).unwrap()])
            .unwrap();
        let tree = git::worktree_tree(&s.worktree).unwrap();
        let worker = s.ctx.attempt_id.clone().unwrap();
        store
            .submit_result(
                interlock_store::SubmitResult {
                    attempt_id: worker,
                    token: s.token.clone(),
                    epoch: 1,
                    output_tree: tree.clone(),
                    changed_paths: vec![],
                    summary: "done".into(),
                    open_questions: vec![],
                    event_id: None,
                },
                Utc::now(),
            )
            .unwrap();
        let caps: CapabilitySet = [
            Capability::SessionStart,
            Capability::SessionCollect,
            Capability::SessionCancel,
            Capability::PerCallPolicy,
        ]
        .into_iter()
        .collect();
        let Started::Yes { attempt, token, .. } = store
            .start_attempt(
                StartAttempt {
                    task_id: "t1".into(),
                    role: Role::Verifier,
                    mode: Mode::Headless,
                    host: HostRef { host: "test".into(), version: "0".into() },
                    capabilities: caps,
                    profile: Profile::Conservative,
                    host_policy: HostPolicy::open(),
                    agent: None,
                    model: None,
                    worktree: Some(s.worktree.display().to_string()),
                },
                Utc::now(),
            )
            .unwrap()
        else {
            panic!("no verifier")
        };
        let assess = |store: &mut Store, strength: Strength| {
            store
                .add_evidence(
                    EvidenceKind::Assessment,
                    AddEvidence {
                        attempt_id: attempt.id.clone(),
                        token: token.clone(),
                        criterion_id: "tests".into(),
                        strength,
                        tree: tree.clone(),
                        environment: None,
                        evidence_refs: vec![],
                        note: None,
                        event_id: None,
                    },
                    Utc::now(),
                )
                .unwrap();
        };
        assess(&mut store, Strength::Tested);
        let ctx = HookContext { attempt_id: Some(attempt.id.clone()), tree: Some(tree.clone()), ..s.ctx.clone() };
        let payload = json!({"hook_event_name": "Stop", "stop_hook_active": false, "cwd": s.worktree});
        let held = handle(&ctx, &payload, Utc::now());
        assert!(
            held.stdout.contains("tests: you recorded `tested`, but it needs `observed` or stronger"),
            "the reason names the criterion and the gap: {}",
            held.stdout
        );
        assess(&mut store, Strength::Observed);
        assert_eq!(handle(&ctx, &payload, Utc::now()), HookResponse::allow());
    }

    #[test]
    fn everything_under_the_store_directory_but_worktrees_is_off_limits() {
        let s = setup(true);
        let dir = s.ctx.db.parent().unwrap().display().to_string();
        // The controller lock and the hooks plugin, by absolute path and from
        // the attempt's worktree (which lives two levels under the store dir).
        for cmd in [
            format!("rm {dir}/supervisor.lock"),
            "echo 999 > ../../supervisor.lock".to_string(),
            "cat ../../plugin/hooks/hooks.json".to_string(),
        ] {
            let r = pre(&s, "Bash", json!({ "command": cmd }));
            assert_eq!(r.exit_code, 2, "{cmd}");
            assert!(r.stderr.contains("interlock's own directory"), "{cmd}: {}", r.stderr);
        }
        let own = format!("cat {}/src/a.rs", s.worktree.display());
        assert_eq!(pre(&s, "Bash", json!({ "command": own })).exit_code, 0, "the attempt's own worktree is fine");
    }

    #[test]
    fn interactive_sessions_ask_instead_of_deny() {
        let s = setup(false);
        let r = pre(&s, "Bash", json!({"command": "git push origin fix"}));
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("\"permissionDecision\":\"ask\""));
    }

    #[test]
    fn a_missing_store_fails_closed() {
        let mut s = setup(true);
        s.ctx.db = PathBuf::from("/nonexistent/dir/state.db");
        assert_eq!(pre(&s, "Bash", json!({"command": "ls"})).exit_code, 2);
    }

    #[test]
    fn the_stop_guard_wants_a_claim_for_the_final_tree() {
        let s = setup(true);
        let held = stop(&s, false);
        assert!(held.stdout.contains("\"decision\":\"block\""));
        assert!(held.stdout.contains("record a claim for tests"), "{}", held.stdout);
        assert_eq!(stop(&s, true), HookResponse::allow(), "never holds twice in a row");

        // A claim for the current tree releases it; a later edit makes it stale again.
        std::fs::write(s.worktree.join("src/a.rs"), "fn a() { /* fixed */ }\n").unwrap();
        let tree = git::worktree_tree(&s.worktree).unwrap();
        let mut store = Store::open(&s.ctx.db).unwrap();
        store
            .add_evidence(
                EvidenceKind::Claim,
                AddEvidence {
                    attempt_id: s.ctx.attempt_id.clone().unwrap(),
                    token: s.token.clone(),
                    criterion_id: "tests".into(),
                    strength: Strength::Tested,
                    tree,
                    environment: None,
                    evidence_refs: vec!["cargo test".into()],
                    note: None,
                    event_id: None,
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(stop(&s, false), HookResponse::allow());
        std::fs::write(s.worktree.join("src/a.rs"), "fn a() { /* changed again */ }\n").unwrap();
        assert!(stop(&s, false).stdout.contains("block"), "the claim no longer matches the files");
    }

    #[test]
    fn the_stop_guard_also_wants_interlock_to_have_run_each_check() {
        let s = setup(true);
        let store = Store::open(&s.ctx.db).unwrap();
        let mut task = store.task("t1").unwrap();
        task.criteria[0].check = Some("true".into());
        store
            .connection()
            .execute("UPDATE tasks SET record = ?1 WHERE id = 't1'", [serde_json::to_string(&task).unwrap()])
            .unwrap();
        drop(store);
        let held = stop(&s, false);
        assert!(held.stdout.contains("interlock check run --criterion tests --attempt"), "{}", held.stdout);
        let mut store = Store::open(&s.ctx.db).unwrap();
        crate::checks::run_check(
            &mut store,
            crate::checks::CheckRequest {
                task_id: "t1",
                criterion_id: "tests",
                target: RunTarget::Output,
                attempt: Some((s.ctx.attempt_id.clone().unwrap(), s.token.clone())),
                dir: &s.worktree,
                scratch: &s.worktree.join("../../scratch"),
                timeout: std::time::Duration::from_secs(30),
            },
        )
        .unwrap();
        let still = stop(&s, false);
        assert!(!still.stdout.contains("check run"), "the run is recorded: {}", still.stdout);
        assert!(still.stdout.contains("record a claim for tests"), "{}", still.stdout);
    }
}
