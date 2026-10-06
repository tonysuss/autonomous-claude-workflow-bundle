//! What interlock's hooks plugin runs on each tool call and each stop. The
//! same code serves Claude Code and Copilot CLI. It fails closed: a governed
//! action is denied when the attempt cannot be loaded.

use std::path::{Path, PathBuf};

use interlock_adapter::hooks::{HookEvent, HookResponse, action_of, parse_event};
use interlock_core::evidence::{same_currency, same_tree};
use interlock_core::grants::{self, Verdict};
use interlock_core::scope::{Placement, place};
use interlock_schema::{AttemptStatus, Currency, Producer, Role, RunTarget, Timestamp};
use interlock_store::Store;
use serde_json::Value;

use crate::git;

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

fn pre_tool_use(
    ctx: &HookContext,
    attempt_id: &str,
    tool_name: &str,
    input: &Value,
    cwd: Option<PathBuf>,
) -> HookResponse {
    let action = action_of(tool_name, input);
    if let Some(cmd) = &action.command {
        let db = ctx.db.display().to_string();
        if cmd.contains(&db) || cmd.contains("state.db") || touches_store_dir(cmd, &ctx.db) {
            return HookResponse::deny("interlock's store is off limits; report through the interlock command");
        }
    }
    if !action.governed {
        return HookResponse::allow();
    }
    let loaded = Store::open(&ctx.db).and_then(|s| {
        let attempt = s.attempt(attempt_id)?;
        let task = s.task(&attempt.task_id)?;
        Ok((attempt, task))
    });
    let (attempt, task) = match loaded {
        Ok(v) => v,
        Err(e) => return HookResponse::deny(&format!("interlock could not load attempt {attempt_id}: {e}")),
    };
    if !matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted) {
        return HookResponse::deny(&format!("attempt {attempt_id} is no longer active ({:?})", attempt.status));
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

/// Whether a command names interlock's own directory: the store, the
/// controller lock, the hooks plugin. Attempt worktrees under it are fine.
/// A string check, not a sandbox: a path built at run time gets past it.
fn touches_store_dir(cmd: &str, db: &Path) -> bool {
    let mut needles = vec![".interlock".to_string()];
    if let Some(dir) = db.parent().map(|d| d.display().to_string()).filter(|d| !d.is_empty()) {
        needles.push(dir);
    }
    needles.iter().any(|n| cmd.match_indices(n.as_str()).any(|(i, _)| !cmd[i + n.len()..].starts_with("/worktrees/")))
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
    if missing_records.is_empty() && missing_runs.is_empty() {
        return HookResponse::allow();
    }
    let mut steps = Vec::new();
    if !missing_runs.is_empty() {
        steps.push(format!(
            "have interlock run the check on your current files for: {} (interlock check run --criterion <id>)",
            missing_runs.join(", ")
        ));
    }
    if !missing_records.is_empty() {
        steps.push(match attempt.role {
            Role::Worker => format!(
                "record a claim for: {} (interlock claim add --criterion <id> --strength \
                 <observed|tested|static|failed|blocked> --tree auto --ref \"<command>\" --note \"<what you saw>\")",
                missing_records.join(", ")
            ),
            _ => format!(
                "record an assessment for: {} (interlock assess add --criterion <id> --strength \
                 <observed|tested|static|failed|blocked> --ref \"<command>\" --note \"<what you saw>\")",
                missing_records.join(", ")
            ),
        });
    }
    let reason =
        format!("Before you finish, {}. Evidence made before your last edit no longer counts.", steps.join(", then "));
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
    fn everything_under_the_store_directory_but_worktrees_is_off_limits() {
        let s = setup(true);
        let dir = s.ctx.db.parent().unwrap().display().to_string();
        for cmd in [
            format!("rm {dir}/supervisor.lock"),
            "echo 999 > .interlock/supervisor.lock".to_string(),
            "cat ../../.interlock/plugin/hooks/hooks.json".to_string(),
        ] {
            let r = pre(&s, "Bash", json!({ "command": cmd }));
            assert_eq!(r.exit_code, 2, "{cmd}");
            assert!(r.stderr.contains("store is off limits"), "{cmd}: {}", r.stderr);
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
        assert!(held.stdout.contains("record a claim for: tests"), "{}", held.stdout);
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
        assert!(held.stdout.contains("interlock check run --criterion <id>"), "{}", held.stdout);
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
        assert!(still.stdout.contains("record a claim for: tests"), "{}", still.stdout);
    }
}
