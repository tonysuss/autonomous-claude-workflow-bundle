//! interlock's own state, everything under the repository's `.interlock/`,
//! is off limits to the agents it governs: edit tools may not write there,
//! and shell commands that would change it are refused. Reading is allowed:
//! Claude Code's skills keep their references there. An attempt's own
//! worktree, which lives there too, is the one exception.
//!
//! Commands are judged by what they name. A command that computes a path at
//! run time, or a script that writes there, is beyond what a hook can see.

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

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    /// Ends a simple command: `;`, `&&`, `||`, `|`, `&`, a newline, `(`, `)`, `` ` ``, `$(`.
    Sep,
    /// An output redirection: the next word is a file the command writes.
    Out,
}

/// Removes here-document bodies: they are data on stdin, not paths.
fn strip_heredocs(command: &str) -> String {
    let mut out = String::new();
    let mut until: Option<(String, bool)> = None;
    for line in command.lines() {
        if let Some((delim, tabs)) = &until {
            let l = if *tabs { line.trim_start_matches('\t') } else { line };
            if l.trim_end() == delim {
                until = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(i) = line.find("<<").filter(|i| !line[i + 2..].starts_with('<')) {
            let rest = &line[i + 2..];
            let tabs = rest.starts_with('-');
            let rest = rest.trim_start_matches('-').trim_start();
            let delim: String =
                rest.trim_start_matches(['\'', '"']).chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !delim.is_empty() {
                until = Some((delim, tabs));
            }
        }
    }
    out
}

/// A shell command as words, separators and output redirections, with quotes removed.
fn tokens(command: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = command.chars().peekable();
    let end = |word: &mut String, quoted: &mut bool, toks: &mut Vec<Tok>| {
        if !word.is_empty() || *quoted {
            toks.push(Tok::Word(std::mem::take(word)));
        }
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                quoted = true;
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    word.push(d);
                }
            }
            '"' => {
                quoted = true;
                while let Some(d) = chars.next() {
                    match d {
                        '"' => break,
                        '\\' => word.extend(chars.next()),
                        d => word.push(d),
                    }
                }
            }
            '\\' => word.extend(chars.next()),
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                end(&mut word, &mut quoted, &mut toks);
                toks.push(Tok::Sep);
            }
            ';' | '|' | '&' | '\n' | '(' | ')' | '`' => {
                if c == '&' && chars.peek() == Some(&'>') {
                    continue; // `&>`: the `>` follows
                }
                end(&mut word, &mut quoted, &mut toks);
                toks.push(Tok::Sep);
            }
            '>' => {
                // A file descriptor number before `>` is not a word.
                if !word.is_empty() && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                }
                end(&mut word, &mut quoted, &mut toks);
                if matches!(chars.peek(), Some('>') | Some('|')) {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2`: a duplicated descriptor, not a file.
                    chars.next();
                    while chars.peek().is_some_and(|d| d.is_ascii_digit() || *d == '-') {
                        chars.next();
                    }
                } else {
                    toks.push(Tok::Out);
                }
            }
            '<' => end(&mut word, &mut quoted, &mut toks),
            c if c.is_whitespace() => end(&mut word, &mut quoted, &mut toks),
            c => word.push(c),
        }
    }
    end(&mut word, &mut quoted, &mut toks);
    toks
}

/// Programs that only read the files they name, given their arguments.
fn reads_only(program: &str, args: &[String]) -> bool {
    let has = |flags: &[&str]| args.iter().any(|a| flags.iter().any(|f| a == f || a.starts_with(&format!("{f}="))));
    match program.rsplit('/').next().unwrap_or(program) {
        "cat" | "head" | "tail" | "less" | "more" | "grep" | "egrep" | "fgrep" | "rg" | "ls" | "wc" | "diff"
        | "cmp" | "file" | "stat" | "du" | "tree" | "jq" | "sort" | "uniq" | "cut" | "nl" | "od" | "xxd"
        | "sha256sum" | "md5sum" | "realpath" | "readlink" | "basename" | "dirname" | "pwd" | "cd" | "echo"
        | "printf" | "test" | "[" | "true" | "false" | "which" | "type" | "interlock" => true,
        "sed" => !args.iter().any(|a| a.starts_with("-i") || a.starts_with("--in-place")),
        "find" => !has(&["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls"]),
        "git" => {
            // The first argument that is not an option or an option's value.
            let mut rest = args.iter();
            let mut sub = None;
            while let Some(a) = rest.next() {
                if a == "-C" || a == "-c" {
                    rest.next();
                } else if !a.starts_with('-') {
                    sub = Some(a.as_str());
                    break;
                }
            }
            matches!(
                sub,
                Some("show" | "log" | "diff" | "status" | "blame" | "rev-parse" | "ls-files" | "ls-tree" | "cat-file")
                    | Some("grep" | "describe" | "shortlog" | "show-ref" | "merge-base")
            )
        }
        _ => false,
    }
}

/// Paths a word mentions: the word itself, and any run of path characters in
/// it that names `.interlock` (as in `python3 -c "open('.interlock/x', 'w')"`).
fn mentions(word: &str, interlock_dir: &Path) -> Vec<String> {
    let mut out = vec![word.to_string()];
    let is_path_char = |c: char| c.is_alphanumeric() || "._/-~+@".contains(c);
    let abs = interlock_dir.display().to_string();
    for needle in [".interlock", abs.as_str()] {
        for (i, _) in word.match_indices(needle) {
            let start = word[..i].rfind(|c: char| !is_path_char(c)).map_or(0, |j| j + 1);
            let end = word[i..].find(|c: char| !is_path_char(c)).map_or(word.len(), |j| i + j);
            let run = word[start..end].to_string();
            if run != word && !out.contains(&run) {
                out.push(run);
            }
        }
    }
    out
}

/// The parts of `command` that would change interlock state: files it
/// redirects output to, and paths that commands other than read-only ones
/// name, resolved against the directory each part runs in (following `cd`).
/// A command that changes files while its directory is inside interlock state
/// names that directory. Empty when the command leaves the state alone.
pub fn state_paths_in_command(
    command: &str,
    cwd: &Path,
    interlock_dir: &Path,
    own_worktree: Option<&Path>,
) -> Vec<String> {
    let is_state =
        |word: &str, dir: &Path| is_interlock_state_path(&normalize(Path::new(word), dir), interlock_dir, own_worktree);
    let mut named: Vec<String> = Vec::new();
    let mut name = |w: String| {
        if !named.contains(&w) {
            named.push(w);
        }
    };
    let mut dir = cwd.to_path_buf();
    let toks = tokens(&strip_heredocs(command));
    for segment in toks.split(|t| *t == Tok::Sep) {
        let mut words: Vec<String> = Vec::new();
        let mut outputs: Vec<String> = Vec::new();
        let mut redirect = false;
        for t in segment {
            match t {
                Tok::Out => redirect = true,
                Tok::Word(w) if redirect => {
                    outputs.push(w.clone());
                    redirect = false;
                }
                Tok::Word(w) => words.push(w.clone()),
                Tok::Sep => {}
            }
        }
        for target in outputs.iter().filter(|t| *t != "/dev/null") {
            if is_state(target, &dir) {
                name(target.clone());
            }
        }
        // Skip leading variable assignments and wrappers to find the program.
        let wrappers = ["sudo", "env", "exec", "command", "nohup", "time", "nice"];
        let start = words
            .iter()
            .position(|w| !wrappers.contains(&w.as_str()) && !(w.contains('=') && !w.starts_with(['-', '/', '.'])))
            .unwrap_or(words.len());
        let Some(program) = words.get(start) else { continue };
        let args = &words[start + 1..];
        if program == "cd" {
            if let Some(target) = args.iter().find(|a| !a.starts_with('-')) {
                dir = normalize(Path::new(target), &dir);
            }
            continue;
        }
        if reads_only(program, args) {
            continue;
        }
        if is_interlock_state_path(&normalize(&dir, Path::new("/")), interlock_dir, own_worktree) {
            name(format!("{} (the command's directory)", dir.display()));
        }
        for word in words.iter().skip(start) {
            let value = word.split_once('=').map_or(word.as_str(), |(_, v)| v);
            for m in mentions(value, interlock_dir) {
                let looks_like_path = m.contains('/') || m.starts_with('.') || dir.join(&m).exists();
                if looks_like_path && is_state(&m, &dir) {
                    name(m);
                }
            }
        }
    }
    named
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo {
        _dir: tempfile::TempDir,
        root: PathBuf,
        state: PathBuf,
        wt: PathBuf,
    }

    fn repo() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join(".interlock");
        let wt = state.join("worktrees/t-worker-1");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(state.join("worktrees/t-verifier-1")).unwrap();
        std::fs::create_dir_all(state.join("guided/claude-code-plugin/skills/route/references")).unwrap();
        std::fs::write(state.join("state.db"), "").unwrap();
        Repo { _dir: dir, root, state, wt }
    }

    #[test]
    fn state_is_everything_under_interlock_but_the_own_worktree() {
        let r = repo();
        let state =
            |p: &str, base: &Path| is_interlock_state_path(&normalize(Path::new(p), base), &r.state, Some(&r.wt));
        assert!(state(".interlock/state.db", &r.root));
        assert!(state("../../state.db", &r.wt), "climbing out of the worktree");
        assert!(state(".interlock/tasks/x.toml", &r.root), "missing files count too");
        assert!(state(".interlock/worktrees/t-verifier-1/a.py", &r.root), "another attempt's worktree");
        assert!(!state("src/a.py", &r.wt));
        assert!(!state(&r.wt.join("src/a.py").display().to_string(), &r.root), "the own worktree");
        assert!(!state("README.md", &r.root));
        std::os::unix::fs::symlink(r.state.join("state.db"), r.root.join("link.db")).unwrap();
        assert!(state("link.db", &r.root), "through a symlink");
    }

    #[test]
    fn commands_that_would_change_the_state_are_found() {
        let r = repo();
        let found = |cmd: &str, cwd: &Path| state_paths_in_command(cmd, cwd, &r.state, Some(&r.wt));
        assert_eq!(found("rm -rf .interlock", &r.root), [".interlock"]);
        assert!(!found("sed -i s/a/b/ ../../state.db", &r.wt).is_empty(), "sed -i out of the worktree");
        assert!(!found("echo x > ../../guided.json", &r.wt).is_empty(), "redirection out of the worktree");
        assert!(!found("echo x >> .interlock/guided/claude-code-plugin/hooks/hooks.json", &r.root).is_empty());
        assert!(!found("mv x /tmp/y && cp a --target-directory=.interlock/tasks", &r.root).is_empty());
        assert!(!found("cd .interlock && rm state.db", &r.root).is_empty(), "following cd");
        assert!(!found("cd .interlock/worktrees/t-verifier-1 && rm -rf *", &r.root).is_empty(), "inside the state");
        assert!(!found("python3 -c \"open('.interlock/state.db', 'w')\"", &r.root).is_empty(), "inside a string");
        assert!(!found("echo $(touch .interlock/x)", &r.root).is_empty(), "inside a substitution");
        assert!(!found("git -C .interlock/worktrees/t-verifier-1 checkout -- .", &r.root).is_empty());
    }

    #[test]
    fn reading_the_state_and_working_in_the_own_worktree_are_fine() {
        let r = repo();
        let found = |cmd: &str, cwd: &Path| state_paths_in_command(cmd, cwd, &r.state, Some(&r.wt));
        let none = Vec::<String>::new();
        let refs = ".interlock/guided/claude-code-plugin/skills/route/references/task-templates.md";
        assert_eq!(found(&format!("cat {refs} | head -120"), &r.root), none, "the skills' references");
        assert_eq!(found("ls .interlock/worktrees; grep -rn x .interlock/guided", &r.root), none);
        assert_eq!(found("git -C .interlock/worktrees/t-verifier-1 show --stat HEAD", &r.root), none);
        assert_eq!(found("sed -n 1,5p .interlock/state.db 2>/dev/null >/tmp/out", &r.root), none);
        let own = format!("cd {} && interlock check run --criterion repro && python3 -m unittest", r.wt.display());
        assert_eq!(found(&own, &r.root), none, "working in the own worktree");
        assert_eq!(found("python3 -m unittest discover -s tests -t . > /tmp/log 2>&1", &r.wt), none);
        let heredoc = "interlock task create - <<'EOF'\nid = \"x\"\ncheck = \"sh .interlock/x\"\nEOF\necho done";
        assert_eq!(found(heredoc, &r.root), none, "a here-document is data");
        // A session no attempt governs, whose directory is a submitted worktree, may still leave it and use interlock.
        let free = |cmd: &str| state_paths_in_command(cmd, &r.wt, &r.state, None);
        assert_eq!(free(&format!("cd {} && interlock status t", r.root.display())), none);
        assert_eq!(free("interlock advance t"), none);
        assert!(!free("python3 -m unittest").is_empty(), "but not change files there");
    }
}
