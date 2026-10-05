//! Delivery through the real `GhForge` against a fake `gh` and a local bare
//! repository standing in for GitHub. Deterministic, no network. They skip
//! when python3 (which runs the fake) is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chrono::Utc;
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::delivery::UNKNOWN_OUTCOME;
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, TaskSpec};
use interlock_core::workflow::Mode;
use interlock_forge::testing::{FakeGh, Remote, python3_available};
use interlock_forge::{DeliverConfig, Delivery, GhForge, git, integrate, reconcile};
use interlock_schema::*;
use interlock_store::*;

const TASK: &str = "fix-add";
const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn caps() -> CapabilitySet {
    [Capability::SessionStart, Capability::SessionCollect, Capability::SessionCancel, Capability::ToolRestriction]
        .into_iter()
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    remote: Remote,
    gh: FakeGh,
    forge: GhForge,
    store: Store,
    base: String,
    cfg: DeliverConfig,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        if !python3_available() {
            eprintln!("skipping: the fake gh needs python3");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"], &[]);
        std::fs::write(repo.join("calc.py"), BUGGY).unwrap();
        git(&repo, &["add", "-A"], &[]);
        git(&repo, &["commit", "-q", "-m", "init"], &[]);
        let base = git(&repo, &["rev-parse", "HEAD"], &[]);
        let remote = Remote::create(&dir.path().join("remote.git"), &repo);
        let gh = FakeGh::install(&dir.path().join("gh"), &remote);
        let db = dir.path().join("state.db");
        gh.watch_store(&db);
        let forge = GhForge {
            gh: gh.bin.clone(),
            dir: repo.clone(),
            remote: "origin".into(),
            repo: None,
            timeout: Duration::from_secs(30),
        };
        let mut store = Store::open(&db).unwrap();
        let spec: TaskSpec = serde_json::from_value(serde_json::json!({
            "id": TASK,
            "repository": repo,
            "workflow": "bug-fix",
            "intent": "add() returns the difference instead of the sum",
            "integration_required": true,
            "criteria": [
                {"id": "fixed", "statement": "add(2, 3) returns 5", "min_strength": "tested", "producer": "self"},
                {"id": "verified", "statement": "An independent check passes",
                 "min_strength": "observed", "producer": "independent"}
            ]
        }))
        .unwrap();
        store.create_task(spec, Utc::now()).unwrap();
        let snapshot = Snapshot {
            repository: repo.display().to_string(),
            base_commit: base.clone(),
            untracked_hash: None,
            protected_paths: vec![],
        };
        store.ready(TASK, snapshot, Utc::now()).unwrap();
        let cfg = DeliverConfig { wait: Duration::ZERO, poll: Duration::from_millis(10), ..DeliverConfig::default() };
        Some(Fixture { _dir: dir, repo, remote, gh, forge, store, base, cfg })
    }

    /// A tree like `commit`'s with `path` = `content`, written through a temporary index.
    fn tree_with(&self, commit: &str, path: &str, content: &str) -> String {
        let index = self.repo.join(".git/interlock-test-index");
        let index = index.display().to_string();
        let env = [("GIT_INDEX_FILE", index.as_str())];
        git(&self.repo, &["read-tree", commit], &env);
        let file = self.repo.join(".git/blob.tmp");
        std::fs::write(&file, content).unwrap();
        let blob = git(&self.repo, &["hash-object", "-w", &file.display().to_string()], &[]);
        git(&self.repo, &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], &env);
        git(&self.repo, &["write-tree"], &env)
    }

    fn start(&mut self, role: Role) -> (String, String, u32) {
        let started = self
            .store
            .start_attempt(
                StartAttempt {
                    task_id: TASK.into(),
                    role,
                    mode: Mode::Headless,
                    host: HostRef { host: "test".into(), version: "0".into() },
                    capabilities: caps(),
                    profile: Profile::Conservative,
                    host_policy: HostPolicy::open(),
                    agent: None,
                    model: None,
                    worktree: None,
                },
                Utc::now(),
            )
            .unwrap();
        match started {
            Started::Yes { attempt, token, .. } => (attempt.id, token, attempt.epoch),
            Started::Blocked { moved } => panic!("blocked: {}", moved.reason),
        }
    }

    fn evidence(
        &mut self,
        kind: EvidenceKind,
        attempt: &(String, String, u32),
        criterion: &str,
        s: Strength,
        tree: &str,
    ) {
        self.store
            .add_evidence(
                kind,
                AddEvidence {
                    attempt_id: attempt.0.clone(),
                    token: attempt.1.clone(),
                    criterion_id: criterion.into(),
                    strength: s,
                    tree: tree.into(),
                    environment: None,
                    evidence_refs: vec![],
                    note: None,
                    event_id: None,
                },
                Utc::now(),
            )
            .unwrap();
    }

    /// A worker submits the fixed tree, and a verifier passes it: the task is verified.
    fn verified(&mut self) -> String {
        let tree = self.tree_with(&self.base.clone(), "calc.py", FIXED);
        let w = self.start(Role::Worker);
        self.store
            .submit_result(
                SubmitResult {
                    attempt_id: w.0.clone(),
                    token: w.1.clone(),
                    epoch: w.2,
                    output_tree: tree.clone(),
                    changed_paths: vec!["calc.py".into()],
                    summary: "fixed add".into(),
                    open_questions: vec![],
                    event_id: None,
                },
                Utc::now(),
            )
            .unwrap();
        self.evidence(EvidenceKind::Claim, &w, "fixed", Strength::Tested, &tree);
        self.reverify(&tree);
        tree
    }

    /// Fresh evidence for `tree` from a worker claim on record and a new verifier.
    fn reverify(&mut self, tree: &str) {
        let v = self.start(Role::Verifier);
        self.evidence(EvidenceKind::Assessment, &v, "fixed", Strength::Tested, tree);
        self.evidence(EvidenceKind::Assessment, &v, "verified", Strength::Observed, tree);
        self.store.advance(TASK, Utc::now()).unwrap();
        assert_eq!(self.state(), State::Verified, "{:?}", self.store.evaluate(TASK).unwrap().1.missing());
    }

    fn grant_landing(&mut self) -> Grant {
        self.store
            .create_grant(
                NewGrant {
                    principal: "operator".into(),
                    task_scope: vec![TASK.into()],
                    action_classes: vec![ActionClass::Landing],
                    tools: ToolPolicy::default(),
                    landing_authority: LandingAuthority::Operator,
                    origin: "land the add fix once it is verified".into(),
                    expires_at: None,
                },
                Utc::now(),
            )
            .unwrap()
    }

    fn integrate(&mut self) -> Delivery {
        integrate(&mut self.store, &self.repo, &self.forge, &self.cfg, TASK, &AtomicBool::new(false)).unwrap()
    }

    fn state(&self) -> State {
        self.store.task(TASK).unwrap().state
    }

    fn signals(&self) -> Vec<String> {
        self.store.transitions(TASK).unwrap().into_iter().map(|t| t.signal).collect()
    }

    fn ops(&self) -> Vec<(OperationKind, OperationState)> {
        self.store.operations(TASK).unwrap().iter().map(|o| (o.kind, o.state)).collect()
    }

    fn head(&self) -> String {
        git::verified_head(&self.repo, &self.store.task(TASK).unwrap()).unwrap()
    }
}

#[test]
fn a_verified_task_lands_only_at_its_verified_head() {
    let Some(mut f) = Fixture::new() else { return };
    let tree = f.verified();
    f.grant_landing();
    let d = f.integrate();
    assert_eq!(d.final_state, State::Done, "{d:#?}");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G5", "G6"]);

    // The verified head is the verified tree on the snapshot base.
    let head = d.head.clone().unwrap();
    assert_eq!(git::tree_of(&f.repo, &head).unwrap(), tree);
    assert_eq!(git(&f.repo, &["rev-parse", &format!("{head}^")], &[]), f.base);
    // It was merged pinned to that head, and only that head reached main.
    let merges = f.gh.calls_to(&["pr", "merge"]);
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0], ["pr", "merge", "1", "--merge", "--match-head-commit", head.as_str()]);
    let main = f.remote.tip("main").unwrap();
    assert_eq!(f.remote.show(&main), format!("{main} {} {head}", f.base), "main is a merge of the verified head");
    assert_eq!(f.remote.file("main", "calc.py"), FIXED.trim_end());
    assert_eq!(f.remote.tip("interlock/fix-add").as_deref(), Some(head.as_str()));
    // Both operations were confirmed; the user's branch and worktree never moved.
    assert_eq!(
        f.ops(),
        [(OperationKind::Merge, OperationState::Confirmed), (OperationKind::OpenPr, OperationState::Confirmed)]
    );
    assert_eq!(git(&f.repo, &["rev-parse", "main"], &[]), f.base);
    assert_eq!(std::fs::read_to_string(f.repo.join("calc.py")).unwrap(), BUGGY);
}

#[test]
fn every_forge_call_finds_its_operation_already_started() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    assert_eq!(f.integrate().final_state, State::Done);
    let seen = f.gh.seen();
    let at = |prefix: &[&str]| {
        seen.iter()
            .find(|(call, _)| call.iter().zip(prefix).all(|(a, b)| a == b))
            .map(|(_, ops)| ops.iter().map(|(_, kind, state)| (kind.clone(), state.clone())).collect::<Vec<_>>())
            .unwrap_or_else(|| panic!("no {prefix:?} call: {seen:?}"))
    };
    // The landing operation is planned at G5, before anything reaches the forge.
    let first = &seen[0].1;
    assert_eq!(first[0].1, "merge");
    assert_eq!(first[0].2, "planned");
    // Opening the pull request (after the push) runs under a started open_pr row.
    assert_eq!(at(&["pr", "create"]), [("merge".into(), "planned".into()), ("open_pr".into(), "started".into())]);
    // The merge call finds its own row started, committed before the call.
    assert_eq!(at(&["pr", "merge"]), [("merge".into(), "started".into()), ("open_pr".into(), "confirmed".into())]);
}

#[test]
fn without_landing_authority_the_task_blocks_at_verified_and_the_forge_is_untouched() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked);
    let task = f.store.task(TASK).unwrap();
    assert_eq!(task.blocked_reason.as_deref(), Some("no landing authority is granted for this task"));
    assert_eq!(task.resume_point, Some(State::Verified));
    assert!(f.gh.calls().is_empty(), "no gh call was made: {:?}", f.gh.calls());
    assert_eq!(f.remote.tip("interlock/fix-add"), None, "nothing was pushed");
    assert!(f.store.operations(TASK).unwrap().is_empty());
}

#[test]
fn a_head_that_moves_during_the_merge_is_refused_and_r2_sends_the_task_back() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.fault("merge", "move_head", 1);
    let d = f.integrate();
    assert_eq!(d.final_state, State::AwaitingVerification, "{d:#?}");
    assert_eq!(f.signals().last().map(String::as_str), Some("R2"));
    let head = d.head.unwrap();
    assert_eq!(f.gh.calls_to(&["pr", "merge"])[0][4..], ["--match-head-commit".to_string(), head.clone()]);
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()), "nothing reached main");
    let merge_op = f.store.operations(TASK).unwrap().into_iter().find(|o| o.kind == OperationKind::Merge).unwrap();
    assert_eq!(merge_op.state, OperationState::Failed);

    // The evidence still covers the verified tree, but the branch now holds a
    // commit nobody verified: interlock will not overwrite it.
    f.store.advance(TASK, Utc::now()).unwrap();
    assert_eq!(f.state(), State::Verified);
    let moved = f.remote.tip("interlock/fix-add").unwrap();
    assert_ne!(moved, head);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    assert!(d.stopped_because.contains("which interlock did not push"), "{}", d.stopped_because);
    assert_eq!(f.remote.tip("interlock/fix-add"), Some(moved), "the unverified commit was left alone");
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()));
}

#[test]
fn a_changed_base_invalidates_the_evidence_and_the_rebased_tree_lands_after_reverification() {
    let Some(mut f) = Fixture::new() else { return };
    let tree = f.verified();
    f.grant_landing();
    let old_head = f.head();
    // Someone merges other work into main after verification.
    let moved_base = f.remote.commit_file("main", "README.md", "other work\n");

    let d = f.integrate();
    assert_eq!(d.final_state, State::AwaitingVerification, "{d:#?}");
    assert_eq!(f.signals().last().map(String::as_str), Some("R2"));
    let task = f.store.task(TASK).unwrap();
    let rebased = task.current_tree.clone().unwrap();
    assert_ne!(rebased, tree, "the tree under evidence is now the one that would land");
    assert_eq!(task.input_snapshot.as_ref().unwrap().base_commit, moved_base);
    let (_, report) = f.store.evaluate(TASK).unwrap();
    assert!(!report.all_pass, "evidence about the old tree no longer counts");
    assert!(report.criteria.iter().all(|c| c.stale > 0 && c.current_claims + c.current_assessments == 0));
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty(), "no merge was attempted");
    assert_eq!(f.remote.tip("main"), Some(moved_base.clone()));

    // Verification on the rebased tree, then the new head lands on the new base.
    f.reverify(&rebased);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Done, "{d:#?}");
    let head = d.head.unwrap();
    assert_ne!(head, old_head);
    assert_eq!(git(&f.repo, &["rev-parse", &format!("{head}^")], &[]), moved_base);
    assert_eq!(git::tree_of(&f.repo, &head).unwrap(), rebased);
    let main = f.remote.tip("main").unwrap();
    assert_eq!(f.remote.show(&main), format!("{main} {moved_base} {head}"));
    assert_eq!(f.remote.file("main", "calc.py"), FIXED.trim_end());
    assert_eq!(f.remote.file("main", "README.md"), "other work");
    assert_eq!(f.signals(), ["G1", "G2", "G3", "G4", "G5", "R2", "G4", "G5", "G6"]);
}

#[test]
fn pending_checks_hold_the_merge_until_they_pass() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("pending");
    let d = f.integrate();
    assert_eq!(d.final_state, State::Integrating);
    assert!(d.stopped_because.starts_with("waiting for the forge: checks pending"), "{}", d.stopped_because);
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty());
    assert_eq!(
        f.ops(),
        [(OperationKind::Merge, OperationState::Planned), (OperationKind::OpenPr, OperationState::Confirmed)]
    );

    f.gh.checks("pass");
    let d = f.integrate();
    assert_eq!(d.final_state, State::Done, "{d:#?}");
    assert_eq!(f.gh.calls_to(&["pr", "create"]).len(), 1, "the pull request was reused");
}

#[test]
fn failing_checks_block_the_task_with_the_reason() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("fail");
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked);
    assert_eq!(f.store.task(TASK).unwrap().blocked_reason.as_deref(), Some("checks failed on pull request #1: ci"));
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty());
}

#[test]
fn a_merge_refused_by_policy_blocks_with_what_the_forge_said() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.fault("merge", "refuse", 1);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked);
    let reason = f.store.task(TASK).unwrap().blocked_reason.unwrap();
    assert!(reason.contains("the base branch policy prohibits the merge"), "{reason}");
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()));
}

#[test]
fn a_merge_call_that_dies_unanswered_is_settled_from_what_the_forge_shows() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.fault("merge", "die", 1);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Done, "the merge happened, whatever the call said: {d:#?}");
    assert_eq!(f.signals().last().map(String::as_str), Some("G6"));
}

#[test]
fn an_unknown_outcome_blocks_the_task_until_reconcile_can_tell() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    // The merge goes through, then the forge stops answering.
    f.gh.fault("merge", "vanish", 1);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    let task = f.store.task(TASK).unwrap();
    assert!(task.blocked_reason.as_deref().unwrap().starts_with(UNKNOWN_OUTCOME), "{:?}", task.blocked_reason);
    assert_eq!(task.resume_point, Some(State::Integrating));
    let merge = f.store.operations(TASK).unwrap().into_iter().find(|o| o.kind == OperationKind::Merge).unwrap();
    assert_eq!(merge.state, OperationState::Unknown);

    // Still unreachable: reconcile changes nothing.
    let r = reconcile(&mut f.store, &f.forge, None).unwrap();
    assert_eq!((r[0].before, r[0].after), (OperationState::Unknown, OperationState::Unknown));
    assert_eq!(f.state(), State::Blocked);

    // The forge answers again: the merged head is found, the block lifts, and G6 applies.
    f.gh.clear_faults();
    let r = reconcile(&mut f.store, &f.forge, Some(TASK)).unwrap();
    assert_eq!((r[0].before, r[0].after), (OperationState::Unknown, OperationState::Confirmed));
    assert_eq!(f.state(), State::Done);
    let tail: Vec<String> = f.signals().into_iter().rev().take(3).collect();
    assert_eq!(tail, ["G6", "unblock", "block"]);
}

#[test]
fn revoking_landing_authority_stops_the_merge_before_the_call() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    let grant = f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.store.revoke_grant(&grant.id, Utc::now()).unwrap();
    f.gh.checks("pass");
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    let reason = f.store.task(TASK).unwrap().blocked_reason.unwrap();
    assert!(reason.starts_with("landing authority is no longer granted"), "{reason}");
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty(), "the core refused before the call");
    let merge = f.store.operations(TASK).unwrap().into_iter().find(|o| o.kind == OperationKind::Merge).unwrap();
    assert_eq!(merge.state, OperationState::Failed);
}

#[test]
fn auto_merge_is_armed_at_the_verified_head_and_confirmed_when_the_forge_merges() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.cfg.auto_merge = true;
    f.gh.checks("pending");
    let d = f.integrate();
    assert_eq!(d.final_state, State::Integrating, "{d:#?}");
    let head = d.head.unwrap();
    let call = &f.gh.calls_to(&["pr", "merge"])[0];
    assert_eq!(call[3..], ["--merge", "--match-head-commit", head.as_str(), "--auto"]);
    let arm = f.store.operations(TASK).unwrap().into_iter().find(|o| o.kind == OperationKind::ArmAutoMerge).unwrap();
    assert_eq!(arm.state, OperationState::Started, "armed and in flight");

    // Checks pass and the forge merges on its own; reconcile confirms it.
    f.gh.checks("pass");
    let r = reconcile(&mut f.store, &f.forge, Some(TASK)).unwrap();
    assert_eq!(r[0].after, OperationState::Confirmed);
    assert_eq!(f.state(), State::Done);
}

#[test]
fn the_verified_head_is_the_same_commit_every_time_and_moves_no_branch() {
    let Some(mut f) = Fixture::new() else { return };
    let tree = f.verified();
    let before = git(&f.repo, &["for-each-ref", "refs/heads"], &[]);
    let a = f.head();
    let b = f.head();
    assert_eq!(a, b, "a restarted controller rebuilds the head it pinned");
    assert_eq!(git::tree_of(&f.repo, &a).unwrap(), tree);
    assert!(!git(&f.repo, &["cat-file", "commit", &a], &[]).contains("gpgsig"), "never signed");
    assert_eq!(git(&f.repo, &["rev-parse", "refs/interlock/fix-add/head"], &[]), a);
    assert_eq!(git(&f.repo, &["for-each-ref", "refs/heads"], &[]), before);
}
