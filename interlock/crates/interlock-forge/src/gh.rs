//! The GitHub forge: pull requests through `gh`, branches through `git push`
//! and `git ls-remote` against the repository's remote.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use interlock_core::delivery::PrState;
use serde_json::Value;

use crate::{Check, CheckStatus, Forge, ForgeError, MergeMethod, NewPr, PrView};

/// The fields interlock reads from `gh pr view` and `gh pr list`.
pub const PR_FIELDS: &str = "number,url,state,isDraft,headRefName,headRefOid,baseRefName,baseRefOid,\
                             mergeable,mergeStateStatus,statusCheckRollup,mergeCommit,autoMergeRequest";

#[derive(Debug, Clone)]
pub struct GhForge {
    /// The `gh` binary.
    pub gh: PathBuf,
    /// The local repository. `gh` and `git` run here.
    pub dir: PathBuf,
    /// The git remote that is the GitHub repository.
    pub remote: String,
    /// `OWNER/REPO`, passed to `gh --repo` when set.
    pub repo: Option<String>,
    /// How long one `gh` or `git` call may take before its outcome counts as unknown.
    pub timeout: Duration,
}

impl GhForge {
    /// Reads INTERLOCK_GH_BIN, INTERLOCK_FORGE_REMOTE and INTERLOCK_FORGE_REPO.
    pub fn from_env(dir: &Path) -> GhForge {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        GhForge {
            gh: var("INTERLOCK_GH_BIN").map_or_else(|| PathBuf::from("gh"), PathBuf::from),
            dir: dir.to_path_buf(),
            remote: var("INTERLOCK_FORGE_REMOTE").unwrap_or_else(|| "origin".into()),
            repo: var("INTERLOCK_FORGE_REPO"),
            timeout: Duration::from_secs(120),
        }
    }

    fn gh(&self, args: &[&str]) -> Result<String, ForgeError> {
        let mut all: Vec<&str> = args.to_vec();
        if let Some(repo) = &self.repo {
            all.extend(["--repo", repo]);
        }
        let env = [("GH_PROMPT_DISABLED", "1"), ("GH_NO_UPDATE_NOTIFIER", "1"), ("NO_COLOR", "1")];
        run(&self.gh, &all, &self.dir, &env, self.timeout)
    }

    fn git(&self, args: &[&str]) -> Result<String, ForgeError> {
        run(Path::new("git"), args, &self.dir, &[("GIT_TERMINAL_PROMPT", "0")], self.timeout)
    }
}

/// Runs one command to completion or `timeout`. A command that cannot start,
/// times out, or dies from a signal is `Unavailable`: it may have acted.
pub(crate) fn run(
    program: &Path,
    args: &[&str],
    dir: &Path,
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<String, ForgeError> {
    let shown = format!("{} {}", program.display(), args.join(" "));
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ForgeError::Unavailable(format!("cannot run {shown}: {e}")))?;
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ForgeError::Unavailable(format!("{shown} gave no answer in {}s", timeout.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(ForgeError::Unavailable(format!("{shown}: {e}"))),
        }
    };
    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    match status.code() {
        Some(0) => Ok(stdout.trim().to_string()),
        Some(code) => Err(ForgeError::Failed {
            code,
            message: format!("{shown} exited {code}: {}", last_lines(&stderr, &stdout)),
        }),
        None => Err(ForgeError::Unavailable(format!("{shown} was killed before it answered"))),
    }
}

fn last_lines(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(4)..].join(" | ")
}

fn pr_state(s: &str) -> Option<PrState> {
    match s {
        "OPEN" => Some(PrState::Open),
        "CLOSED" => Some(PrState::Closed),
        "MERGED" => Some(PrState::Merged),
        _ => None,
    }
}

/// One entry of `statusCheckRollup`: a check run or a commit status.
fn parse_check(v: &Value) -> Check {
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_ascii_uppercase();
    let name = v["name"].as_str().or_else(|| v["context"].as_str()).unwrap_or("?").to_string();
    let status = if v["__typename"] == "StatusContext" || v.get("state").is_some_and(Value::is_string) {
        match text("state").as_str() {
            "SUCCESS" => CheckStatus::Passed,
            "PENDING" | "EXPECTED" => CheckStatus::Pending,
            _ => CheckStatus::Failed,
        }
    } else if text("status") != "COMPLETED" {
        CheckStatus::Pending
    } else {
        match text("conclusion").as_str() {
            "SUCCESS" => CheckStatus::Passed,
            "NEUTRAL" | "SKIPPED" => CheckStatus::Skipped,
            _ => CheckStatus::Failed,
        }
    };
    Check { name, status }
}

/// Reads one pull request from `gh pr view --json` or `gh pr list --json` output.
pub fn parse_pr(v: &Value) -> Result<PrView, ForgeError> {
    let bad = |what: &str| ForgeError::Invalid(format!("pull request JSON has no {what}: {v}"));
    let text = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    Ok(PrView {
        number: v["number"].as_u64().ok_or_else(|| bad("number"))?,
        url: text("url").unwrap_or_default(),
        state: v["state"].as_str().and_then(pr_state).ok_or_else(|| bad("state"))?,
        draft: v["isDraft"].as_bool().unwrap_or(false),
        head_branch: text("headRefName").unwrap_or_default(),
        head: text("headRefOid").ok_or_else(|| bad("headRefOid"))?,
        base_branch: text("baseRefName").unwrap_or_default(),
        base: text("baseRefOid"),
        mergeable: text("mergeable"),
        merge_state: text("mergeStateStatus"),
        checks: v["statusCheckRollup"].as_array().map(|a| a.iter().map(parse_check).collect()).unwrap_or_default(),
        merge_commit: v["mergeCommit"]["oid"].as_str().map(str::to_string),
        auto_merge: v["autoMergeRequest"].is_object(),
    })
}

fn json(text: &str) -> Result<Value, ForgeError> {
    serde_json::from_str(text).map_err(|e| ForgeError::Invalid(format!("{e}: {text}")))
}

impl Forge for GhForge {
    fn name(&self) -> &str {
        "github"
    }

    fn default_branch(&self) -> Result<String, ForgeError> {
        let out = self.git(&["ls-remote", "--symref", &self.remote, "HEAD"])?;
        out.lines()
            .find_map(|l| l.strip_prefix("ref: refs/heads/").and_then(|r| r.split_whitespace().next()))
            .map(str::to_string)
            .ok_or_else(|| ForgeError::Invalid(format!("{} reports no default branch", self.remote)))
    }

    fn branch_head(&self, branch: &str) -> Result<Option<String>, ForgeError> {
        let out = self.git(&["ls-remote", &self.remote, &format!("refs/heads/{branch}")])?;
        Ok(out.split_whitespace().next().map(str::to_string))
    }

    fn push(&self, commit: &str, branch: &str, lease: Option<&str>) -> Result<(), ForgeError> {
        let lease = format!("--force-with-lease=refs/heads/{branch}:{}", lease.unwrap_or(""));
        let refspec = format!("{commit}:refs/heads/{branch}");
        self.git(&["push", "--porcelain", "--no-follow-tags", &lease, &self.remote, &refspec]).map(|_| ())
    }

    fn fetch(&self, branch: &str, into: &str) -> Result<String, ForgeError> {
        let refspec = format!("+refs/heads/{branch}:{into}");
        self.git(&["fetch", "--no-tags", "--no-write-fetch-head", "--quiet", &self.remote, &refspec])?;
        self.git(&["rev-parse", "--verify", &format!("{into}^{{commit}}")])
    }

    fn find_pr(&self, branch: &str) -> Result<Option<PrView>, ForgeError> {
        let out = self.gh(&["pr", "list", "--head", branch, "--state", "all", "--limit", "20", "--json", PR_FIELDS])?;
        let mut prs: Vec<PrView> = match json(&out)? {
            Value::Array(items) => items.iter().map(parse_pr).collect::<Result<_, _>>()?,
            other => return Err(ForgeError::Invalid(format!("gh pr list did not return a list: {other}"))),
        };
        prs.sort_by_key(|p| (p.state == PrState::Open, p.number));
        Ok(prs.pop())
    }

    fn view_pr(&self, number: u64) -> Result<PrView, ForgeError> {
        parse_pr(&json(&self.gh(&["pr", "view", &number.to_string(), "--json", PR_FIELDS])?)?)
    }

    fn create_pr(&self, pr: &NewPr) -> Result<u64, ForgeError> {
        let out = self.gh(&[
            "pr", "create", "--head", &pr.branch, "--base", &pr.base, "--title", &pr.title, "--body", &pr.body,
        ])?;
        out.lines()
            .rev()
            .find_map(|l| l.trim().rsplit_once("/pull/").and_then(|(_, n)| n.trim_end_matches('/').parse().ok()))
            .ok_or_else(|| ForgeError::Invalid(format!("gh pr create printed no pull request URL: {out}")))
    }

    fn merge(&self, number: u64, head: &str, method: MergeMethod, auto: bool) -> Result<(), ForgeError> {
        let n = number.to_string();
        let mut args = vec!["pr", "merge", n.as_str(), method.flag(), "--match-head-commit", head];
        if auto {
            args.push("--auto");
        }
        self.gh(&args).map(|_| ())
    }
}
