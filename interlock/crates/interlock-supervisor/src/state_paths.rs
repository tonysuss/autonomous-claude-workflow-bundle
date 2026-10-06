//! interlock's own state, everything under the repository's `.interlock/`,
//! is off limits to the agents it governs: to edit tools, and to shell
//! commands that name it. An attempt's own worktree, which lives there too,
//! is the one exception.

use std::path::{Component, Path, PathBuf};

/// `path` made absolute against `base` with `.` and `..` removed, then with
/// its deepest existing ancestor resolved through any symlinks.
pub fn normalize(path: &Path, base: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    let mut lexical = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            other => lexical.push(other.as_os_str()),
        }
    }
    let mut existing = lexical.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return lexical,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    out.extend(rest.iter().rev());
    out
}

/// Whether `path` (already normalized) is interlock state: inside
/// `interlock_dir` and not inside the attempt's own worktree.
pub fn is_interlock_state_path(path: &Path, interlock_dir: &Path, own_worktree: Option<&Path>) -> bool {
    let dir = normalize(interlock_dir, Path::new("/"));
    if !path.starts_with(&dir) {
        return false;
    }
    !own_worktree.is_some_and(|w| path.starts_with(normalize(w, Path::new("/"))))
}

/// The words of a shell command that could be paths: split on whitespace and
/// shell operators, quotes dropped, and the value side of `name=value` kept.
fn words(command: &str) -> Vec<String> {
    let spaced: String = command
        .chars()
        .map(|c| if matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>' | '`' | '\n' | '"' | '\'') { ' ' } else { c })
        .collect();
    let mut out = Vec::new();
    for w in spaced.split_whitespace() {
        out.push(w.to_string());
        if let Some((_, value)) = w.split_once('=') {
            if !value.is_empty() {
                out.push(value.to_string());
            }
        }
    }
    out
}

/// The words of `command` that name interlock state, resolved against the
/// command's directory, the repository root, and any `cd` target in it.
pub fn state_paths_in_command(
    command: &str,
    cwd: &Path,
    interlock_dir: &Path,
    own_worktree: Option<&Path>,
) -> Vec<String> {
    let words = words(command);
    let repo = interlock_dir.parent().unwrap_or(Path::new("/"));
    let mut bases = vec![cwd.to_path_buf(), repo.to_path_buf()];
    for pair in words.windows(2) {
        if pair[0] == "cd" {
            bases.push(normalize(Path::new(&pair[1]), cwd));
        }
    }
    let mut out: Vec<String> = Vec::new();
    for w in &words {
        if w.starts_with('-') && !w.contains('/') {
            continue;
        }
        let named =
            bases.iter().any(|b| is_interlock_state_path(&normalize(Path::new(w), b), interlock_dir, own_worktree));
        if named && !out.contains(w) {
            out.push(w.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_everything_under_interlock_but_the_own_worktree() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let dir = root.join(".interlock");
        let wt = dir.join("worktrees/t-worker-1");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(dir.join("state.db"), "").unwrap();
        let state = |p: &str, base: &Path| is_interlock_state_path(&normalize(Path::new(p), base), &dir, Some(&wt));
        assert!(state(".interlock/state.db", &root));
        assert!(state("../../state.db", &wt), "climbing out of the worktree");
        assert!(state(".interlock/tasks/x.toml", &root), "missing files count too");
        assert!(state(".interlock/worktrees/t-verifier-1/a.py", &root), "another attempt's worktree");
        assert!(!state("src/a.py", &wt));
        assert!(!state(&wt.join("src/a.py").display().to_string(), &root), "the own worktree");
        assert!(!state("README.md", &root));
        std::os::unix::fs::symlink(dir.join("state.db"), root.join("link.db")).unwrap();
        assert!(state("link.db", &root), "through a symlink");
    }

    #[test]
    fn commands_that_name_the_state_are_found() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let dir = root.join(".interlock");
        let wt = dir.join("worktrees/t-worker-1");
        std::fs::create_dir_all(&wt).unwrap();
        let found = |cmd: &str, cwd: &Path| state_paths_in_command(cmd, cwd, &dir, Some(&wt));
        assert_eq!(found("rm -rf .interlock", &root), [".interlock"]);
        assert!(!found("cd / && sed -i s/a/b/ .interlock/state.db", &wt).is_empty(), "relative to the repository");
        assert!(!found("echo x > ../../guided.json", &wt).is_empty(), "redirection out of the worktree");
        assert!(!found("mv x /tmp/y && cp a --target-directory=.interlock/tasks", &root).is_empty());
        assert!(!found("cd .interlock && rm state.db", &root).is_empty());
        let own = format!("cd {} && interlock check run --criterion repro", wt.display());
        assert_eq!(found(&own, &root), Vec::<String>::new(), "working in the own worktree is fine");
        assert_eq!(found("python3 -m unittest discover -s tests", &wt), Vec::<String>::new());
        assert_eq!(found("interlock task create - <<'EOF'\nid = \"x\"\nEOF", &root), Vec::<String>::new());
    }
}
