//! Sorts a shell command into an action class, so a per-call policy hook can
//! decide whether the effective grant covers it. Unknown commands are treated
//! as local and reversible; anything that reaches a remote is at least
//! external. This is a conservative first cut, not a sandbox: it does not
//! parse shell grammar, so containment still comes from host tool restrictions.

use interlock_schema::ActionClass;

const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "wc", "grep", "rg", "fd", "pwd", "echo", "which", "file", "stat", "tree", "diff",
    "sort", "uniq", "less", "more", "true", "false", "test", "[",
];

/// Programs that run another command given as their arguments.
const WRAPPERS: &[&str] = &[
    "sudo", "env", "nohup", "time", "nice", "xargs", "command", "exec", "timeout", "sh", "bash", "zsh", "dash", "eval",
];

const GIT_READ_ONLY: &[&str] = &["status", "log", "diff", "show", "branch", "rev-parse", "ls-files", "blame", "remote"];

/// Git options that take a separate value before the subcommand.
const GIT_VALUE_OPTS: &[&str] = &["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"];

/// gh options that take a separate value.
const GH_VALUE_OPTS: &[&str] = &["-R", "--repo", "--hostname", "-X", "--method", "-f", "-F", "--field", "--raw-field"];

/// interlock subcommands only the operator may run.
const OPERATOR_ONLY: &[&[&str]] = &[
    &["grant"],
    &["task", "unblock"],
    &["task", "tree"],
    &["task", "cancel"],
    &["task", "fail"],
    &["task", "retry"],
    &["integrate", "confirm"],
];

/// The highest action class among the commands in a shell line.
pub fn classify_shell(line: &str) -> ActionClass {
    split_commands(line).iter().map(|c| classify_one(c)).max().unwrap_or(ActionClass::Read)
}

/// Splits on command separators, treating subshells and substitutions as
/// separate commands and dropping quote characters.
fn split_commands(line: &str) -> Vec<Vec<String>> {
    let spaced: String = line
        .chars()
        .flat_map(|c| match c {
            '`' | '(' | ')' | '{' | '}' | '\n' | ';' => vec![' ', ';', ' '],
            '|' | '&' => vec![' ', ';', ' '],
            '"' | '\'' => vec![],
            c => vec![c],
        })
        .collect();
    let mut commands = Vec::new();
    let mut current = Vec::new();
    for word in spaced.split_whitespace() {
        if word == ";" {
            if !current.is_empty() {
                commands.push(std::mem::take(&mut current));
            }
        } else {
            current.push(word.to_string());
        }
    }
    if !current.is_empty() {
        commands.push(current);
    }
    commands
}

fn classify_one(words: &[String]) -> ActionClass {
    let mut words: Vec<&str> = words.iter().map(String::as_str).collect();
    // Strip wrappers, their flags and variable assignments down to the real program.
    loop {
        while words.first().is_some_and(|w| w.contains('=') && !w.starts_with('-')) {
            words.remove(0);
        }
        match words.first() {
            Some(w) if WRAPPERS.contains(w) => {
                let wrapper = words.remove(0);
                while words.first().is_some_and(|w| w.starts_with('-')) {
                    words.remove(0);
                }
                if wrapper == "timeout"
                    && words.first().is_some_and(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
                {
                    words.remove(0);
                }
            }
            _ => break,
        }
    }
    let Some(&program) = words.first() else {
        return ActionClass::Read;
    };
    let args = &words[1..];
    let redirects = args.iter().any(|a| a.contains('>'));
    let class = match program.rsplit('/').next().unwrap_or(program) {
        "git" => classify_git(args),
        "gh" => classify_gh(args),
        "interlock" => classify_interlock(args),
        "ssh" | "scp" | "rsync" | "sftp" => ActionClass::ExternalReversible,
        "curl" | "wget" => {
            let writes = args.iter().any(|a| {
                matches!(*a, "-X" | "--request" | "-d" | "--data" | "-F" | "--form" | "-T" | "--upload-file")
                    || a.starts_with("--data")
            });
            if writes { ActionClass::ExternalReversible } else { ActionClass::Read }
        }
        "find" => {
            // -exec runs another command; classify it too.
            match args.iter().position(|a| matches!(*a, "-exec" | "-execdir" | "-ok" | "-okdir")) {
                Some(i) => {
                    let inner: Vec<String> = args[i + 1..]
                        .iter()
                        .take_while(|a| !matches!(**a, "\\;" | "+"))
                        .map(|s| s.to_string())
                        .collect();
                    classify_one(&inner).max(ActionClass::LocalReversible)
                }
                None if args.contains(&"-delete") => ActionClass::LocalReversible,
                None => ActionClass::Read,
            }
        }
        p if READ_ONLY.contains(&p) => ActionClass::Read,
        _ => ActionClass::LocalReversible,
    };
    if redirects { class.max(ActionClass::LocalReversible) } else { class }
}

fn positional<'a>(args: &[&'a str], value_opts: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if value_opts.contains(a) {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(*a);
        }
    }
    out
}

fn classify_git(args: &[&str]) -> ActionClass {
    let pos = positional(args, GIT_VALUE_OPTS);
    let Some(&sub) = pos.first() else {
        return ActionClass::Read;
    };
    match sub {
        "push" => {
            let forced = args.iter().any(|a| {
                matches!(*a, "--force" | "--mirror" | "--delete" | "--prune")
                    || a.starts_with("--force-with-lease")
                    || a.starts_with("--force-if-includes")
                    // Short flag clusters such as -f, -uf or -d.
                    || (a.starts_with('-') && !a.starts_with("--") && (a.contains('f') || a.contains('d')))
            });
            // +ref forces, :ref deletes.
            let refspec_rewrites = pos.iter().skip(2).any(|r| r.starts_with('+') || r.starts_with(':'));
            if forced || refspec_rewrites { ActionClass::Irreversible } else { ActionClass::ExternalReversible }
        }
        s if GIT_READ_ONLY.contains(&s) => ActionClass::Read,
        _ => ActionClass::LocalReversible,
    }
}

fn classify_gh(args: &[&str]) -> ActionClass {
    let pos = positional(args, GH_VALUE_OPTS);
    let has = |pair: [&str; 2]| pos.windows(2).any(|w| w == pair);
    if has(["pr", "merge"]) {
        return ActionClass::Landing;
    }
    if has(["repo", "delete"]) || has(["release", "delete"]) {
        return ActionClass::Irreversible;
    }
    if pos.first() == Some(&"api") {
        let method = args.windows(2).find(|w| matches!(w[0], "-X" | "--method")).map(|w| w[1].to_ascii_uppercase());
        let fields = args.iter().any(|a| matches!(*a, "-f" | "-F" | "--field" | "--raw-field"));
        return match method.as_deref() {
            Some("DELETE") => ActionClass::Irreversible,
            Some("PUT" | "POST" | "PATCH") if pos.iter().any(|p| p.contains("/merge")) => ActionClass::Landing,
            Some("GET") | None if !fields => ActionClass::Read,
            _ => ActionClass::ExternalReversible,
        };
    }
    match pos.get(1) {
        Some(&("view" | "list" | "status" | "diff" | "checks")) => ActionClass::Read,
        _ => ActionClass::ExternalReversible,
    }
}

/// Operator-only interlock commands always pause, so an agent cannot grant
/// itself authority or unblock its own task.
fn classify_interlock(args: &[&str]) -> ActionClass {
    let pos = positional(args, &["--db", "--profile"]);
    if OPERATOR_ONLY.iter().any(|cmd| pos.starts_with(cmd)) { ActionClass::Irreversible } else { ActionClass::Read }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ActionClass::*;

    #[test]
    fn classes() {
        assert_eq!(classify_shell("ls -la"), Read);
        assert_eq!(classify_shell("git status"), Read);
        assert_eq!(classify_shell("cargo test"), LocalReversible);
        assert_eq!(classify_shell("git commit -m wip"), LocalReversible);
        assert_eq!(classify_shell("git push origin feature"), ExternalReversible);
        assert_eq!(classify_shell("git push -u origin feature"), ExternalReversible);
        assert_eq!(classify_shell("gh pr comment 4 --body hi"), ExternalReversible);
        assert_eq!(classify_shell("gh pr merge 4 --squash"), Landing);
        assert_eq!(classify_shell("gh pr view 4"), Read);
        assert_eq!(classify_shell("echo hi > notes.txt"), LocalReversible);
    }

    #[test]
    fn force_push_is_irreversible_wherever_the_flag_sits() {
        assert_eq!(classify_shell("git push --force"), Irreversible);
        assert_eq!(classify_shell("git push origin main --force"), Irreversible);
        assert_eq!(classify_shell("git push -uf origin main"), Irreversible);
        assert_eq!(classify_shell("git push origin +main"), Irreversible);
        assert_eq!(classify_shell("git push origin :old-branch"), Irreversible);
        assert_eq!(classify_shell("git push --force-with-lease=main origin"), Irreversible);
    }

    #[test]
    fn options_before_the_subcommand_do_not_hide_it() {
        assert_eq!(classify_shell("git -C ../repo push --force"), Irreversible);
        assert_eq!(classify_shell("git -c user.name=x push origin main"), ExternalReversible);
        assert_eq!(classify_shell("gh -R owner/repo pr merge 4"), Landing);
        assert_eq!(classify_shell("gh api -X PUT repos/o/r/pulls/4/merge"), Landing);
        assert_eq!(classify_shell("gh api -X DELETE repos/o/r/git/refs/heads/x"), Irreversible);
        assert_eq!(classify_shell("gh api repos/o/r/pulls"), Read);
    }

    #[test]
    fn wrappers_and_substitutions_do_not_hide_the_command() {
        assert_eq!(classify_shell("bash -c \"git push --force\""), Irreversible);
        assert_eq!(classify_shell("sudo -E env FOO=1 git push -f"), Irreversible);
        assert_eq!(classify_shell("timeout 30 gh pr merge 4"), Landing);
        assert_eq!(classify_shell("echo $(git push -f)"), Irreversible);
        assert_eq!(classify_shell("echo `gh pr merge 4`"), Landing);
        assert_eq!(classify_shell("find . -name x -exec git push -f \\;"), Irreversible);
        assert_eq!(classify_shell("/usr/bin/git push --force"), Irreversible);
    }

    #[test]
    fn chained_commands_take_the_highest_class() {
        assert_eq!(classify_shell("cargo fmt && git push -f"), Irreversible);
        assert_eq!(classify_shell("git status; git push"), ExternalReversible);
        assert_eq!(classify_shell("FOO=1 git push origin x"), ExternalReversible);
        assert_eq!(classify_shell("cargo test & git push -f"), Irreversible);
    }

    #[test]
    fn agents_cannot_run_operator_commands() {
        assert_eq!(classify_shell("interlock grant create --classes landing"), Irreversible);
        assert_eq!(classify_shell("interlock --db x task unblock t1"), Irreversible);
        assert_eq!(classify_shell("interlock status t1"), Read);
        assert_eq!(classify_shell("interlock result submit --tree abc"), Read);
    }
}
