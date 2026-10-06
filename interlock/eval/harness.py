#!/usr/bin/env python3
"""Three-condition evaluation harness for interlock (design §12 P0 S3, §13).

Conditions, on the same frozen tasks, with the host version, model and effort
pinned:

  plain      the host's ordinary headless workflow: the task prompt, the same
             host tools interlock grants the task's worker, no interlock
  skills     the same, with a skills plugin loaded (--skills-dir, or
             --skills-generate) and no runtime enforcement
  interlock  `interlock run <task> --host ...`; with --interlock-skills it also
             loads the skills plugin into its sessions ("skills with interlock")

Subcommands:

  build      build the frozen task repositories; fails if any differs from the lock
  selftest   prove each task sound with no model calls
  run        run conditions x tasks x repeats, judge each output, keep spend
             under the budget
  report     aggregate results into report.json and report.md
  export     copy sanitized evidence (no output trees) into a directory

Directories, which must not overlap: --state-dir (frozen repositories and
start trees), --results (records, transcripts and judged trees), and
--run-base, which holds only the run in progress: each run gets a fresh root
there, deleted after judging. Python 3.11+, standard library only. See
docs/evaluation.md.
"""

import argparse
import datetime
import gzip
import hashlib
import importlib.util
import json
import os
import random
import re
import secrets
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

EVAL_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, EVAL_DIR)
import interrupt  # noqa: E402
import leakscan  # noqa: E402

TASKSET = os.path.join(EVAL_DIR, "taskset")
LOCK_FILE = os.path.join(TASKSET, "frozen.lock.json")
EVIDENCE_DIR = os.path.normpath(os.path.join(EVAL_DIR, "..", "evidence"))
HARNESS_VERSION = "2"
CONDITIONS = ("plain", "skills", "interlock")
IGNORE = shutil.ignore_patterns("__pycache__", "*.pyc", ".pytest_cache")

# Every run's environment is built from this list, never inherited wholesale:
# the session that launches the harness has many variables (CLAUDE_CODE_*
# session bindings, tokens) that must not reach the agents. --pass-env adds
# names; the names, never the values, are recorded with each run.
ENV_ALLOW = [
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "TERM", "TZ", "TMPDIR",
    "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "NO_PROXY", "no_proxy", "ALL_PROXY", "all_proxy",
    "SSL_CERT_FILE", "SSL_CERT_DIR", "NODE_EXTRA_CA_CERTS", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE", "GIT_SSL_CAINFO",
    "ANTHROPIC_BASE_URL", "GOPATH", "GOROOT", "GOCACHE", "GOMODCACHE",
]

# What a plain run is told about finishing. interlock's own state is its claim.
STATUS_SUFFIX = (
    "\n\nWhen you have finished, end your final message with a line `STATUS: DONE` if the task is "
    "complete, or `STATUS: NOT DONE: <reason>` if it is not."
)
RECOVERY_PREFIX = (
    "A previous session working on this task was interrupted before it finished. Any changes it "
    "made are still in the working directory.\n\n"
)

# Default budget charge for a session whose cost the host never reported
# (killed or timed out before its result event). For the budget only; never
# a metric. On October 6 a whole plain session averaged $0.076 and a whole
# interlock worker session $0.105; a session killed part-way costs less.
UNREPORTED_SESSION_RESERVE_USD = 0.30


# --------------------------------------------------------------------------
# Small utilities


def now_iso():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def run_cmd(cmd, cwd=None, env=None, check=True, stdin=None, timeout=None):
    p = subprocess.run(cmd, cwd=cwd, env=env, input=stdin, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)} (in {cwd}) exited {p.returncode}:\n{p.stdout}\n{p.stderr}")
    return p


def git(repo, *args, env=None, check=True):
    full = dict(os.environ if env is None else env)
    full.setdefault("GIT_CONFIG_NOSYSTEM", "1")
    return run_cmd(["git", *args], cwd=repo, env=full, check=check).stdout.strip()


def write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=1, sort_keys=False)
        f.write("\n")
    os.replace(tmp, path)


def read_json(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def jsonl(path):
    try:
        with open(path, errors="replace") as f:
            for line in f:
                line = line.strip()
                if line.startswith("{"):
                    try:
                        yield json.loads(line)
                    except ValueError:
                        pass
    except OSError:
        return


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree_digest(root):
    """A digest of every file under root, by relative path and content."""
    h = hashlib.sha256()
    for dirpath, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d != "__pycache__")
        for f in sorted(files):
            if f.endswith(".pyc"):
                continue
            p = os.path.join(dirpath, f)
            h.update(os.path.relpath(p, root).encode() + b"\0")
            with open(p, "rb") as fh:
                h.update(hashlib.sha256(fh.read()).digest())
    return h.hexdigest()


def inside(path, root):
    path, root = os.path.realpath(path), os.path.realpath(root)
    return path == root or path.startswith(root.rstrip("/") + "/")


def glob_matches(pattern, path):
    """interlock's scope globs: `**` spans segments, `*` and `?` stay in one."""
    pat = [s for s in pattern.split("/") if s]
    segs = [s for s in path.split("/") if s]

    def one(p, s):
        return re.fullmatch("".join(".*" if c == "*" else "." if c == "?" else re.escape(c) for c in p), s) is not None

    def match(pi, si):
        if pi == len(pat):
            return si == len(segs)
        if pat[pi] == "**":
            return any(match(pi + 1, k) for k in range(si, len(segs) + 1))
        return si < len(segs) and one(pat[pi], segs[si]) and match(pi + 1, si + 1)

    return match(0, 0)


def out_of_scope(paths, scope):
    """Unlike interlock, an empty scope here means no file may change."""
    return [p for p in paths if not any(glob_matches(g, p) for g in scope)]


# --------------------------------------------------------------------------
# Process trees


def _children_map():
    kids = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat") as f:
                stat = f.read()
            ppid = int(stat[stat.rindex(")") + 2 :].split()[1])
        except (OSError, ValueError, IndexError):
            continue
        kids.setdefault(ppid, []).append(int(name))
    return kids


def descendants(pid):
    kids, out, todo = _children_map(), [], [pid]
    while todo:
        p = todo.pop()
        for c in kids.get(p, []):
            out.append(c)
            todo.append(c)
    return out


def kill_tree(pid):
    """Freezes, then kills, a process and everything it started: the host,
    its tool processes, and interlock with its sessions and hooks. A crash,
    not a polite cancel."""
    me = os.getpid()
    seen = set()
    for _ in range(3):
        pids = [p for p in [pid, *descendants(pid)] if p != me and p not in seen]
        for p in pids:
            try:
                os.kill(p, signal.SIGSTOP)
            except ProcessLookupError:
                pass
        seen.update(pids)
    for p in seen:
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    return sorted(seen)


def run_proc(cmd, cwd, env, stdin_text, stdout_path, stderr_path, timeout_s, watch=None):
    """Runs cmd in its own session. With `watch`, the loop calls it about
    every 50 ms; a string from it kills the whole tree with that reason."""
    os.makedirs(os.path.dirname(stdout_path), exist_ok=True)
    start = time.monotonic()
    killed, killed_pids = None, []
    with open(stdout_path, "wb") as out, open(stderr_path, "wb") as err:
        p = subprocess.Popen(
            cmd,
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE if stdin_text is not None else subprocess.DEVNULL,
            stdout=out,
            stderr=err,
            start_new_session=True,
        )
        if stdin_text is not None:

            def feed():
                try:
                    p.stdin.write(stdin_text.encode())
                    p.stdin.close()
                except OSError:
                    pass

            threading.Thread(target=feed, daemon=True).start()
        while True:
            try:
                p.wait(timeout=0.05 if watch else 1)
                break
            except subprocess.TimeoutExpired:
                pass
            if time.monotonic() - start > timeout_s:
                killed = f"harness timeout after {timeout_s}s"
            elif watch is not None:
                killed = watch()
            if killed:
                killed_pids = kill_tree(p.pid)
                p.wait()
                break
    return {
        "exit_code": p.returncode,
        "killed": killed,
        "killed_pids": len(killed_pids),
        "wall_s": round(time.monotonic() - start, 1),
    }


# --------------------------------------------------------------------------
# Tasks, the lock and frozen repositories


class Task:
    def __init__(self, task_dir):
        self.dir = task_dir
        with open(os.path.join(task_dir, "meta.toml"), "rb") as f:
            self.meta = tomllib.load(f)
        with open(os.path.join(task_dir, "task.toml"), "rb") as f:
            self.spec = tomllib.load(f)
        with open(os.path.join(task_dir, "prompt.md")) as f:
            self.prompt = f.read()
        self.id = self.meta["id"]
        self.kind = self.meta["kind"]
        self.language = self.meta["language"]
        self.repo = self.meta["repo"]
        self.scope = self.meta["scope"]
        self.interrupt = self.meta.get("interrupt")
        self.wants_answer = bool(self.meta.get("answer"))
        self.harder = self.meta.get("harder")
        self.history = os.path.join(task_dir, "history.py")
        self.solution = os.path.join(task_dir, "hidden", "solution")
        self.near_misses_file = os.path.join(task_dir, "hidden", "near_misses.toml")

    @property
    def judge(self):
        return os.path.join(self.dir, self.meta["judge"])

    def near_misses(self):
        if not os.path.exists(self.near_misses_file):
            return []
        with open(self.near_misses_file, "rb") as f:
            return tomllib.load(f).get("near_miss", [])


def load_tasks(ids=None):
    root = os.path.join(TASKSET, "tasks")
    tasks = [Task(os.path.join(root, d)) for d in sorted(os.listdir(root))]
    if ids:
        known = {t.id for t in tasks}
        unknown = [i for i in ids if i not in known]
        if unknown:
            sys.exit(f"unknown task(s): {', '.join(unknown)}")
        tasks = [t for t in tasks if t.id in ids]
    return tasks


def load_lock():
    lock = read_json(LOCK_FILE) or {}
    if lock.get("version") != 2:
        return {"version": 2, "judgelib_sha256": None, "tasks": {}}
    return lock


def judgelib_digest():
    return sha256_file(os.path.join(EVAL_DIR, "judgelib.py"))


def _commit_env(i):
    authors = [("Rae Lin", "rae@example.org"), ("Tomas Ek", "tomas@example.org")]
    name, email = authors[i % 2]
    date = f"2026-08-{1 + i:02d}T10:{i:02d}:00+00:00"
    env = dict(os.environ)
    env.update(
        GIT_AUTHOR_NAME=name,
        GIT_AUTHOR_EMAIL=email,
        GIT_AUTHOR_DATE=date,
        GIT_COMMITTER_NAME=name,
        GIT_COMMITTER_EMAIL=email,
        GIT_COMMITTER_DATE=date,
        GIT_CONFIG_GLOBAL="/dev/null",
        GIT_CONFIG_NOSYSTEM="1",
    )
    return env


def _commit_all(repo, message, i):
    for root, dirs, files in os.walk(repo):
        dirs[:] = [d for d in dirs if d != ".git"]
        for f in files:
            os.chmod(os.path.join(root, f), 0o644)
    env = _commit_env(i)
    git(repo, "add", "-A", env=env)
    git(repo, "commit", "-q", "--no-verify", "-m", message, env=env)
    return git(repo, "rev-parse", "HEAD", env=env)


def _overlay(src, dest):
    for root, _, files in os.walk(src):
        for f in files:
            if f.endswith(".pyc"):
                continue
            rel = os.path.relpath(os.path.join(root, f), src)
            os.makedirs(os.path.dirname(os.path.join(dest, rel)), exist_ok=True)
            shutil.copy(os.path.join(root, f), os.path.join(dest, rel))


def _apply_edits(repo, edits):
    for edit in edits:
        path = os.path.join(repo, edit[1])
        if edit[0] == "write":
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(edit[2])
        elif edit[0] == "replace":
            with open(path) as f:
                text = f.read()
            if text.count(edit[2]) != 1:
                raise RuntimeError(f"history edit for {edit[1]}: expected one match of {edit[2]!r}")
            with open(path, "w") as f:
                f.write(text.replace(edit[2], edit[3]))
        else:
            raise RuntimeError(f"unknown edit {edit[0]}")


def _clone_at(source, dest):
    """A real repository at the commit before a known fix. Every ref and the
    remote are removed and unreachable objects pruned, so the fix cannot be
    found in the agent's copy."""
    run_cmd(["git", "clone", "-q", "--no-checkout", source["git"], dest])
    git(dest, "checkout", "-q", "--detach", source["commit"])
    for ref in git(dest, "for-each-ref", "--format=%(refname)").splitlines():
        git(dest, "update-ref", "-d", ref)
    git(dest, "remote", "remove", "origin", check=False)
    git(dest, "checkout", "-q", "-B", "main")
    git(dest, "reflog", "expire", "--expire=now", "--all")
    git(dest, "gc", "-q", "--prune=now")
    return [{"sha": git(dest, "rev-parse", "HEAD"), "message": f"upstream {source['commit']}"}]


def build_repo(task, dest):
    """Builds a task's own starting repository deterministically: same inputs,
    same commit ids, on any machine."""
    if os.path.exists(dest):
        shutil.rmtree(dest)
    commits = []
    source = task.meta.get("source")
    if source:
        commits = _clone_at(source, dest)
        start = os.path.join(task.dir, "start")
        if os.path.isdir(start):
            _overlay(start, dest)
            env = _commit_env(1)
            git(dest, "add", "-A", env=env)
            git(dest, "commit", "-q", "--no-verify", "-m", "Add the task's visible checks", env=env)
            commits.append({"sha": git(dest, "rev-parse", "HEAD", env=env), "message": "Add the task's visible checks"})
        return _facts(task, dest, commits)
    shutil.copytree(os.path.join(TASKSET, "repos", task.repo), dest, ignore=IGNORE)
    git(dest, "init", "-q", "-b", "main", env=_commit_env(0))
    if os.path.exists(task.history):
        spec = importlib.util.spec_from_file_location(f"history_{task.id.replace('-', '_')}", task.history)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for i, (message, culprit, edits) in enumerate(module.COMMITS):
            _apply_edits(dest, edits)
            commits.append({"sha": _commit_all(dest, message, i), "message": message, "culprit": culprit})
    else:
        start = os.path.join(task.dir, "start")
        if os.path.isdir(start):
            _overlay(start, dest)
        commits.append({"sha": _commit_all(dest, f"Import {task.repo}", 0), "message": f"Import {task.repo}"})
    return _facts(task, dest, commits)


def _facts(task, dest, commits):
    facts = {
        "task": task.id,
        "base_commit": commits[-1]["sha"],
        "base_tree": git(dest, "rev-parse", "HEAD^{tree}"),
        "commits": commits,
    }
    culprits = [c["sha"] for c in commits if c.get("culprit")]
    if culprits:
        facts["culprit"] = culprits[0]
    return facts


def export_tree(repo, tree, dest):
    os.makedirs(dest, exist_ok=True)
    archive = subprocess.run(["git", "archive", "--format=tar", tree], cwd=repo, capture_output=True, check=True)
    subprocess.run(["tar", "-x", "-C", dest], input=archive.stdout, check=True)


class Frozen:
    """A task's frozen repository, checked against the lock: HEAD is the
    locked commit, the tree is clean, and the task directory and the judge
    library hash to what the lock says. The start tree is exported afresh
    from the locked commit."""

    def __init__(self, state_dir, task, verify=True):
        self.task = task
        self.repo = os.path.join(state_dir, "frozen", task.id)
        self.facts_path = os.path.join(state_dir, "frozen", f"{task.id}.facts.json")
        self.start_dir = os.path.join(state_dir, "start", task.id)
        facts = read_json(self.facts_path)
        if facts is None or not os.path.isdir(self.repo):
            facts = build_repo(task, self.repo)
            write_json(self.facts_path, facts)
        self.facts = facts
        if verify:
            self.verify()
        if os.path.exists(self.start_dir):
            shutil.rmtree(self.start_dir)
        export_tree(self.repo, self.facts["base_tree"], self.start_dir)

    def verify(self):
        lock = load_lock()
        entry = lock["tasks"].get(self.task.id)
        problems = []
        if not entry:
            problems.append("no entry in the lock")
        else:
            head = git(self.repo, "rev-parse", "HEAD")
            if head != entry["base_commit"] or head != self.facts["base_commit"]:
                problems.append(f"HEAD {head} is not the locked commit {entry['base_commit']}")
            if git(self.repo, "status", "--porcelain"):
                problems.append("the frozen repository has uncommitted changes")
            digest = tree_digest(self.task.dir)
            if digest != entry.get("task_dir_sha256"):
                problems.append("the task directory differs from the lock (prompt, task file, hidden checks or judge)")
        if lock.get("judgelib_sha256") != judgelib_digest():
            problems.append("judgelib.py differs from the lock")
        if problems:
            raise SystemExit(
                f"{self.task.id}: {'; '.join(problems)}. If the task set changed on purpose, run "
                "`harness.py build --update-lock`."
            )


def worktree_tree(repo):
    """The tree of the files in a working directory, as git would commit
    them (ignored files left out), without touching the real index."""
    fd, index = tempfile.mkstemp(prefix="eval-index-")
    os.close(fd)
    os.unlink(index)
    env = dict(os.environ, GIT_INDEX_FILE=index)
    try:
        git(repo, "add", "-A", env=env)
        return git(repo, "write-tree", env=env)
    finally:
        if os.path.exists(index):
            os.unlink(index)


def changed_paths(repo, base_tree, tree):
    out = git(repo, "diff-tree", "-r", "--no-renames", "--name-only", base_tree, tree)
    return [l for l in out.splitlines() if l]


# --------------------------------------------------------------------------
# Layout and per-run working space


def check_layout(run_base, state_dir, results):
    """The run base must share no ancestry with the state, the results or the
    harness, so a run's root has no frozen repositories, start trees,
    results or earlier runs next to it."""
    places = {"run base": run_base, "state dir": state_dir, "results": results}
    fixed = {"harness": EVAL_DIR, "evidence": EVIDENCE_DIR}
    names = list(places)
    for i, a in enumerate(names):
        for b in names[i + 1 :]:
            if inside(places[a], places[b]) or inside(places[b], places[a]):
                raise SystemExit(f"the {a} ({places[a]}) and the {b} ({places[b]}) must not contain each other")
        for b, path in fixed.items():
            if inside(places[a], path) or inside(path, places[a]):
                raise SystemExit(f"the {a} ({places[a]}) and the {b} ({path}) must not contain each other")


def prepare_run_base(run_base, state_dir):
    """Creates the run base, or empties it of roots this harness left behind
    (a crash). Anything it did not create stops the run."""
    os.makedirs(run_base, exist_ok=True)
    log = os.path.join(state_dir, "run-roots.log")
    ours = set()
    if os.path.exists(log):
        with open(log) as f:
            ours = {l.strip() for l in f if l.strip()}
    for name in os.listdir(run_base):
        path = os.path.join(run_base, name)
        if path in ours:
            shutil.rmtree(path, ignore_errors=True)
        else:
            raise SystemExit(f"the run base {run_base} holds {name}, which this harness did not create")


class RunSpace:
    """A fresh root in the run base, alone there for the run's lifetime."""

    def __init__(self, run_base, state_dir):
        self.root = os.path.join(run_base, secrets.token_hex(5))
        os.mkdir(self.root)
        with open(os.path.join(state_dir, "run-roots.log"), "a") as f:
            f.write(self.root + "\n")

    def materialize(self, frozen, name):
        """A working copy of the locked commit (not the frozen working tree),
        with no remote and no trace of where it was fetched from."""
        repo = os.path.join(self.root, name)
        os.mkdir(repo)
        commit = frozen.facts["base_commit"]
        git(repo, "init", "-q", "-b", "main")
        git(repo, "fetch", "-q", "--no-tags", frozen.repo, "main")
        git(repo, "checkout", "-q", "-B", "main", commit)
        if git(repo, "rev-parse", "HEAD") != commit:
            raise RuntimeError(f"working copy is not at the locked commit {commit}")
        fetch_head = os.path.join(repo, ".git", "FETCH_HEAD")
        if os.path.exists(fetch_head):
            os.unlink(fetch_head)
        git(repo, "reflog", "expire", "--expire=now", "--all")
        return repo

    def plugin(self, master):
        """This run's own copy of the skills plugin, inside its root, so the
        agent reading a skill file does not reach outside the run."""
        dest = os.path.join(self.root, "plugins", "workflow-skills")
        shutil.copytree(master, dest)
        return dest

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


# --------------------------------------------------------------------------
# Hosts


class HostConfig:
    def __init__(self, a, state_dir):
        self.host = a.host
        self.model = a.model
        self.effort = a.effort
        self.max_turns = a.max_turns
        self.session_timeout_s = a.session_timeout_min * 60
        self.max_sessions = a.max_sessions
        self.per_run_usd = a.per_run_usd
        self.interlock_bin = os.path.abspath(a.interlock_bin)
        self.claude_bin = a.claude_bin or shutil.which("claude")
        self.copilot_bin = a.copilot_bin or shutil.which("copilot")
        self.fake_model = a.fake_model
        self.pass_env = list(a.pass_env or [])
        self.interlock_skills = getattr(a, "interlock_skills", False)
        self.unreported_reserve_usd = getattr(a, "unreported_reserve_usd", UNREPORTED_SESSION_RESERVE_USD)
        self.state_dir = state_dir
        self.skills_plugin = None
        self._features = None
        self._tools = {}

    def bin(self):
        return self.claude_bin if self.host == "claude-code" else self.copilot_bin

    def version(self):
        try:
            out = run_cmd([self.bin(), "--version"], check=False, timeout=60).stdout.strip()
        except (OSError, subprocess.TimeoutExpired) as e:
            return f"unavailable: {e}"
        m = re.search(r"\d+\.\d+\.\d+", out)
        return m.group(0) if m else out

    def check_interlock_bin(self):
        """Agents call `interlock` by name; interlock puts its own directory
        first on their PATH. That only finds the binary being measured if it
        is named `interlock`."""
        if os.path.basename(self.interlock_bin) != "interlock":
            raise SystemExit(f"{self.interlock_bin}: the interlock binary must be named `interlock`, or agents "
                             "would run whatever `interlock` is on PATH instead of the one measured")
        path = os.path.dirname(self.interlock_bin) + os.pathsep + self.base_env()["PATH"]
        found = shutil.which("interlock", path=path)
        if not found or not os.path.samefile(found, self.interlock_bin):
            raise SystemExit(f"`interlock` on the session PATH resolves to {found}, not {self.interlock_bin}")

    def features(self):
        """Flags this interlock's `run` accepts, from its help."""
        if self._features is None:
            help_text = run_cmd([self.interlock_bin, "run", "--help"], check=False).stdout
            skills_help = run_cmd([self.interlock_bin, "skills", "--help"], check=False)
            self._features = {
                "effort": "--effort" in help_text,
                "skills": "--skills" in help_text,
                "skills_generate": skills_help.returncode == 0 and "generate" in skills_help.stdout,
            }
        return self._features

    def base_env(self):
        env = {k: os.environ[k] for k in ENV_ALLOW + self.pass_env if k in os.environ}
        env["GOTOOLCHAIN"] = "local"
        if self.host == "copilot":
            env["COPILOT_AUTO_UPDATE"] = "false"
        return env

    def env_for(self, condition):
        env = self.base_env()
        if condition == "interlock":
            if self.host == "claude-code" and self.claude_bin:
                env["INTERLOCK_CLAUDE_BIN"] = self.claude_bin
            if self.host == "copilot" and self.copilot_bin:
                env["INTERLOCK_COPILOT_BIN"] = self.copilot_bin
            if self.effort and not self.features()["effort"]:
                # This interlock cannot pass --effort; Claude Code reads the level from here.
                env["CLAUDE_CODE_EFFORT_LEVEL"] = self.effort
        return env

    def effort_mechanism(self, condition):
        if not self.effort:
            return "host default"
        if self.host != "claude-code":
            return "not supported by this host"
        if condition != "interlock":
            return "--effort"
        return "interlock run --effort" if self.features()["effort"] else "CLAUDE_CODE_EFFORT_LEVEL"

    def worker_tools(self, task):
        """The host patterns interlock grants this task's worker, from
        `interlock host tools`, so the plain and skills conditions get exactly
        the same tools."""
        if task.id in self._tools:
            return self._tools[task.id]
        tmp = tempfile.mkdtemp(prefix="tools-", dir=os.path.join(self.state_dir))
        try:
            db = os.path.join(tmp, "state.db")
            env = self.base_env()
            run_cmd([self.interlock_bin, "--db", db, "init"], cwd=tmp, env=env)
            run_cmd([self.interlock_bin, "--db", db, "task", "create", os.path.join(task.dir, "task.toml")],
                    cwd=tmp, env=env)
            out = json.loads(run_cmd([self.interlock_bin, "--db", db, "host", "tools", task.id, "--role", "worker",
                                      "--host", self.host], cwd=tmp, env=env).stdout)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
        tools = {
            "allow": out["host_patterns"]["allow"],
            "deny": out["host_patterns"]["deny"],
            "host_neutral": out["host_neutral"],
            "source": f"interlock host tools {task.id} --role worker --host {self.host}",
        }
        self._tools[task.id] = tools
        return tools

    def plain_command(self, prompt, tools, plugin_dir=None, budget_usd=None):
        """The host's ordinary headless invocation, with the worker's tools.
        Permission mode and settings isolation mirror interlock's sessions;
        interlock's hooks are not loaded."""
        allow, deny = list(tools["allow"]), list(tools["deny"])
        if self.host == "claude-code":
            if plugin_dir:
                allow.append("Skill")
            cmd = [
                self.claude_bin, "-p", "--output-format", "stream-json", "--verbose",
                "--permission-mode", "dontAsk", "--setting-sources", "", "--no-session-persistence",
                "--max-turns", str(self.max_turns),
            ]
            if self.model:
                cmd += ["--model", self.model]
            if self.effort:
                cmd += ["--effort", self.effort]
            if budget_usd:
                cmd += ["--max-budget-usd", f"{budget_usd:.2f}"]
            if plugin_dir:
                cmd += ["--plugin-dir", plugin_dir]
            cmd += ["--allowedTools", *allow]
            if deny:
                cmd += ["--disallowedTools", *deny]
            return cmd, prompt
        cmd = [self.copilot_bin, "-p", prompt, "--output-format", "json", "--no-ask-user", "--no-auto-update"]
        cmd += [f"--allow-tool={t}" for t in allow] + [f"--deny-tool={t}" for t in deny]
        if plugin_dir:
            cmd += ["--plugin-dir", plugin_dir, "--allow-tool=skill"]
        if self.model:
            cmd += ["--model", self.model]
        return cmd, None

    def interlock_command(self, task_id, plugin_dir=None):
        cmd = [
            self.interlock_bin, "run", task_id, "--host", self.host,
            "--timeout", f"{self.session_timeout_s}s", "--max-turns", str(self.max_turns),
            "--max-sessions", str(self.max_sessions),
        ]
        if self.model:
            cmd += ["--model", self.model]
        if self.effort and self.features()["effort"]:
            cmd += ["--effort", self.effort]
        if self.interlock_skills and plugin_dir:
            cmd += ["--skills", plugin_dir]
        return cmd


def prepare_skills(a, cfg):
    """The skills plugin both the skills and (with --interlock-skills) the
    interlock condition load: a given directory, or interlock's generator."""
    src = None
    if a.skills_generate:
        if not cfg.features()["skills_generate"]:
            raise SystemExit("this interlock has no `skills generate`; pass --skills-dir instead")
        out = os.path.join(cfg.state_dir, "skills", cfg.host)
        if os.path.exists(out):
            shutil.rmtree(out)
        target = {"claude-code": "claude-code", "copilot": "copilot"}[cfg.host]
        cmd = [w.format(target=target, out=out, interlock=cfg.interlock_bin) for w in a.skills_generate_cmd.split()]
        run_cmd(cmd, env=cfg.base_env())
        src = out
    elif a.skills_dir:
        src = os.path.abspath(a.skills_dir)
    if src is None:
        return None
    if os.path.exists(os.path.join(src, ".claude-plugin", "plugin.json")):
        return src
    plugin = os.path.join(cfg.state_dir, "skills-plugin")
    if os.path.exists(plugin):
        shutil.rmtree(plugin)
    os.makedirs(os.path.join(plugin, ".claude-plugin"))
    with open(os.path.join(plugin, ".claude-plugin", "plugin.json"), "w") as f:
        # A neutral name: the agent sees it, and it should not hint at an evaluation.
        json.dump({"name": "workflow-skills", "version": "0.0.0"}, f)
    skills = os.path.join(src, "skills") if os.path.isdir(os.path.join(src, "skills")) else src
    shutil.copytree(skills, os.path.join(plugin, "skills"))
    return plugin


def parse_session(host, path):
    """What a host's output stream says about one session: final text,
    cost and tokens when the host reports them. Missing values stay None."""
    if host == "copilot":
        return _parse_copilot(path)
    events = list(jsonl(path))
    result = next((e for e in reversed(events) if e.get("type") == "result"), None)
    init = next((e for e in events if e.get("type") == "system" and e.get("subtype") == "init"), {})
    s = {
        "complete": result is not None,
        "final_text": None,
        "is_error": True,
        "cost_usd": None,
        "tokens": None,
        "turns": None,
        "models": [],
        "model_requested": init.get("model"),
        "denials": None,
        "events": len(events),
    }
    if result:
        s["final_text"] = result.get("result")
        s["is_error"] = bool(result.get("is_error", True))
        s["subtype"] = result.get("subtype")
        s["cost_usd"] = result.get("total_cost_usd")
        s["turns"] = result.get("num_turns")
        s["denials"] = len(result.get("permission_denials") or [])
        usage = result.get("modelUsage") or {}
        s["models"] = sorted(usage)
        s["tokens"] = {
            "input": sum(u.get("inputTokens", 0) for u in usage.values()),
            "output": sum(u.get("outputTokens", 0) for u in usage.values()),
            "cache_read": sum(u.get("cacheReadInputTokens", 0) for u in usage.values()),
            "cache_write": sum(u.get("cacheCreationInputTokens", 0) for u in usage.values()),
        }
    return s


def _parse_copilot(path):
    events = list(jsonl(path))
    s = {"complete": False, "final_text": None, "is_error": True, "cost_usd": None, "tokens": None,
         "turns": 0, "models": [], "denials": 0, "events": len(events)}
    for e in events:
        t = e.get("type")
        data = e.get("data") or {}
        if t == "assistant.message" and isinstance(data.get("content"), str) and data["content"].strip():
            s["final_text"] = data["content"]
        elif t == "assistant.turn_end":
            s["turns"] += 1
        elif t == "tool.execution_complete" and (data.get("error") or {}).get("code") == "denied":
            s["denials"] += 1
        elif t == "result":
            s["complete"] = True
            s["is_error"] = e.get("exitCode") not in (0, None)
            usage = e.get("usage")
            if isinstance(usage, dict):
                s["usage_reported"] = usage
    # Copilot CLI reports premium requests, not dollars or tokens.
    return s


STATUS_RE = re.compile(r"^\W*STATUS\W*:\s*(NOT\s+DONE|DONE)\b", re.I | re.M)
ANSWER_RE = re.compile(r"^\W*ANSWER\W*:.*$", re.I | re.M)


def status_claim(text):
    found = STATUS_RE.findall(text or "")
    if not found:
        return False, "no STATUS line"
    last = found[-1].upper().split()
    return last == ["DONE"], " ".join(last)


def answer_lines(text):
    return [m.group(0).strip() for m in ANSWER_RE.finditer(text or "")]


def plain_prompt(task, recovery=False):
    """The task prompt, the task's scope in words, and the STATUS request."""
    if task.scope:
        scope = "Change only files that match these paths: " + ", ".join(f"`{g}`" for g in task.scope) + "."
    else:
        scope = "Do not change any files."
    text = task.prompt.rstrip("\n") + "\n\n" + scope + STATUS_SUFFIX
    return RECOVERY_PREFIX + text if recovery else text


def tool_usage(raw_dir):
    """Descriptive counts from a run's transcripts: skills invoked, and shell
    commands that call interlock. Not metrics."""
    skills, interlock_cmds = [], 0
    for root, _, files in os.walk(raw_dir):
        for f in files:
            if not f.endswith(".jsonl"):
                continue
            for e in jsonl(os.path.join(root, f)):
                if e.get("type") != "assistant":
                    continue
                for item in (e.get("message") or {}).get("content") or []:
                    if not isinstance(item, dict) or item.get("type") != "tool_use":
                        continue
                    inp = item.get("input") or {}
                    if item.get("name") == "Skill":
                        skills.append(str(inp.get("skill") or inp.get("name") or inp))
                    cmd = inp.get("command")
                    if isinstance(cmd, str) and re.search(r"(^|[;&|]\s*|\bcd [^;&|]*&&\s*)interlock\b", cmd):
                        interlock_cmds += 1
    return {"skills_invoked": skills, "interlock_commands": interlock_cmds}


def checks_run(raw_dir):
    """Test or check commands that ran to a result anywhere in a run's
    transcripts (an agent's own evidence, read from what it did)."""
    uses, done = {}, set()
    for root, _, files in os.walk(raw_dir):
        for f in files:
            if not f.endswith(".jsonl"):
                continue
            with open(os.path.join(root, f), errors="replace") as fh:
                for line in fh:
                    for kind, tid, info in interrupt._tool_events(line):
                        if kind == "use" and info["test"]:
                            uses[tid] = info["command"]
                        elif kind == "result":
                            done.add(tid)
    return [cmd for tid, cmd in uses.items() if tid in done]


# --------------------------------------------------------------------------
# Running one (task, condition, repeat)


class Runner:
    def __init__(self, cfg, state_dir, run_base, results, label, seed):
        self.cfg = cfg
        self.state_dir = state_dir
        self.run_base = run_base
        self.results = results
        self.label = label
        self.seed = seed

    def _delay(self, task, condition, repeat, attempt):
        rng = random.Random(f"{self.seed}:{task.id}:{condition}:{repeat}:{attempt}")
        return rng.uniform(0, float(task.interrupt.get("max_delay_s", 20)))

    def run_one(self, task, frozen, condition, repeat, attempt=1):
        run_id = f"{task.id}.{condition}.r{repeat}.{secrets.token_hex(3)}"
        run_dir = os.path.join(self.results, "runs", run_id)
        raw = os.path.join(run_dir, "raw")
        os.makedirs(raw)
        space = RunSpace(self.run_base, self.state_dir)
        env = self.cfg.env_for(condition)
        tools = self.cfg.worker_tools(task)
        record = {
            "run_id": run_id,
            "label": self.label,
            "task": task.id,
            "kind": task.kind,
            "language": task.language,
            "condition": condition,
            "repeat": repeat,
            "attempt": attempt,
            "host": self.cfg.host,
            "host_version": self.cfg.version(),
            "model": self.cfg.model,
            "effort": self.cfg.effort,
            "effort_mechanism": self.cfg.effort_mechanism(condition),
            "fake_model": bool(self.cfg.fake_model),
            "started_at": now_iso(),
            "base_commit": frozen.facts["base_commit"],
            "tools": tools if condition != "interlock" else {**tools, "note": "interlock grants these itself"},
            "skills_plugin": self.cfg.skills_plugin if (condition == "skills" or
                                                         (condition == "interlock" and self.cfg.interlock_skills)) else None,
            "env_names": sorted(env),
            "sessions": [],
            "notes": [],
        }
        fake = None
        started = time.monotonic()
        try:
            repo = space.materialize(frozen, task.repo)
            plugin = None
            if record["skills_plugin"]:
                plugin = space.plugin(self.cfg.skills_plugin)
            if self.cfg.fake_model:
                fake = self._start_fake(task, frozen, env)
            if condition == "interlock":
                out = self._run_interlock(task, repo, space.root, raw, env, record, repeat, attempt, plugin)
            else:
                out = self._run_plain(task, repo, raw, env, record, condition, tools, repeat, attempt, plugin)
            record["wall_s"] = round(time.monotonic() - started, 1)
            self._finish(task, frozen, repo, space, raw, record, out)
        finally:
            if fake:
                fake.close()
            space.cleanup()
        write_json(os.path.join(run_dir, "run.json"), record)
        return record

    # -- plain and skills ----------------------------------------------------

    def _run_plain(self, task, repo, raw, env, record, condition, tools, repeat, attempt, plugin):
        if condition == "skills" and not plugin:
            raise SystemExit("the skills condition needs --skills-dir or --skills-generate")
        timeout = self.cfg.session_timeout_s
        budget = self.cfg.per_run_usd
        s1 = os.path.join(raw, "session-1.jsonl")
        watch = None
        if task.interrupt:
            watch = interrupt.Interrupter(self._delay(task, condition, repeat, attempt), lambda: [repo], lambda: [s1])
        cmd, stdin = self.cfg.plain_command(plain_prompt(task), tools, plugin, budget)
        res = run_proc(cmd, repo, env, stdin, s1, os.path.join(raw, "session-1.stderr.txt"), timeout, watch)
        s = parse_session(self.cfg.host, s1)
        s.update(role=condition, exit_code=res["exit_code"], killed=res["killed"], wall_s=res["wall_s"],
                 transcript="raw/session-1.jsonl")
        record["sessions"].append(s)
        final = s
        if task.interrupt:
            record["interruption"] = {**watch.record(), "killed_processes": res["killed_pids"]}
            if watch.fired_at is not None:
                # The ordinary recovery: run the task again in the same working copy.
                spent = s["cost_usd"] or 0
                s2p = os.path.join(raw, "session-2.jsonl")
                cmd, stdin = self.cfg.plain_command(plain_prompt(task, recovery=True), tools, plugin,
                                                    max(budget - spent, 0.5) if budget else None)
                res2 = run_proc(cmd, repo, env, stdin, s2p, os.path.join(raw, "session-2.stderr.txt"), timeout)
                s2 = parse_session(self.cfg.host, s2p)
                s2.update(role=f"{condition}-recovery", exit_code=res2["exit_code"], killed=res2["killed"],
                          wall_s=res2["wall_s"], transcript="raw/session-2.jsonl")
                record["sessions"].append(s2)
                final = s2
        if final["complete"] and not final["is_error"]:
            claimed, detail = status_claim(final.get("final_text"))
        else:
            claimed, detail = False, "the session did not finish normally"
        record["claim"] = {"claimed_done": claimed, "source": "status-line", "detail": detail}
        with open(os.path.join(raw, "final.txt"), "w") as f:
            f.write(final.get("final_text") or "")
        return {"tree": worktree_tree(repo), "delivered": True, "answer_text": final.get("final_text")}

    # -- interlock -------------------------------------------------------------

    def _interlock(self, repo, env, *args, check=True):
        p = run_cmd([self.cfg.interlock_bin, *args], cwd=repo, env=env, check=check)
        try:
            return json.loads(p.stdout)
        except ValueError:
            return {"stdout": p.stdout, "stderr": p.stderr, "exit": p.returncode}

    def _run_interlock(self, task, repo, root, raw, env, record, repeat, attempt, plugin):
        task_file = os.path.join(root, "task.toml")
        shutil.copy(os.path.join(task.dir, "task.toml"), task_file)
        self._interlock(repo, env, "init")
        self._interlock(repo, env, "task", "create", task_file)
        cmd = self.cfg.interlock_command(task.id, plugin)
        record["interlock_command"] = [os.path.basename(cmd[0]), *cmd[1:]]
        run_timeout = self.cfg.session_timeout_s * (self.cfg.max_sessions + 1) + 600
        worktrees = os.path.join(repo, ".interlock", "worktrees")
        tdir = os.path.join(repo, ".interlock", "transcripts")

        def worker_dirs():
            if not os.path.isdir(worktrees):
                return []
            return [os.path.join(worktrees, d) for d in sorted(os.listdir(worktrees)) if "-worker-" in d]

        def transcripts():
            if not os.path.isdir(tdir):
                return []
            return [os.path.join(tdir, f) for f in sorted(os.listdir(tdir)) if f.endswith(".jsonl")]

        invocations = []
        watch = None
        if task.interrupt:
            watch = interrupt.Interrupter(self._delay(task, "interlock", repeat, attempt), worker_dirs, transcripts)
        res = run_proc(cmd, repo, env, None, os.path.join(raw, "run-1.json"), os.path.join(raw, "run-1.stderr.txt"),
                       run_timeout, watch)
        invocations.append(res)
        if task.interrupt:
            record["interruption"] = {**watch.record(), "killed_processes": res["killed_pids"]}
            if watch.fired_at is not None:
                # interlock's recovery: start the supervisor again. It reconciles
                # the attempt the crash left running and carries on.
                res = run_proc(cmd, repo, env, None, os.path.join(raw, "run-2.json"),
                               os.path.join(raw, "run-2.stderr.txt"), run_timeout)
                invocations.append(res)
        report = read_json(os.path.join(raw, f"run-{len(invocations)}.json"), {})
        status = self._interlock(repo, env, "status", task.id, check=False)
        log = self._interlock(repo, env, "task", "log", task.id, check=False)
        brief = self._interlock(repo, env, "brief", task.id, "--role", "verifier", "--format", "json", check=False)
        write_json(os.path.join(raw, "status.json"), status)
        write_json(os.path.join(raw, "task-log.json"), log)
        write_json(os.path.join(raw, "brief-verifier.json"), brief)

        attempts = (status.get("attempts") if isinstance(status, dict) else None) or []
        for a in attempts:
            src = os.path.join(tdir, f"{a['id']}.jsonl")
            dest = os.path.join(raw, "transcripts", f"{a['id']}.jsonl")
            if os.path.exists(src):
                os.makedirs(os.path.dirname(dest), exist_ok=True)
                shutil.copy(src, dest)
            s = parse_session(self.cfg.host, dest)
            s.update(role=a.get("role"), attempt=a["id"], attempt_status=a.get("status"),
                     transcript=os.path.relpath(dest, os.path.dirname(raw)))
            record["sessions"].append(s)

        task_state = (status.get("task") or {}) if isinstance(status, dict) else {}
        state = task_state.get("state")
        record["interlock"] = {
            "final_state": state,
            "stopped_because": report.get("stopped_because"),
            "blocked_reason": task_state.get("blocked_reason"),
            "attempts_used": task_state.get("attempts_used"),
            "signals": [m.get("signal") for m in log] if isinstance(log, list) else None,
            "invocations": invocations,
            "baseline": report.get("baseline"),
        }
        record["claim"] = {"claimed_done": state == "done", "source": "interlock-state", "detail": state}
        summary = ((brief or {}).get("previous_result") or {}).get("summary") if isinstance(brief, dict) else None
        with open(os.path.join(raw, "final.txt"), "w") as f:
            f.write(summary or "")
        return {"tree": task_state.get("current_tree"), "delivered": state == "done", "answer_text": summary}

    # -- judging and metrics ------------------------------------------------------

    def _leak_context(self, repo, space, run_id):
        other_roots = []
        log = os.path.join(self.state_dir, "run-roots.log")
        if os.path.exists(log):
            with open(log) as f:
                other_roots = [os.path.basename(l.strip()) for l in f if l.strip() and l.strip() != space.root]
        runs_dir = os.path.join(self.results, "runs")
        other_runs = [d for d in os.listdir(runs_dir) if os.path.isdir(os.path.join(runs_dir, d)) and d != run_id] \
            if os.path.isdir(runs_dir) else []
        forbidden = [("the harness state", self.state_dir), ("the results", self.results),
                     ("the task set", TASKSET), ("the harness", EVAL_DIR), ("recorded evidence", EVIDENCE_DIR)]
        needles = [(f"the path of {label}", path) for label, path in forbidden]
        needles += [("another run's working copy", n) for n in other_roots]
        needles += [("another run's id", n) for n in other_runs]
        return leakscan.Context(cwd=repo, own_root=space.root, run_base=self.run_base, forbidden=forbidden,
                                needles=needles)

    def _upstream_needles(self, task):
        source = task.meta.get("source") or {}
        out = []
        if source.get("git"):
            out.append(("the upstream repository", source["git"]))
        if task.meta.get("fix_commit"):
            out.append(("the upstream fix commit", task.meta["fix_commit"]))
        return out

    def _finish(self, task, frozen, repo, space, raw, record, out):
        tree = out["tree"]
        base_tree = frozen.facts["base_tree"]
        record["output"] = {"tree": tree, "delivered": out["delivered"]}
        verdict = None
        if tree:
            paths = changed_paths(repo, base_tree, tree)
            record["output"]["changed_paths"] = paths
            record["output"]["scope_violations"] = out_of_scope(paths, task.scope)
            verdict = judge(task, frozen, repo, tree, answer_lines(out["answer_text"]), self.results)
            record["judge"] = {
                "anon_id": verdict["anon_id"],
                "hidden_pass": verdict["pass"],
                "cases": len(verdict["cases"]),
                "failed": [c["id"] for c in verdict["cases"] if not c["pass"]],
                "condition_words_in_judged_files": verdict["condition_words"],
            }
        else:
            record["output"]["changed_paths"] = []
            record["output"]["scope_violations"] = []
            record["judge"] = {"anon_id": None, "hidden_pass": None, "cases": 0, "failed": []}

        ctx = self._leak_context(repo, space, record["run_id"])
        ctx.needles += self._upstream_needles(task)
        transcripts = [os.path.join(r, f) for r, _, fs in os.walk(raw) for f in fs if f.endswith(".jsonl")]
        findings = leakscan.scan_files(transcripts, ctx)
        record["leak_scan"] = [f.as_dict() for f in findings]

        ran = checks_run(raw)
        record["checks_run"] = ran[:20]
        record["tool_usage"] = tool_usage(raw)
        claimed = record["claim"]["claimed_done"]
        hidden_pass = bool(verdict and verdict["pass"])
        failed = len(record["judge"]["failed"])
        cost_complete, tokens_complete = _completeness(record["sessions"])
        costs = [s["cost_usd"] for s in record["sessions"] if s.get("cost_usd") is not None]
        tokens = {}
        for s in record["sessions"]:
            for k, v in (s.get("tokens") or {}).items():
                tokens[k] = tokens.get(k, 0) + v
        inter = record.get("interruption")
        record["metrics"] = {
            "accepted": bool(out["delivered"] and claimed and hidden_pass),
            "hidden_pass": hidden_pass if tree else None,
            "claimed_done": claimed,
            "false_claim": bool(claimed and not hidden_pass),
            "claim_without_evidence": bool(claimed and not ran),
            "missed_defects": failed if claimed else 0,
            "scope_violation": bool(record["output"]["scope_violations"]),
            "correct_but_not_claimed": bool(hidden_pass and not claimed),
            "recovery": None,
            "human_interventions": None,  # n/a: headless runs have nobody to intervene
            "operator_needed": not claimed,
            "restarts": 1 if (inter or {}).get("interrupted") else 0,
            "sessions": len(record["sessions"]),
            "wall_s": record["wall_s"],
            "cost_usd": round(sum(costs), 4) if costs else None,
            "cost_complete": cost_complete,
            "tokens": tokens or None,
            "tokens_complete": tokens_complete,
            "hidden_material_seen": any(f.strong for f in findings),
            "suspicious_searches": sum(1 for f in findings if not f.strong),
        }
        if task.interrupt:
            record["metrics"]["recovery"] = {
                "interrupted": bool((inter or {}).get("interrupted")),
                "valid": bool((inter or {}).get("valid")),
                "recovered": bool((inter or {}).get("valid") and record["metrics"]["accepted"]),
            }
        unreported = sum(1 for s in record["sessions"] if s.get("cost_usd") is None and s.get("events"))
        if record["fake_model"]:
            unreported = 0  # a scripted model costs nothing
        reserve = self.cfg.unreported_reserve_usd
        record["budget_charge_usd"] = round(sum(costs) + unreported * reserve, 4)
        if unreported:
            record["notes"].append(
                f"{unreported} session(s) never reported a cost; the budget reserves ${reserve} each, "
                "which is not a measurement"
            )

    def _start_fake(self, task, frozen, env):
        from fake_model import FakeModel
        import base64

        # The scripted fix writes the reference solution from inside the
        # working directory: Copilot refuses to read paths outside it.
        fix = []
        if os.path.isdir(task.solution):
            for root, _, files in os.walk(task.solution):
                for name in sorted(files):
                    path = os.path.join(root, name)
                    rel = os.path.relpath(path, task.solution)
                    with open(path, "rb") as f:
                        b64 = base64.b64encode(f.read()).decode()
                    fix.append(f"mkdir -p ./{os.path.dirname(rel) or '.'} && echo {b64} | base64 -d > ./{rel}")
        fix = fix or ["true"]
        if task.interrupt:
            # One edit, a pause, then a test run: the interruption lands in between.
            visible = next(c["check"] for c in task.spec["criterion"] if c.get("check"))
            fix = fix[:1] + ["sleep 2", f"{visible} || true"] + fix[1:]
        answer = f"ANSWER: {frozen.facts['culprit']}\n" if task.wants_answer else ""
        checks = [c for c in task.spec["criterion"] if c.get("check")]
        worker = fix + [f"interlock check run --criterion {c['id']}" for c in checks if c["producer"] == "self"]
        claims = [
            f"interlock claim add --criterion {c['id']} --strength tested --tree auto --ref 'scripted' --note 'scripted'"
            for c in task.spec["criterion"] if c["producer"] == "self"
        ]
        verifier = [f"interlock check run --criterion {c['id']}" for c in checks] + [
            f"interlock assess add --criterion {c['id']} --strength {c['min_strength']} --ref 'scripted' "
            "--note 'scripted plumbing check'"
            for c in task.spec["criterion"]
        ]
        script = {
            "plain": {"steps": fix + ([f"{checks[0]['check']} || true"] if checks else []),
                      "final": f"Applied the scripted change.\n{answer}STATUS: DONE"},
            "worker": {"steps": worker + claims, "final": f"Applied the scripted change.\n{answer}"},
            "verifier": {"steps": verifier, "final": "Scripted verification finished."},
            "on_block": claims,
        }
        fake = FakeModel(script)
        home = tempfile.mkdtemp(prefix="copilot-home-", dir=self.state_dir)
        env.update(
            COPILOT_OFFLINE="true",
            COPILOT_PROVIDER_BASE_URL=fake.base_url,
            COPILOT_MODEL="gpt-4.1",
            COPILOT_HOME=home,
            NO_PROXY="127.0.0.1,localhost",
            no_proxy="127.0.0.1,localhost",
        )
        return fake


def _completeness(sessions):
    """A run's cost (tokens) is complete only when every session reported it."""
    if not sessions:
        return False, False
    return (all(s.get("cost_usd") is not None for s in sessions),
            all(s.get("tokens") is not None for s in sessions))


def judge(task, frozen, repo, tree, answers, results):
    """Runs a task's hidden checks on an anonymized copy of the output. The
    judged directory holds the output's files and its ANSWER lines, under a
    random id: nothing in it names the condition."""
    anon = secrets.token_hex(6)
    jdir = os.path.join(results, "judge", anon)
    export_tree(repo, tree, os.path.join(jdir, "tree"))
    with open(os.path.join(jdir, "answer.txt"), "w") as f:
        f.write("\n".join(answers) + ("\n" if answers else ""))
    cases = judge_dir(task, frozen, os.path.join(jdir, "tree"), answers)
    words = condition_words(jdir)
    verdict = {"anon_id": anon, "task": task.id, "cases": cases, "pass": bool(cases) and all(c["pass"] for c in cases),
               "condition_words": words}
    write_json(os.path.join(jdir, "verdict.json"), verdict)
    return verdict


def judge_dir(task, frozen, tree_dir, answers):
    """Runs the hidden judge on a scratch copy of a tree."""
    scratch = tempfile.mkdtemp(prefix="judge-")
    try:
        t = os.path.join(scratch, "tree")
        shutil.copytree(tree_dir, t, ignore=shutil.ignore_patterns(".git", "__pycache__"))
        ans = os.path.join(scratch, "answer.txt")
        with open(ans, "w") as f:
            f.write("\n".join(answers) + ("\n" if answers else ""))
        env = {k: v for k, v in os.environ.items() if not k.startswith("INTERLOCK_")}
        env.update(GOTOOLCHAIN="local", PYTHONDONTWRITEBYTECODE="1")
        p = subprocess.run(
            [sys.executable, task.judge, "--tree", t, "--start", frozen.start_dir, "--answer", ans,
             "--facts", frozen.facts_path],
            cwd=scratch, capture_output=True, text=True, timeout=1800, env=env,
        )
        try:
            return json.loads(p.stdout)["cases"]
        except (ValueError, KeyError):
            return [{"id": "judge", "pass": False, "detail": f"judge exited {p.returncode}: {p.stderr[-1500:]}"}]
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def condition_words(jdir):
    found = []
    for root, _, files in os.walk(jdir):
        for f in files:
            if f == "verdict.json":
                continue
            try:
                with open(os.path.join(root, f), errors="replace") as fh:
                    text = fh.read().lower()
            except OSError:
                continue
            for w in ("interlock", "status: done", "condition"):
                if w in text:
                    found.append(f"{os.path.relpath(os.path.join(root, f), jdir)}: {w}")
    return found


# --------------------------------------------------------------------------
# Budget and ordering


def load_runs(results):
    runs = []
    root = os.path.join(results, "runs")
    if os.path.isdir(root):
        for d in sorted(os.listdir(root)):
            r = read_json(os.path.join(root, d, "run.json"))
            if r:
                runs.append(r)
    return runs


def run_counts(r):
    """Whether a run satisfies a repeat: an interruption run counts only if
    the interruption landed as designed."""
    return (r.get("interruption") or {"valid": True}).get("valid", True)


def estimate_next(runs, task, condition, default_usd):
    """A conservative estimate for the next run: the most a comparable run
    has cost so far, or the default when there is none."""
    same = [r["budget_charge_usd"] for r in runs if r["condition"] == condition and r["task"] == task.id]
    cond = [r["budget_charge_usd"] for r in runs if r["condition"] == condition]
    if same:
        est = max(same)
    elif cond:
        est = max(cond)
    else:
        est = default_usd[condition]
    if task.interrupt and not same:
        est *= 1.5
    return round(est, 4)


def condition_order(conditions, task_id, repeat, seed):
    """ABBA counterbalancing: a seeded order per task for odd repeats, the
    reverse for even ones."""
    order = list(conditions)
    random.Random(f"{seed}:{task_id}").shuffle(order)
    return order if repeat % 2 == 1 else order[::-1]


# --------------------------------------------------------------------------
# Subcommands


def cmd_build(a):
    state = os.path.abspath(a.state_dir)
    lock = load_lock()
    changed = []
    for task in load_tasks(a.tasks):
        dest = os.path.join(state, "frozen", task.id)
        facts = build_repo(task, dest)
        write_json(os.path.join(state, "frozen", f"{task.id}.facts.json"), facts)
        entry = {"base_commit": facts["base_commit"], "task_dir_sha256": tree_digest(task.dir)}
        old = lock["tasks"].get(task.id)
        state_word = "ok" if old == entry else "CHANGED"
        if old != entry:
            changed.append(task.id)
        print(f"{task.id:22} {facts['base_commit']}  {state_word}")
        if a.update_lock:
            lock["tasks"][task.id] = entry
    jl = judgelib_digest()
    if lock.get("judgelib_sha256") != jl:
        changed.append("judgelib.py")
        print(f"{'judgelib.py':22} {jl[:40]}  CHANGED")
    if a.update_lock:
        lock["judgelib_sha256"] = jl
        if not a.tasks:
            known = {t.id for t in load_tasks()}
            lock["tasks"] = {k: v for k, v in lock["tasks"].items() if k in known}
        lock["tasks"] = dict(sorted(lock["tasks"].items()))
        write_json(LOCK_FILE, lock)
        print(f"wrote {LOCK_FILE}")
        return 0
    if changed:
        print(f"the task set differs from the lock: {', '.join(changed)}. Rerun with --update-lock if that is intended.")
        return 1
    return 0


def _check_runs_as_promised(repo, criterion):
    p = subprocess.run(["sh", "-c", criterion["check"]], cwd=repo, capture_output=True, text=True, timeout=600,
                       env=dict(os.environ, GOTOOLCHAIN="local", PYTHONDONTWRITEBYTECODE="1"))
    return p.returncode, (p.stdout + p.stderr)[-800:]


def _apply_near_miss(repo, nm):
    for e in nm.get("edit", []):
        path = os.path.join(repo, e["path"])
        with open(path) as f:
            text = f.read()
        if text.count(e["old"]) != 1:
            raise RuntimeError(f"near miss {nm['name']!r}: {e['path']} has {text.count(e['old'])} matches")
        with open(path, "w") as f:
            f.write(text.replace(e["old"], e["new"]))


def cmd_selftest(a):
    """No model calls. Proves each task is sound before any money is spent."""
    state = os.path.abspath(a.state_dir)
    tasks = load_tasks(a.tasks)
    all_tasks = load_tasks()
    report, failures = {}, []
    frozen = {t.id: Frozen(state, t, verify=not a.no_lock) for t in all_tasks if t.id in {x.id for x in tasks}
              or not a.skip_cross}
    for task in tasks:
        r = report[task.id] = {}

        def fail(msg):
            failures.append(f"{task.id}: {msg}")
            r.setdefault("failures", []).append(msg)

        fz = frozen[task.id]
        if task.spec["intent"] != task.prompt:
            fail("task.toml intent differs from prompt.md")
        if task.spec.get("scope", {}).get("paths", []) != task.scope:
            if not (task.kind == "investigation" and not task.scope):
                fail("task.toml scope differs from meta.toml scope")
        files = run_cmd(["git", "ls-files"], cwd=fz.repo).stdout
        if re.search(r"(^|/)(hidden|judge)", files, re.M):
            fail("hidden material in the frozen repository")
        if "interlock" in run_cmd(["git", "grep", "-il", "interlock"], cwd=fz.repo, check=False).stdout.lower():
            fail("the word 'interlock' appears in the frozen repository")

        scratch = tempfile.mkdtemp(prefix="selftest-")
        try:
            start_copy = os.path.join(scratch, "start")
            shutil.copytree(fz.repo, start_copy)
            r["baseline"] = {}
            for c in task.spec["criterion"]:
                if not c.get("check"):
                    continue
                code, out = _check_runs_as_promised(start_copy, c)
                r["baseline"][c["id"]] = code
                want = c.get("baseline", "any")
                if want == "fails" and code == 0:
                    fail(f"{c['id']} passes on the start state but promises to fail")
                if want == "passes" and code != 0:
                    fail(f"{c['id']} fails on the start state but promises to pass: {out}")

            first = "ANSWER: " + fz.facts["commits"][0]["sha"]
            v0 = judge_dir(task, fz, start_copy, [first])
            r["hidden_on_start"] = {c["id"]: c["pass"] for c in v0}
            if all(c["pass"] for c in v0):
                fail("every hidden check passes on the start state")

            sol = os.path.join(scratch, "solution")
            shutil.copytree(fz.repo, sol)
            if os.path.isdir(task.solution):
                _overlay(task.solution, sol)
            for c in task.spec["criterion"]:
                if c.get("check") and task.kind != "investigation":
                    code, out = _check_runs_as_promised(sol, c)
                    if code != 0:
                        fail(f"{c['id']} fails on the reference solution: {out}")
            answers = [f"ANSWER: {fz.facts['culprit']}"] if task.wants_answer else []
            v1 = judge_dir(task, fz, sol, answers)
            r["hidden_on_solution"] = {c["id"]: c["pass"] for c in v1}
            bad = [c for c in v1 if not c["pass"]]
            if bad:
                fail(f"hidden checks fail on the reference solution: {bad}")
            changed = changed_paths(sol, fz.facts["base_tree"], worktree_tree(sol))
            r["solution_changes"] = changed
            if out_of_scope(changed, task.scope):
                fail(f"reference solution leaves the scope: {out_of_scope(changed, task.scope)}")

            r["near_misses"] = _selftest_near_misses(task, fz, start_copy, sol, scratch, fail)
            if not a.skip_cross and os.path.isdir(task.solution):
                r["cross_task"] = _selftest_cross(task, fz, start_copy, changed, all_tasks, frozen, scratch, fail)
            if task.wants_answer:
                r["culprit"] = fz.facts["culprit"]
                _selftest_history(task, fz, start_copy, fail, r)
        finally:
            shutil.rmtree(scratch, ignore_errors=True)

        if a.interlock_bin and os.path.exists(a.interlock_bin):
            r["interlock_baseline"] = _selftest_interlock(task, fz, a.interlock_bin, fail)
        print(f"{task.id:22} {'FAIL' if r.get('failures') else 'ok'}", flush=True)
    if a.out:
        write_json(a.out, {"harness_version": HARNESS_VERSION, "at": now_iso(), "lock": load_lock(),
                           "tasks": report, "failures": failures})
    for f in failures:
        print("  " + f)
    return 1 if failures else 0


def _selftest_near_misses(task, fz, start_copy, sol, scratch, fail):
    """Each near miss must fail at least one hidden case."""
    out = []
    for i, nm in enumerate(task.near_misses()):
        if "answers" in nm:
            commits = [c["sha"] for c in fz.facts["commits"]]
            idx = commits.index(fz.facts["culprit"])
            subs = {"culprit": commits[idx], "parent": commits[idx - 1],
                    "child": commits[idx + 1] if idx + 1 < len(commits) else commits[idx - 1]}
            answers = [s.format(**subs) for s in nm["answers"]]
            cases = judge_dir(task, fz, start_copy, answers)
            visible = {}
        else:
            d = os.path.join(scratch, f"near-{i}")
            shutil.copytree(sol if nm.get("from") == "solution" else start_copy, d)
            try:
                _apply_near_miss(d, nm)
            except RuntimeError as e:
                fail(str(e))
                continue
            cases = judge_dir(task, fz, d, [])
            visible = {c["id"]: _check_runs_as_promised(d, c)[0] for c in task.spec["criterion"] if c.get("check")}
        failing = [c["id"] for c in cases if not c["pass"]]
        out.append({"name": nm["name"], "visible_exit_codes": visible, "hidden_failing": failing})
        if not failing:
            fail(f"near miss {nm['name']!r} passes every hidden check")
    return out


def _selftest_cross(task, fz, start_copy, sol_paths, all_tasks, frozen, scratch, fail):
    """No other task's frozen repository, and no pristine sample project,
    holds this task's fix: their versions of the files the solution changes
    must not pass this task's hidden checks."""
    out = {}
    sources = [(u.id, frozen[u.id].repo) for u in all_tasks if u.repo == task.repo and u.id != task.id
               and u.id in frozen]
    sources.append(("repos/" + task.repo, os.path.join(TASKSET, "repos", task.repo)))
    for name, src in sources:
        d = os.path.join(scratch, f"cross-{re.sub(r'[^a-z0-9]+', '-', name)}")
        shutil.copytree(start_copy, d)
        took = []
        for p in sol_paths:
            if os.path.isfile(os.path.join(src, p)):
                os.makedirs(os.path.dirname(os.path.join(d, p)), exist_ok=True)
                shutil.copy(os.path.join(src, p), os.path.join(d, p))
                took.append(p)
        cases = judge_dir(task, fz, d, [])
        passed = bool(cases) and all(c["pass"] for c in cases)
        out[name] = {"files": took, "passes_hidden": passed}
        if passed:
            fail(f"{name} holds this task's fix: its {', '.join(took)} pass every hidden check")
        shutil.rmtree(d, ignore_errors=True)
    return out


def _selftest_history(task, fz, repo, fail, r):
    culprit = fz.facts["culprit"]
    script = os.path.join(repo, "checks", "settle-repro.sh")
    with open(script) as f:
        text = f.read()
    tmp = tempfile.mkdtemp()
    try:
        copy = os.path.join(tmp, "repro.sh")
        with open(copy, "w") as f:
            f.write(text)

        def ok_at(rev):
            git(repo, "checkout", "-q", rev)
            return subprocess.run(["sh", copy], cwd=repo, capture_output=True,
                                  env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1")).returncode == 0

        r["repro_at_culprit_parent"] = ok_at(culprit + "^")
        r["repro_at_culprit"] = ok_at(culprit)
        r["repro_at_head"] = ok_at(fz.facts["base_commit"])
        git(repo, "checkout", "-q", fz.facts["base_commit"])
        if not r["repro_at_culprit_parent"] or r["repro_at_culprit"] or r["repro_at_head"]:
            fail("the reproduction does not single out the culprit commit")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _selftest_interlock(task, fz, interlock_bin, fail):
    """`interlock task create` accepts the task file, and interlock's own
    baseline runs (as the operator, no host involved) see each check keep its
    promise, with real tests run."""
    tmp = tempfile.mkdtemp(prefix="selftest-il-")
    try:
        repo = os.path.join(tmp, task.repo)
        shutil.copytree(fz.repo, repo)
        env = {k: v for k, v in os.environ.items() if not k.startswith("INTERLOCK_")}
        env["GOTOOLCHAIN"] = "local"
        run_cmd([interlock_bin, "init"], cwd=repo, env=env)
        p = run_cmd([interlock_bin, "task", "create", os.path.join(task.dir, "task.toml")], cwd=repo, env=env,
                    check=False)
        if p.returncode != 0:
            fail(f"interlock task create refused the task: {p.stderr}")
            return "refused"
        run_cmd([interlock_bin, "task", "ready", task.id, "--base", fz.facts["base_commit"]], cwd=repo, env=env)
        seen = {}
        for c in task.spec["criterion"]:
            if not c.get("check") or c.get("baseline", "any") == "any":
                continue
            p = run_cmd([interlock_bin, "check", "run", "--criterion", c["id"], "--target", "base", "--operator",
                         "--task", task.id], cwd=repo, env=env, check=False)
            try:
                out = json.loads(p.stdout)
            except ValueError:
                fail(f"interlock check run {c['id']} failed: {p.stderr[-500:]}")
                continue
            seen[c["id"]] = {"passed": out["passed"], "failed": out["failed"], "checked_nothing": out["checked_nothing"]}
            kept = out["failed"] if c["baseline"] == "fails" else out["passed"]
            if out["checked_nothing"] or not kept:
                fail(f"interlock's baseline run of {c['id']} breaks its promise: {seen[c['id']]}")
        return seen
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def cmd_run(a):
    state = os.path.abspath(a.state_dir)
    results = os.path.abspath(a.results)
    run_base = os.path.abspath(a.run_base)
    os.makedirs(state, exist_ok=True)
    os.makedirs(results, exist_ok=True)
    check_layout(run_base, state, results)
    cfg = HostConfig(a, state)
    conditions = [c.strip() for c in a.conditions.split(",") if c.strip()]
    for c in conditions:
        if c not in CONDITIONS:
            sys.exit(f"unknown condition {c}")
    if not cfg.bin():
        sys.exit(f"{cfg.host} is not installed")
    if not os.path.exists(cfg.interlock_bin):
        sys.exit(f"no interlock binary at {cfg.interlock_bin} (needed for every condition: it supplies the worker's tools)")
    cfg.check_interlock_bin()
    if ("skills" in conditions or a.interlock_skills) and not (a.skills_dir or a.skills_generate):
        sys.exit("the skills condition and --interlock-skills need --skills-dir or --skills-generate")
    if a.interlock_skills and not cfg.features()["skills"]:
        sys.exit("this interlock's `run` has no --skills flag yet, so it cannot load the skills plugin")
    cfg.skills_plugin = prepare_skills(a, cfg)
    if cfg.host == "claude-code" and not cfg.fake_model and not a.dry_run:
        p = run_cmd([cfg.claude_bin, "auth", "status"], env=cfg.base_env(), check=False)
        try:
            if not json.loads(p.stdout).get("loggedIn"):
                sys.exit("claude reports it is not logged in with the harness's environment allowlist")
        except ValueError:
            sys.exit(f"cannot read `claude auth status` with the allowlisted environment: {p.stderr[-300:]}")
    prepare_run_base(run_base, state)
    tasks = load_tasks(a.tasks)

    manifest_path = os.path.join(results, "manifest.json")
    manifest = read_json(manifest_path) or {"created_at": now_iso(), "invocations": []}
    pinned = {"host": cfg.host, "host_version": cfg.version(), "model": cfg.model, "effort": cfg.effort,
              "max_turns": cfg.max_turns, "session_timeout_s": cfg.session_timeout_s,
              "max_sessions": cfg.max_sessions, "fake_model": bool(cfg.fake_model)}
    if manifest.get("pinned") and manifest["pinned"] != pinned and not a.allow_repin:
        sys.exit(f"this results directory is pinned to {manifest['pinned']}; got {pinned}")
    manifest.update(
        pinned=pinned,
        harness_version=HARNESS_VERSION,
        interlock_commit=git(EVAL_DIR, "rev-parse", "HEAD", check=False),
        interlock_describe=git(EVAL_DIR, "describe", "--always", "--dirty", "--abbrev=12", check=False),
        interlock_bin_sha256=sha256_file(cfg.interlock_bin),
        interlock_features=cfg.features(),
        budget_usd=a.budget_usd,
        frozen=load_lock(),
        env_allowlist=sorted(cfg.base_env()),
        skills_plugin=cfg.skills_plugin,
        skills_plugin_sha256=tree_digest(cfg.skills_plugin) if cfg.skills_plugin else None,
        environment={
            "python": sys.version.split()[0],
            "go": run_cmd(["go", "version"], check=False).stdout.strip(),
            "git": run_cmd(["git", "--version"], check=False).stdout.strip(),
        },
    )
    invocation = {"at": now_iso(), "argv": sys.argv[1:], "label": a.label, "runs": [], "stopped": None}
    manifest["invocations"].append(invocation)
    write_json(manifest_path, manifest)

    frozen = {t.id: Frozen(state, t, verify=not a.no_lock) for t in tasks}
    runner = Runner(cfg, state, run_base, results, a.label, a.seed)
    defaults = {"plain": a.default_plain_usd, "skills": a.default_plain_usd, "interlock": a.default_interlock_usd}
    count_labels = set(a.count_labels or [a.label])
    plan = []
    for repeat in range(1, a.repeats + 1):
        for task in tasks:
            plan += [(task, c, repeat) for c in condition_order(conditions, task.id, repeat, a.seed)]
    for task, condition, repeat in plan:
        stopped = run_until_counted(runner, task, frozen[task.id], condition, repeat, results, a.budget_usd,
                                    defaults, count_labels, a.interrupt_retries, cfg.fake_model, a.dry_run, invocation,
                                    lambda: write_json(manifest_path, manifest))
        if stopped:
            break
    write_json(manifest_path, manifest)
    return 0


def run_until_counted(runner, task, frozen, condition, repeat, results, budget_usd, defaults, count_labels, retries,
                      free, dry_run, invocation, save):
    """Runs one (task, condition, repeat) until a run counts. A forced
    interruption that did not land as designed is kept, flagged, and re-run
    up to `retries` times. Returns a reason when the budget stops the plan."""
    attempt = 0
    while True:
        runs = load_runs(results)
        same = [r for r in runs if r["task"] == task.id and r["condition"] == condition
                and r.get("label") in count_labels]
        if sum(1 for r in same if run_counts(r)) >= repeat:
            return None  # already run; earlier runs with a counted label are data
        tries = [r for r in same if r.get("repeat") == repeat]
        if attempt >= 1 + retries or len(tries) > retries:
            invocation.setdefault("gave_up", []).append(f"{task.id} {condition} r{repeat}")
            print(f"    giving up on {task.id} {condition} r{repeat}: the interruption never landed as designed")
            save()
            return None
        attempt += 1
        spent = round(sum(r.get("budget_charge_usd", 0) for r in runs), 4)
        est = 0.0 if free else estimate_next(runs, task, condition, defaults)
        if spent + est > budget_usd:
            invocation["stopped"] = (f"budget: spent ${spent:.2f}; the next run ({task.id}, {condition}) is "
                                     f"estimated at ${est:.2f}, which would pass the ${budget_usd:.2f} cap")
            print(invocation["stopped"])
            save()
            return invocation["stopped"]
        if dry_run:
            print(f"would run {task.id} {condition} r{repeat} (estimate ${est:.2f}, spent ${spent:.2f})")
            return None
        print(f"[{now_iso()}] {task.id} {condition} r{repeat} (spent ${spent:.2f}, estimate ${est:.2f})", flush=True)
        rec = runner.run_one(task, frozen, condition, repeat, len(tries) + 1)
        m = rec["metrics"]
        inter = rec.get("interruption")
        print(f"    accepted={m['accepted']} claimed={m['claimed_done']} hidden={m['hidden_pass']} "
              f"scope={rec['output']['scope_violations']} cost={m['cost_usd']} wall={m['wall_s']}s"
              + (f" interruption={'valid' if inter.get('valid') else 'NOT VALID: ' + str(inter.get('why_not_valid'))}"
                 if inter else "")
              + (" LEAK" if m["hidden_material_seen"] else ""), flush=True)
        invocation["runs"].append(rec["run_id"])
        save()
        if run_counts(rec):
            return None


# --------------------------------------------------------------------------
# Report


def _mean(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.mean(xs), 3) if xs else None


def _median(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 3) if xs else None


def upper_bound(k, n, conf=0.95, two_sided=False):
    """Exact (Clopper-Pearson) upper bound on a failure rate, for k = 0 only."""
    if n == 0 or k != 0:
        return None
    alpha = (1 - conf) / (2 if two_sided else 1)
    return round(1 - alpha ** (1 / n), 3)


def aggregate(runs):
    counted = [r for r in runs if run_counts(r)]
    flagged = [r for r in runs if not run_counts(r)]
    n = len(counted)
    m = [r["metrics"] for r in counted]
    costs = [x["cost_usd"] for x in m]
    complete = [r["metrics"] for r in counted if _completeness(r["sessions"])[0]]
    tok_complete = [r["metrics"] for r in counted if _completeness(r["sessions"])[1]]
    rec = [x["recovery"] for x in m if x.get("recovery")]
    tokens = {}
    for x in tok_complete:
        for k, v in (x.get("tokens") or {}).items():
            tokens[k] = tokens.get(k, 0) + v
    accepted = sum(x["accepted"] for x in m)
    return {
        "runs": n,
        "interruption_runs_not_landed": len(flagged),
        "accepted": accepted,
        "failure_rate_upper_95_one_sided": upper_bound(n - accepted, n),
        "failure_rate_upper_95_two_sided": upper_bound(n - accepted, n, two_sided=True),
        "hidden_pass": sum(bool(x["hidden_pass"]) for x in m),
        "claimed_done": sum(x["claimed_done"] for x in m),
        "false_claims": sum(x.get("false_claim", x.get("unsupported_claim", False)) for x in m),
        "claims_without_evidence": sum(bool(x.get("claim_without_evidence")) for x in m),
        "missed_defects": sum(x["missed_defects"] for x in m),
        "scope_violation_runs": sum(x["scope_violation"] for x in m),
        "correct_but_not_claimed": sum(x["correct_but_not_claimed"] for x in m),
        "recovery": {"interrupted_as_designed": sum(r.get("valid", r["interrupted"]) for r in rec),
                     "recovered": sum(r["recovered"] for r in rec)} if rec else None,
        "human_interventions": "n/a (headless)",
        "operator_needed": sum(x["operator_needed"] for x in m),
        "sessions_mean": _mean([x["sessions"] for x in m]),
        "wall_s_mean": _mean([x["wall_s"] for x in m]),
        "wall_s_median": _median([x["wall_s"] for x in m]),
        "cost_usd_total_observed": round(sum(c for c in costs if c is not None), 4),
        "cost_usd_mean_complete_runs": _mean([x["cost_usd"] for x in complete]),
        "runs_with_complete_cost": len(complete),
        "runs_with_complete_tokens": len(tok_complete),
        "tokens_complete_runs": tokens or None,
        "hidden_material_seen": sum(bool(x.get("hidden_material_seen")) for x in m),
        "suspicious_searches": sum(x.get("suspicious_searches", 0) for x in m),
        "runs_invoking_skills": sum(1 for r in counted if (r.get("tool_usage") or {}).get("skills_invoked")),
        "interlock_commands": sum((r.get("tool_usage") or {}).get("interlock_commands", 0) for r in counted),
    }


def _section(runs):
    conds = [c for c in CONDITIONS if any(r["condition"] == c for r in runs)]
    tasks = sorted({r["task"] for r in runs})
    return {
        "conditions": {c: aggregate([r for r in runs if r["condition"] == c]) for c in conds},
        "tasks": {t: {c: aggregate([r for r in runs if r["task"] == t and r["condition"] == c])
                      for c in conds if any(r["task"] == t and r["condition"] == c for r in runs)} for t in tasks},
    }


def cmd_report(a):
    results = os.path.abspath(a.results)
    runs = load_runs(results)
    if a.exclude_fake:
        runs = [r for r in runs if not r.get("fake_model")]
    manifest = read_json(os.path.join(results, "manifest.json"), {})
    spend = round(sum(r.get("budget_charge_usd", 0) for r in runs), 4)
    observed = round(sum(r["metrics"]["cost_usd"] or 0 for r in runs), 4)
    labels = sorted({r.get("label") for r in runs})
    out = {
        "generated_at": now_iso(),
        "harness_version": HARNESS_VERSION,
        "pinned": manifest.get("pinned"),
        "interlock_commit": manifest.get("interlock_commit"),
        "frozen_task_set": manifest.get("frozen"),
        "environment": manifest.get("environment"),
        "budget_usd": manifest.get("budget_usd"),
        "spend": {"observed_usd": observed, "budget_charged_usd": spend},
        "stopped": [i["stopped"] for i in manifest.get("invocations", []) if i.get("stopped")],
        "labels": labels,
        "all_runs": _section(runs),
        "excluding_pilot": _section([r for r in runs if r.get("label") != "pilot"]) if "pilot" in labels else None,
        "not_run": [c for c in CONDITIONS if not any(r["condition"] == c for r in runs)],
        "runs": [
            {k: r.get(k) for k in ("run_id", "label", "task", "kind", "condition", "repeat", "attempt", "wall_s")}
            | {"metrics": r["metrics"], "judge": r.get("judge"), "claim": r.get("claim"),
               "scope_violations": r["output"].get("scope_violations"),
               "interlock": {k: (r.get("interlock") or {}).get(k) for k in ("final_state", "signals",
                                                                            "stopped_because")}
               if r.get("interlock") else None,
               "interruption": r.get("interruption"), "leak_scan": r.get("leak_scan"), "notes": r.get("notes")}
            for r in runs
        ],
    }
    write_json(os.path.join(results, "report.json"), out)
    with open(os.path.join(results, "report.md"), "w") as f:
        f.write(render_markdown(out))
    print(os.path.join(results, "report.md"))
    return 0


def _rate(k, n):
    return f"{k}/{n}" if n else "-"


def _pct(x):
    return "-" if x is None else f"{100 * x:.1f}%"


ROWS = [
    ("Runs", lambda a: a["runs"]),
    ("Interruption runs that did not land as designed (re-run, not counted)", lambda a: a["interruption_runs_not_landed"]),
    ("Accepted outcomes", lambda a: _rate(a["accepted"], a["runs"])),
    ("Failure rate, exact 95% upper bound (one-sided / two-sided), when no run failed",
     lambda a: f"{_pct(a['failure_rate_upper_95_one_sided'])} / {_pct(a['failure_rate_upper_95_two_sided'])}"),
    ("Hidden checks pass (claimed or not)", lambda a: _rate(a["hidden_pass"], a["runs"])),
    ("Completion claimed", lambda a: _rate(a["claimed_done"], a["runs"])),
    ("False completion claims (claimed done, a hidden check fails)", lambda a: _rate(a["false_claims"], a["runs"])),
    ("Claims without evidence (claimed done, no check run in any transcript)",
     lambda a: _rate(a["claims_without_evidence"], a["runs"])),
    ("Missed defects (failed hidden cases in claimed outputs)", lambda a: a["missed_defects"]),
    ("Runs with scope violations", lambda a: _rate(a["scope_violation_runs"], a["runs"])),
    ("Correct output not claimed", lambda a: _rate(a["correct_but_not_claimed"], a["runs"])),
    ("Recovery after forced interruption",
     lambda a: _rate(a["recovery"]["recovered"], a["recovery"]["interrupted_as_designed"]) if a["recovery"] else "-"),
    ("Human interventions", lambda a: a["human_interventions"]),
    ("Ended needing an operator", lambda a: _rate(a["operator_needed"], a["runs"])),
    ("Sessions per run (mean)", lambda a: a["sessions_mean"]),
    ("Wall time, mean / median (s)", lambda a: f"{a['wall_s_mean']} / {a['wall_s_median']}"),
    ("Cost reported, total (USD)", lambda a: f"{a['cost_usd_total_observed']:.2f}"),
    ("Cost per run, mean over runs with complete cost (USD)",
     lambda a: "unavailable" if a["cost_usd_mean_complete_runs"] is None else f"{a['cost_usd_mean_complete_runs']:.3f}"),
    ("Runs with complete cost", lambda a: _rate(a["runs_with_complete_cost"], a["runs"])),
    ("Runs with complete token counts", lambda a: _rate(a.get("runs_with_complete_tokens", 0), a["runs"])),
    ("Output tokens, summed over runs with complete counts",
     lambda a: (a["tokens_complete_runs"] or {}).get("output", "unavailable")),
    ("Runs whose transcripts reach hidden material", lambda a: a["hidden_material_seen"]),
    ("Suspicious directory searches", lambda a: a["suspicious_searches"]),
    ("Runs that invoked a skill (descriptive)", lambda a: _rate(a.get("runs_invoking_skills", 0), a["runs"])),
    ("Shell commands calling interlock (descriptive)", lambda a: a.get("interlock_commands", 0)),
]


def _condition_table(section):
    conds = section["conditions"]
    lines = ["| Metric | " + " | ".join(conds) + " |", "| --- | " + " | ".join("---" for _ in conds) + " |"]
    for name, fn in ROWS:
        lines.append(f"| {name} | " + " | ".join(str(fn(conds[c])) for c in conds) + " |")
    return lines


def render_markdown(rep):
    p = rep.get("pinned") or {}
    lines = [
        "# Evaluation report",
        "",
        f"Generated {rep['generated_at']}. Host {p.get('host')} {p.get('host_version')}, model "
        f"`{p.get('model')}`, effort `{p.get('effort')}`, interlock at `{(rep.get('interlock_commit') or '')[:12]}`."
        + (" **Scripted fake model: plumbing only, not evaluation evidence.**" if p.get("fake_model") else ""),
        "",
        "Stand-in task set (not the S3 repository). Small n. One host, one model. Hidden checks are executable; "
        "no model judges anything.",
        "",
        f"Spend: ${rep['spend']['observed_usd']:.2f} reported by the host; ${rep['spend']['budget_charged_usd']:.2f} "
        f"charged against the ${rep.get('budget_usd') or 0:.2f} budget (includes reserves for sessions that never "
        "reported a cost).",
        "",
    ]
    if rep["stopped"]:
        lines += ["Stopped early: " + "; ".join(rep["stopped"]), ""]
    if rep["not_run"]:
        lines += [f"Not run: {', '.join(rep['not_run'])}.", ""]
    lines += ["## By condition, all runs", ""] + _condition_table(rep["all_runs"])
    if rep.get("excluding_pilot"):
        lines += ["", "## By condition, excluding pilot runs", ""] + _condition_table(rep["excluding_pilot"])
    lines += ["", "## By task", "", "| Task | Condition | Accepted | Claimed | False claims | Missed defects | "
              "Scope violations | Mean wall (s) | Cost reported (USD) |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for t, conds in rep["all_runs"]["tasks"].items():
        for c, a in conds.items():
            lines.append(f"| {t} | {c} | {_rate(a['accepted'], a['runs'])} | {_rate(a['claimed_done'], a['runs'])} | "
                         f"{a['false_claims']} | {a['missed_defects']} | {a['scope_violation_runs']} | "
                         f"{a['wall_s_mean']} | {a['cost_usd_total_observed']:.2f} |")
    lines += ["", "## Runs", "", "| Run | Label | Claimed | Hidden | Failed hidden cases | Scope violations | interlock | "
              "Interruption | Wall (s) | Cost (USD) |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for r in rep["runs"]:
        m = r["metrics"]
        il = r.get("interlock") or {}
        sig = " ".join(il.get("signals") or []) if il else ""
        inter = r.get("interruption")
        inter_text = "" if not inter else ("valid: " + str(inter.get("trigger")) if inter.get("valid") else
                                           "NOT VALID: " + str(inter.get("why_not_valid")))
        lines.append(
            f"| {r['run_id']} | {r.get('label')} | {m['claimed_done']} | {m['hidden_pass']} | "
            f"{', '.join((r.get('judge') or {}).get('failed') or []) or '-'} | "
            f"{', '.join(r.get('scope_violations') or []) or '-'} | {(il.get('final_state') or '') + (' ' + sig if sig else '')} | "
            f"{inter_text} | {m['wall_s']} | {m['cost_usd'] if m['cost_usd'] is not None else 'unavailable'} |"
        )
    lines += ["", "Definitions are in docs/evaluation.md.", ""]
    return "\n".join(lines)


# --------------------------------------------------------------------------
# Evidence export


def _secrets():
    vals = set()
    for k, v in os.environ.items():
        if v and len(v) >= 8 and re.search(r"TOKEN|KEY|SECRET|EMAIL|UUID|SESSION_ID|PASSWORD|AUTH", k):
            vals.add(v)
    return sorted(vals, key=len, reverse=True)


def sanitize(text, secret_values):
    for v in secret_values:
        text = text.replace(v, "<redacted>")
    text = re.sub(r"(INTERLOCK_TOKEN[=\"': ]+)[A-Za-z0-9_-]{8,}", r"\1<redacted>", text)
    text = re.sub(r"(--token[= ]+)[A-Za-z0-9_-]{8,}", r"\1<redacted>", text)
    text = re.sub(r"[A-Za-z0-9._%+-]+@(?!example\.org)[A-Za-z0-9.-]+\.[a-z]{2,}", "<email>", text)
    return text


def _slim_transcript(text):
    """Drops what a reader does not need and what could identify the account:
    the init event's environment details, and very long tool outputs."""
    out = []
    for line in text.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            out.append(line[:2000])
            continue
        if e.get("type") == "system" and e.get("subtype") == "init":
            e = {k: e.get(k) for k in ("type", "subtype", "model", "claude_code_version", "permissionMode", "tools",
                                         "cwd")}
        s = json.dumps(e)
        if len(s) > 6000:
            s = s[:6000] + "...(truncated)"
        out.append(s)
    return "\n".join(out) + "\n"


def cmd_export(a):
    results = os.path.abspath(a.results)
    dest = os.path.abspath(a.dest)
    secret_values = _secrets()
    limit = 195 * 1024
    os.makedirs(dest, exist_ok=True)
    for name in ("manifest.json", "report.json", "report.md"):
        src = os.path.join(results, name)
        if os.path.exists(src):
            with open(src) as f:
                text = sanitize(f.read(), secret_values)
            with open(os.path.join(dest, name), "w") as f:
                f.write(text)
    for r in load_runs(results):
        rdir = os.path.join(results, "runs", r["run_id"])
        odir = os.path.join(dest, "runs", r["run_id"])
        os.makedirs(odir, exist_ok=True)
        for root, _, files in os.walk(rdir):
            for f in files:
                src = os.path.join(root, f)
                rel = os.path.relpath(src, rdir)
                with open(src, errors="replace") as fh:
                    text = sanitize(fh.read(), secret_values)
                if rel.endswith(".jsonl"):
                    text = _slim_transcript(text)
                    data = gzip.compress(text.encode(), mtime=0)
                    while len(data) > limit:
                        text = text[: int(len(text) * 0.8)] + "\n...(truncated to fit the evidence size limit)\n"
                        data = gzip.compress(text.encode(), mtime=0)
                    path = os.path.join(odir, rel + ".gz")
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    with open(path, "wb") as fh:
                        fh.write(data)
                else:
                    if len(text) > limit:
                        text = text[:limit] + "\n...(truncated)\n"
                    path = os.path.join(odir, rel)
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    with open(path, "w") as fh:
                        fh.write(text)
        anon = (r.get("judge") or {}).get("anon_id")
        if anon:
            v = os.path.join(results, "judge", anon, "verdict.json")
            if os.path.exists(v):
                os.makedirs(os.path.join(dest, "judge", anon), exist_ok=True)
                with open(v) as fh:
                    text = sanitize(fh.read(), secret_values)
                with open(os.path.join(dest, "judge", anon, "verdict.json"), "w") as fh:
                    fh.write(text)
    print(dest)
    return 0


# --------------------------------------------------------------------------


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    tmp = tempfile.gettempdir()
    default_state = os.environ.get("EVAL_STATE_DIR", os.path.join(tmp, "interlock-eval-state"))
    default_run_base = os.environ.get("EVAL_RUN_BASE", os.path.join(tmp, "agent-ws"))

    b = sub.add_parser("build", help="build frozen task repositories; fail if any differs from the lock")
    b.add_argument("--state-dir", default=default_state)
    b.add_argument("--tasks", nargs="*")
    b.add_argument("--update-lock", action="store_true")

    s = sub.add_parser("selftest", help="check every task with no model calls")
    s.add_argument("--state-dir", default=default_state)
    s.add_argument("--tasks", nargs="*")
    s.add_argument("--interlock-bin")
    s.add_argument("--no-lock", action="store_true")
    s.add_argument("--skip-cross", action="store_true", help="skip the cross-task fix-exposure check")
    s.add_argument("--out")

    r = sub.add_parser("run", help="run conditions on tasks")
    r.add_argument("--host", choices=["claude-code", "copilot"], required=True)
    r.add_argument("--conditions", default="plain,interlock")
    r.add_argument("--tasks", nargs="*")
    r.add_argument("--repeats", type=int, default=1)
    r.add_argument("--model")
    r.add_argument("--effort", help="Claude Code effort level for every condition")
    r.add_argument("--max-turns", type=int, default=80)
    r.add_argument("--session-timeout-min", type=int, default=15)
    r.add_argument("--max-sessions", type=int, default=6)
    r.add_argument("--budget-usd", type=float, required=True)
    r.add_argument("--per-run-usd", type=float, default=3.0, help="--max-budget-usd for plain Claude Code runs")
    r.add_argument("--unreported-reserve-usd", type=float, default=UNREPORTED_SESSION_RESERVE_USD,
                   help="budget charge for a session that never reported its cost (bookkeeping, not a metric)")
    r.add_argument("--default-plain-usd", type=float, default=1.0)
    r.add_argument("--default-interlock-usd", type=float, default=2.5)
    r.add_argument("--results", required=True)
    r.add_argument("--state-dir", default=default_state)
    r.add_argument("--run-base", default=default_run_base,
                   help="a directory holding nothing but the run in progress")
    r.add_argument("--interlock-bin", default=os.path.join(EVAL_DIR, "..", "target", "debug", "interlock"))
    r.add_argument("--claude-bin")
    r.add_argument("--copilot-bin")
    r.add_argument("--pass-env", action="append", help="also pass this environment variable to runs")
    r.add_argument("--skills-dir", help="a skills directory or plugin for the skills condition")
    r.add_argument("--skills-generate", action="store_true",
                   help="generate the skills plugin with interlock (see --skills-generate-cmd)")
    r.add_argument("--skills-generate-cmd", default="{interlock} skills generate --target {target} --out {out}")
    r.add_argument("--interlock-skills", action="store_true",
                   help="load the skills plugin into interlock's sessions too (interlock run --skills)")
    r.add_argument("--interrupt-retries", type=int, default=2,
                   help="re-runs allowed when a forced interruption does not land as designed")
    r.add_argument("--fake-model", action="store_true", help="Copilot offline against the scripted model (plumbing)")
    r.add_argument("--label", default="main")
    r.add_argument("--count-labels", nargs="*", help="labels whose runs count toward --repeats (default: --label)")
    r.add_argument("--seed", type=int, default=13)
    r.add_argument("--no-lock", action="store_true")
    r.add_argument("--allow-repin", action="store_true")
    r.add_argument("--dry-run", action="store_true")

    rep = sub.add_parser("report", help="aggregate a results directory")
    rep.add_argument("--results", required=True)
    rep.add_argument("--exclude-fake", action="store_true")

    e = sub.add_parser("export", help="copy sanitized evidence, without output trees")
    e.add_argument("--results", required=True)
    e.add_argument("--dest", required=True)

    a = p.parse_args(argv)
    return {"build": cmd_build, "selftest": cmd_selftest, "run": cmd_run, "report": cmd_report,
            "export": cmd_export}[a.cmd](a) or 0


if __name__ == "__main__":
    sys.exit(main())
