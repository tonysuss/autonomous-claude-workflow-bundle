//! Test support, not API: a stateful fake `gh` (a Python script) and a local
//! bare repository standing in for GitHub's copy of a repository. Nothing
//! here touches the network.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// The fake `gh`. See the script's docstring for its state and fault modes.
pub const FAKE_GH: &str = include_str!("fake_gh.py");

/// One call's arguments, and the operations (id, kind, state) committed when it arrived.
pub type SeenCall = (Vec<String>, Vec<(String, String, String)>);

pub fn python3_available() -> bool {
    Command::new("python3").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .env("GIT_AUTHOR_NAME", "someone")
        .env("GIT_AUTHOR_EMAIL", "someone@localhost")
        .env("GIT_COMMITTER_NAME", "someone")
        .env("GIT_COMMITTER_EMAIL", "someone@localhost")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare repository acting as GitHub's copy, added as the `origin` remote
/// of a working repository.
pub struct Remote {
    pub path: PathBuf,
}

impl Remote {
    /// Creates the bare repository at `path`, adds it to `repo` as `origin`,
    /// and pushes `repo`'s HEAD as `main`.
    pub fn create(path: &Path, repo: &Path) -> Remote {
        git(repo, &["init", "-q", "--bare", "-b", "main", &path.display().to_string()], &[]);
        git(repo, &["remote", "add", "origin", &path.display().to_string()], &[]);
        git(repo, &["push", "-q", "origin", "HEAD:refs/heads/main"], &[]);
        Remote { path: path.to_path_buf() }
    }

    fn git(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let dir = self.path.display().to_string();
        let mut all = vec!["--git-dir", dir.as_str()];
        all.extend(args);
        git(&self.path, &all, env)
    }

    pub fn tip(&self, branch: &str) -> Option<String> {
        let dir = self.path.display().to_string();
        let out = Command::new("git")
            .args(["--git-dir", &dir, "rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
            .output()
            .ok()?;
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!sha.is_empty()).then_some(sha)
    }

    /// `git show <rev>` in the bare repository.
    pub fn show(&self, rev: &str) -> String {
        self.git(&["show", "--no-patch", "--format=%H %P", rev], &[])
    }

    pub fn file(&self, rev: &str, path: &str) -> String {
        self.git(&["show", &format!("{rev}:{path}")], &[])
    }

    /// Someone else commits `path` = `content` on top of `branch` and pushes it.
    pub fn commit_file(&self, branch: &str, path: &str, content: &str) -> String {
        let parent = self.tip(branch).expect("branch exists");
        let index = self.path.join(format!("index-{}", std::process::id()));
        let index_str = index.display().to_string();
        let env = [("GIT_INDEX_FILE", index_str.as_str())];
        self.git(&["read-tree", &parent], &env);
        let blob_file = self.path.join("blob.tmp");
        std::fs::write(&blob_file, content).unwrap();
        let blob = self.git(&["hash-object", "-w", &blob_file.display().to_string()], &[]);
        self.git(&["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], &env);
        let tree = self.git(&["write-tree"], &env);
        let commit = self.git(&["commit-tree", &tree, "-p", &parent, "-m", &format!("someone changes {path}")], &[]);
        self.git(&["update-ref", &format!("refs/heads/{branch}"), &commit, &parent], &[]);
        let _ = std::fs::remove_file(index);
        let _ = std::fs::remove_file(blob_file);
        commit
    }
}

/// The fake `gh`, installed as an executable script with its state file.
pub struct FakeGh {
    pub bin: PathBuf,
    pub state: PathBuf,
}

impl FakeGh {
    pub fn install(dir: &Path, remote: &Remote) -> FakeGh {
        std::fs::create_dir_all(dir).unwrap();
        let bin = dir.join("gh");
        std::fs::write(&bin, FAKE_GH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let state = dir.join("gh-state.json");
        let initial = json!({ "remote": remote.path, "prs": [], "checks": "pass", "faults": {}, "calls": [] });
        std::fs::write(&state, serde_json::to_string_pretty(&initial).unwrap()).unwrap();
        FakeGh { bin, state }
    }

    /// Environment that points interlock at this fake.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![("INTERLOCK_GH_BIN".into(), self.bin.display().to_string())]
    }

    /// Records, at each call, the operation rows committed in the store at `db`.
    pub fn watch_store(&self, db: &Path) {
        self.update(|v| v["db"] = json!(db));
    }

    /// For each call, the operations the store held when it arrived.
    pub fn seen(&self) -> Vec<SeenCall> {
        let v = self.read();
        v["seen"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let call = serde_json::from_value(s["call"].clone()).unwrap_or_default();
                let ops = serde_json::from_value(s["operations"].clone()).unwrap_or_default();
                (call, ops)
            })
            .collect()
    }

    pub fn read(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.state).unwrap()).unwrap()
    }

    pub fn update(&self, f: impl FnOnce(&mut Value)) {
        let mut v = self.read();
        f(&mut v);
        std::fs::write(&self.state, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    /// Sets a fault on one subcommand; a negative `times` never runs out.
    pub fn fault(&self, on: &str, mode: &str, times: i64) {
        self.update(|v| v["faults"][on] = json!({ "mode": mode, "times": times }));
    }

    pub fn clear_faults(&self) {
        self.update(|v| v["faults"] = json!({}));
    }

    /// Sets one top-level state key, such as `lag` or `base_oid`.
    pub fn set(&self, key: &str, value: Value) {
        self.update(|v| v[key] = value);
    }

    /// Runs the fake directly, as someone else using `gh` would: an operator
    /// merging, or just time passing for queued merges.
    pub fn gh(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.bin).args(args).output().expect("the fake gh runs")
    }

    pub fn checks(&self, status: &str) {
        self.update(|v| v["checks"] = json!(status));
    }

    /// Every call so far, as argument lists.
    pub fn calls(&self) -> Vec<Vec<String>> {
        serde_json::from_value(self.read()["calls"].clone()).unwrap_or_default()
    }

    /// Calls whose arguments start with `prefix`, for example `["pr", "merge"]`.
    pub fn calls_to(&self, prefix: &[&str]) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.iter().zip(prefix).all(|(a, b)| a == b) && c.len() >= prefix.len())
            .collect()
    }
}
