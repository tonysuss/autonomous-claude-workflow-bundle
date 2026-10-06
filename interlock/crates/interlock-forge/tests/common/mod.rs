//! A verified task in a real git repository, the real `GhForge`, a fake `gh`
//! and a local bare repository standing in for GitHub. No network.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chrono::Utc;
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::grants::{HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, TaskSpec};
use interlock_core::workflow::Mode;
use interlock_forge::testing::{FakeGh, Remote, python3_available};
use interlock_forge::{DeliverConfig, Delivery, GhForge, Reconciled, git, integrate, reconcile};
use interlock_schema::*;
use interlock_store::*;

pub const TASK: &str = "fix-add";
pub const BUGGY: &str = "def add(a, b):\n    return a - b\n";
pub const FIXED: &str = "def add(a, b):\n    return a + b\n";

pub fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
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

/// Makes every commit in `repo` try to sign with a program that always
/// fails, unless the commit asks not to be signed.
pub fn trap_signing(repo: &Path) {
    for (k, v) in [
        ("commit.gpgsign", "true"),
        ("gpg.format", "openpgp"),
        ("gpg.program", "false"),
        ("gpg.ssh.program", "false"),
        ("user.signingkey", "nobody"),
    ] {
        git(repo, &["config", k, v], &[]);
    }
}

/// A tree like `commit`'s with `path` = `content`, written through a temporary index.
pub fn tree_with(repo: &Path, commit: &str, path: &str, content: &str) -> String {
    let index = repo.join(".git/interlock-test-index");
    let index = index.display().to_string();
    let env = [("GIT_INDEX_FILE", index.as_str())];
    git(repo, &["read-tree", commit], &env);
    let file = repo.join(".git/blob.tmp");
    std::fs::write(&file, content).unwrap();
    let blob = git(repo, &["hash-object", "-w", &file.display().to_string()], &[]);
    git(repo, &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], &env);
    git(repo, &["write-tree"], &env)
}

fn caps() -> CapabilitySet {
    [Capability::SessionStart, Capability::SessionCollect, Capability::SessionCancel, Capability::ToolRestriction]
        .into_iter()
        .collect()
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub repo: PathBuf,
    pub remote: Remote,
    pub gh: FakeGh,
    pub forge: GhForge,
    pub store: Store,
    pub base: String,
    pub cfg: DeliverConfig,
}

impl Fixture {
    pub fn new() -> Option<Fixture> {
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
        trap_signing(&repo);
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
            "scope": {"paths": ["calc.py"]},
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
        let cfg = DeliverConfig {
            wait: Duration::ZERO,
            poll: Duration::from_millis(10),
            checks_settle: Duration::ZERO,
            ..DeliverConfig::default()
        };
        Some(Fixture { dir, repo, remote, gh, forge, store, base, cfg })
    }

    pub fn tree_with(&self, commit: &str, path: &str, content: &str) -> String {
        tree_with(&self.repo, commit, path, content)
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
    pub fn verified(&mut self) -> String {
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

    /// Fresh evidence for `tree` from a new verifier.
    pub fn reverify(&mut self, tree: &str) {
        let v = self.start(Role::Verifier);
        self.evidence(EvidenceKind::Assessment, &v, "fixed", Strength::Tested, tree);
        self.evidence(EvidenceKind::Assessment, &v, "verified", Strength::Observed, tree);
        self.store.advance(TASK, Utc::now()).unwrap();
        assert_eq!(self.state(), State::Verified, "{:?}", self.store.evaluate(TASK).unwrap().1.missing());
    }

    pub fn grant(&mut self, authority: LandingAuthority) -> Grant {
        self.store
            .create_grant(
                NewGrant {
                    principal: "operator".into(),
                    task_scope: vec![TASK.into()],
                    action_classes: vec![ActionClass::Landing],
                    tools: ToolPolicy::default(),
                    landing_authority: authority,
                    origin: "land the add fix once it is verified".into(),
                    expires_at: None,
                },
                Utc::now(),
            )
            .unwrap()
    }

    /// Landing authority under which interlock merges itself.
    pub fn grant_landing(&mut self) -> Grant {
        self.grant(LandingAuthority::Coordinator)
    }

    pub fn integrate(&mut self) -> Delivery {
        integrate(&mut self.store, &self.repo, &self.forge, &self.cfg, TASK, &AtomicBool::new(false)).unwrap()
    }

    pub fn reconcile(&mut self) -> Vec<Reconciled> {
        reconcile(&mut self.store, &self.repo, &self.forge, Some(TASK)).unwrap()
    }

    pub fn state(&self) -> State {
        self.store.task(TASK).unwrap().state
    }

    pub fn blocked_reason(&self) -> String {
        self.store.task(TASK).unwrap().blocked_reason.unwrap_or_default()
    }

    pub fn signals(&self) -> Vec<String> {
        self.store.transitions(TASK).unwrap().into_iter().map(|t| t.signal).collect()
    }

    pub fn ops(&self) -> Vec<(OperationKind, OperationState)> {
        self.store.operations(TASK).unwrap().iter().map(|o| (o.kind, o.state)).collect()
    }

    pub fn op(&self, kind: OperationKind) -> Operation {
        self.store.operations(TASK).unwrap().into_iter().rev().find(|o| o.kind == kind).unwrap()
    }

    pub fn head(&self) -> String {
        git::verified_head(&self.repo, &self.store.task(TASK).unwrap()).unwrap()
    }

    /// The refs interlock keeps for the task.
    pub fn refs(&self) -> String {
        git(&self.repo, &["for-each-ref", "--format=%(refname)", "refs/interlock/"], &[])
    }
}
