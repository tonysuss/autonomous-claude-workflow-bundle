//! Task scope: the paths a worker may change, as globs relative to the
//! repository root. `**` matches any number of path segments, `*` and `?`
//! match within one segment. An empty scope allows no path; `**` allows every path.

use std::path::{Component, Path};

pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| match_segments(rest, &segs[i..])),
        Some((p, rest)) => segs.split_first().is_some_and(|(s, tail)| match_one(p, s) && match_segments(rest, tail)),
    }
}

fn match_one(pat: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Where a path falls relative to the worktree root and the task scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Inside the worktree and inside scope; carries the relative path.
    InScope(String),
    /// Inside the worktree but outside the task's scope.
    OutOfScope(String),
    /// Outside the worktree altogether.
    Outside,
}

/// Normalizes `path` (absolute, or relative to `root`) without touching the
/// filesystem, then checks it against `scope`.
pub fn place(root: &Path, path: &Path, scope: &[String]) -> Placement {
    let joined = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
    let mut parts: Vec<String> = Vec::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    let root_parts: Vec<String> = root
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.len() < root_parts.len() || parts[..root_parts.len()] != root_parts[..] {
        return Placement::Outside;
    }
    let rel = parts[root_parts.len()..].join("/");
    if scope.iter().any(|g| glob_matches(g, &rel)) { Placement::InScope(rel) } else { Placement::OutOfScope(rel) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_matches("src/export/**", "src/export/retry.rs"));
        assert!(glob_matches("src/export/**", "src/export/a/b.rs"));
        assert!(!glob_matches("src/export/**", "src/import/retry.rs"));
        assert!(glob_matches("**/*.py", "a/b/c.py"));
        assert!(glob_matches("*.md", "README.md"));
        assert!(!glob_matches("*.md", "docs/README.md"));
        assert!(glob_matches("tests/test_?.py", "tests/test_a.py"));
    }

    #[test]
    fn placement() {
        let root = Path::new("/w/att-1");
        let scope = vec!["src/**".to_string(), "tests/**".to_string()];
        assert_eq!(place(root, Path::new("src/a.rs"), &scope), Placement::InScope("src/a.rs".into()));
        assert_eq!(place(root, Path::new("/w/att-1/tests/t.rs"), &scope), Placement::InScope("tests/t.rs".into()));
        assert_eq!(place(root, Path::new("README.md"), &scope), Placement::OutOfScope("README.md".into()));
        assert_eq!(place(root, Path::new("../att-2/src/a.rs"), &scope), Placement::Outside);
        assert_eq!(place(root, Path::new("/home/user/.bashrc"), &scope), Placement::Outside);
        assert_eq!(place(root, Path::new("anything"), &[]), Placement::OutOfScope("anything".into()), "empty: nothing");
        assert_eq!(place(root, Path::new("a/b"), &["**".to_string()]), Placement::InScope("a/b".into()));
    }
}
