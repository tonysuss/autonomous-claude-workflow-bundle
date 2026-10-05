//! The git plumbing the supervisor needs. None of it moves a branch or
//! touches the user's index: output trees are written through a temporary
//! index, and attempt commits are dangling until something lands them.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
#[error("git {args}: {message}")]
pub struct GitError {
    pub args: String,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, GitError>;

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        // Attempt commits need an identity even where none is configured.
        .env("GIT_AUTHOR_NAME", "interlock")
        .env("GIT_AUTHOR_EMAIL", "interlock@localhost")
        .env("GIT_COMMITTER_NAME", "interlock")
        .env("GIT_COMMITTER_EMAIL", "interlock@localhost")
        .output()
        .map_err(|e| GitError { args: args.join(" "), message: e.to_string() })?;
    if !out.status.success() {
        return Err(GitError {
            args: args.join(" "),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"], &[]).map(PathBuf::from)
}

pub fn head(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "HEAD"], &[])
}

/// The tree of everything in the worktree (tracked, changed and untracked,
/// minus ignored files), written through a temporary index.
pub fn worktree_tree(dir: &Path) -> Result<String> {
    let root = toplevel(dir)?;
    let index = git(&root, &["rev-parse", "--git-path", "index"], &[])?;
    let index = if Path::new(&index).is_absolute() { PathBuf::from(index) } else { root.join(index) };
    let tmp = std::env::temp_dir().join(format!("interlock-index-{}", uuid::Uuid::new_v4().simple()));
    if index.exists() {
        std::fs::copy(&index, &tmp).map_err(|e| GitError { args: "copy index".into(), message: e.to_string() })?;
    }
    let tmp_str = tmp.display().to_string();
    let env = [("GIT_INDEX_FILE", tmp_str.as_str())];
    let result = git(&root, &["add", "-A"], &env).and_then(|_| git(&root, &["write-tree"], &env));
    let _ = std::fs::remove_file(&tmp);
    result
}

/// A commit holding `tree`, with `parent` as its parent. No ref points at it.
pub fn commit_tree(repo: &Path, tree: &str, parent: Option<&str>, message: &str) -> Result<String> {
    let mut args = vec!["commit-tree", tree, "-m", message];
    if let Some(p) = parent {
        args.extend(["-p", p]);
    }
    git(repo, &args, &[])
}

pub fn tree_of(repo: &Path, commit: &str) -> Result<String> {
    git(repo, &["rev-parse", &format!("{commit}^{{tree}}")], &[])
}

pub fn worktree_add(repo: &Path, path: &Path, commit: &str) -> Result<()> {
    git(repo, &["worktree", "add", "--detach", "--force", &path.display().to_string(), commit], &[]).map(|_| ())
}

pub fn worktree_remove(repo: &Path, path: &Path) -> Result<()> {
    git(repo, &["worktree", "remove", "--force", &path.display().to_string()], &[]).map(|_| ())
}

/// Paths that differ between two trees or commits.
pub fn changed_paths(repo: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let out = git(repo, &["diff", "--name-only", from, to], &[])?;
    Ok(out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A repository with one commit holding `files`.
    pub fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"], &[]).unwrap();
        for (path, body) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        git(dir.path(), &["add", "-A"], &[]).unwrap();
        git(dir.path(), &["commit", "-q", "-m", "init"], &[]).unwrap();
        dir
    }

    #[test]
    fn output_trees_include_new_files_and_leave_the_index_alone() {
        let repo = repo_with(&[("a.txt", "a\n"), (".gitignore", "target/\n")]);
        let base = head(repo.path()).unwrap();
        let base_tree = tree_of(repo.path(), &base).unwrap();
        assert_eq!(worktree_tree(repo.path()).unwrap(), base_tree, "an unchanged worktree has the base tree");

        std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
        std::fs::create_dir_all(repo.path().join("target")).unwrap();
        std::fs::write(repo.path().join("target/out"), "ignored\n").unwrap();
        let tree = worktree_tree(repo.path()).unwrap();
        assert_ne!(tree, base_tree);
        assert_eq!(changed_paths(repo.path(), &base_tree, &tree).unwrap(), vec!["b.txt"]);
        let status = git(repo.path(), &["status", "--porcelain"], &[]).unwrap();
        assert!(status.contains("?? b.txt"), "b.txt is still untracked in the user's index: {status}");
    }

    #[test]
    fn attempt_worktrees_and_commits() {
        let repo = repo_with(&[("a.txt", "a\n")]);
        let base = head(repo.path()).unwrap();
        let wt = repo.path().join(".interlock/worktrees/att-1");
        worktree_add(repo.path(), &wt, &base).unwrap();
        std::fs::write(wt.join("a.txt"), "fixed\n").unwrap();
        let tree = worktree_tree(&wt).unwrap();
        let commit = commit_tree(repo.path(), &tree, Some(&base), "interlock att-1").unwrap();
        assert_eq!(tree_of(repo.path(), &commit).unwrap(), tree);
        assert_eq!(head(repo.path()).unwrap(), base, "no branch moved");
        worktree_remove(repo.path(), &wt).unwrap();
        assert!(!wt.exists());
    }
}
