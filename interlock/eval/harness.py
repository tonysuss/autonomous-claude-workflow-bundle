#!/usr/bin/env python3
"""Three-condition evaluation harness for interlock (design §12 P0 S3, §13).

Conditions, on the same frozen tasks, with the host version and model pinned:

  plain      the host's ordinary headless workflow: the task prompt, no interlock
  skills     the same, with generated interlock skills loaded (--skills-dir) and
             no runtime enforcement
  interlock  `interlock run <task> --host ...`

Subcommands:

  build      build the frozen task repositories and check them against the lock
  selftest   prove each task sound with no model calls: visible checks keep
             their baseline promises, hidden checks fail on the start state and
             pass on the reference solution, which stays inside the scope
  run        run conditions x tasks x repeats, judge each output, keep spend
             under the budget
  report     aggregate results into report.json and report.md
  export     copy sanitized evidence (no trees) into a directory

Python 3.11+, standard library only. See docs/evaluation.md.
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
TASKSET = os.path.join(EVAL_DIR, "taskset")
LOCK_FILE = os.path.join(TASKSET, "frozen.lock.json")
HARNESS_VERSION = "1"
CONDITIONS = ("plain", "skills", "interlock")
IGNORE = shutil.ignore_patterns("__pycache__", "*.pyc", ".pytest_cache")

# Variables that bind a child process to the session that launched the harness.
# They are dropped for every condition alike, so each run is a fresh session.
SESSION_VARS = {
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_REMOTE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_CODE_DIAGNOSTICS_FILE",
    "CLAUDE_CODE_DEBUG",
    "CLAUDE_AFTER_LAST_COMPACT",
    "CLAUDECODE",
    # CLAUDE_EFFORT is an output (Claude Code exports the active level to Bash
    # and hooks); CLAUDE_CODE_EFFORT_LEVEL is the input. Neither is inherited.
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_EFFORT_LEVEL",
}

# What a plain run is told about finishing. interlock's own state is its claim.
STATUS_SUFFIX = (
    "\n\nWhen you have finished, end your final message with a line `STATUS: DONE` if the task is "
    "complete, or `STATUS: NOT DONE: <reason>` if it is not."
)
RECOVERY_PREFIX = (
    "A previous session working on this task was interrupted before it finished. Any changes it "
    "made are still in the working directory.\n\n"
)

# Budget charge for a session whose cost the host never reported (killed or
# timed out before its result event). For the budget only; never a metric.
UNREPORTED_SESSION_RESERVE_USD = 0.75


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
    """Runs cmd in its own session, polling `watch` every second; a string
    from `watch` kills the whole tree with that reason."""
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
                p.wait(timeout=0.25 if watch else 1)
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
# Tasks and frozen repositories


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
        self.history = os.path.join(task_dir, "history.py")
        self.solution = os.path.join(task_dir, "hidden", "solution")

    @property
    def judge(self):
        return os.path.join(self.dir, self.meta["judge"])


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


def build_repo(task, dest):
    """Builds a task's starting repository deterministically: same inputs,
    same commit ids, on any machine."""
    if os.path.exists(dest):
        shutil.rmtree(dest)
    shutil.copytree(os.path.join(TASKSET, "repos", task.repo), dest, ignore=IGNORE)
    git(dest, "init", "-q", "-b", "main", env=_commit_env(0))
    commits = []
    if os.path.exists(task.history):
        spec = importlib.util.spec_from_file_location(f"history_{task.id}", task.history)
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


class Frozen:
    """A task's frozen repository, its facts, and its exported start tree."""

    def __init__(self, work, task, check_lock=True):
        self.repo = os.path.join(work, "frozen", task.id)
        self.facts_path = os.path.join(work, "frozen", f"{task.id}.facts.json")
        self.start_dir = os.path.join(work, "start", task.id)
        facts = read_json(self.facts_path)
        if facts is None or not os.path.isdir(self.repo):
            facts = build_repo(task, self.repo)
            write_json(self.facts_path, facts)
            if os.path.exists(self.start_dir):
                shutil.rmtree(self.start_dir)
            export_tree(self.repo, facts["base_tree"], self.start_dir)
        self.facts = facts
        locked = (read_json(LOCK_FILE) or {}).get(task.id)
        if check_lock and locked != facts["base_commit"]:
            raise SystemExit(
                f"{task.id}: built base commit {facts['base_commit']} does not match the lock ({locked}). "
                "Run `harness.py build --update-lock` if the task set changed on purpose."
            )


def export_tree(repo, tree, dest):
    os.makedirs(dest, exist_ok=True)
    archive = subprocess.run(["git", "archive", "--format=tar", tree], cwd=repo, capture_output=True, check=True)
    subprocess.run(["tar", "-x", "-C", dest], input=archive.stdout, check=True)


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
# Hosts


class HostConfig:
    def __init__(self, a):
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
        self.skills_dir = os.path.abspath(a.skills_dir) if a.skills_dir else None
        self.fake_model = a.fake_model
        self.extra_env = {}

    def bin(self):
        return self.claude_bin if self.host == "claude-code" else self.copilot_bin

    def version(self):
        try:
            out = run_cmd([self.bin(), "--version"], check=False, timeout=60).stdout.strip()
        except (OSError, subprocess.TimeoutExpired) as e:
            return f"unavailable: {e}"
        m = re.search(r"\d+\.\d+\.\d+", out)
        return m.group(0) if m else out

    def env(self):
        env = {k: v for k, v in os.environ.items() if k not in SESSION_VARS and not k.startswith("INTERLOCK_")}
        if self.effort:
            env["CLAUDE_CODE_EFFORT_LEVEL"] = self.effort
        env["GOTOOLCHAIN"] = "local"
        if self.host == "claude-code" and self.claude_bin:
            env["INTERLOCK_CLAUDE_BIN"] = self.claude_bin
        if self.host == "copilot" and self.copilot_bin:
            env["INTERLOCK_COPILOT_BIN"] = self.copilot_bin
            env["COPILOT_AUTO_UPDATE"] = "false"
        env.update(self.extra_env)
        return env

    def plain_command(self, prompt, plugin_dir=None, budget_usd=None):
        """The host's ordinary headless invocation. Tool set, permission mode
        and settings isolation mirror what interlock gives a worker, minus
        interlock's hooks."""
        if self.host == "claude-code":
            tools = ["Read", "Grep", "Glob", "Edit", "Write", "NotebookEdit", "Bash"]
            if plugin_dir:
                tools.append("Skill")
            cmd = [
                self.claude_bin, "-p", "--output-format", "stream-json", "--verbose",
                "--permission-mode", "dontAsk", "--setting-sources", "", "--no-session-persistence",
                "--max-turns", str(self.max_turns),
            ]
            if self.model:
                cmd += ["--model", self.model]
            if budget_usd:
                cmd += ["--max-budget-usd", f"{budget_usd:.2f}"]
            if plugin_dir:
                cmd += ["--plugin-dir", plugin_dir]
            cmd += ["--allowedTools", *tools]
            return cmd, prompt
        cmd = [
            self.copilot_bin, "-p", prompt, "--output-format", "json", "--no-ask-user", "--no-auto-update",
            "--allow-tool=write", "--allow-tool=shell",
        ]
        if plugin_dir:
            cmd += ["--plugin-dir", plugin_dir, "--allow-tool=skill"]
        if self.model:
            cmd += ["--model", self.model]
        return cmd, None

    def interlock_command(self, task_id):
        cmd = [
            self.interlock_bin, "run", task_id, "--host", self.host,
            "--timeout", f"{self.session_timeout_s}s", "--max-turns", str(self.max_turns),
            "--max-sessions", str(self.max_sessions),
        ]
        if self.model:
            cmd += ["--model", self.model]
        return cmd


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
    else:
        # A killed session never reports its cost. Its per-message usage is a
        # lower bound on tokens; cost stays unavailable.
        per_msg = {}
        for e in events:
            m = e.get("message") if e.get("type") == "assistant" else None
            if isinstance(m, dict) and m.get("id") and isinstance(m.get("usage"), dict):
                per_msg[m["id"]] = m["usage"]
        if per_msg:
            s["tokens_lower_bound"] = {
                "input": sum(u.get("input_tokens", 0) for u in per_msg.values()),
                "output": sum(u.get("output_tokens", 0) for u in per_msg.values()),
                "cache_read": sum(u.get("cache_read_input_tokens", 0) for u in per_msg.values()),
                "cache_write": sum(u.get("cache_creation_input_tokens", 0) for u in per_msg.values()),
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
    # Copilot CLI reports premium requests, not dollars; cost stays unavailable.
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


# --------------------------------------------------------------------------
# Running one (task, condition, repeat)


class Runner:
    def __init__(self, cfg, work, results, label):
        self.cfg = cfg
        self.work = os.path.abspath(work)
        self.results = os.path.abspath(results)
        self.label = label

    def _workdir(self, task):
        # Neutral names: an agent sees nothing about the condition in its path.
        root = os.path.join(self.work, "ws", secrets.token_hex(5))
        return root, os.path.join(root, task.repo)

    def _skills_plugin(self, root):
        """Wraps a skills directory as a plugin both hosts load with --plugin-dir."""
        src = self.cfg.skills_dir
        if os.path.exists(os.path.join(src, ".claude-plugin", "plugin.json")):
            return src
        plugin = os.path.join(root, "skills-plugin")
        os.makedirs(os.path.join(plugin, ".claude-plugin"))
        with open(os.path.join(plugin, ".claude-plugin", "plugin.json"), "w") as f:
            json.dump({"name": "interlock-skills-eval", "version": "0.0.0"}, f)
        skills = os.path.join(src, "skills") if os.path.isdir(os.path.join(src, "skills")) else src
        shutil.copytree(skills, os.path.join(plugin, "skills"))
        return plugin

    def _edit_watch(self, dirs_fn, deadline_s):
        """Fires at the first change to the agent's working files, or at the deadline."""
        start = time.monotonic()
        state = {}

        def watch():
            for d in dirs_fn():
                if not os.path.isdir(d):
                    continue
                p = subprocess.run(
                    ["git", "--no-optional-locks", "status", "--porcelain", "-uall"],
                    cwd=d, capture_output=True, text=True,
                )
                if p.returncode == 0 and p.stdout.strip():
                    state["trigger"] = "first-edit"
                    state["after_s"] = round(time.monotonic() - start, 1)
                    state["changes"] = p.stdout.strip().splitlines()[:10]
                    return "interrupted at the first edit"
            if time.monotonic() - start > deadline_s:
                state["trigger"] = "deadline"
                state["after_s"] = round(time.monotonic() - start, 1)
                return f"interrupted at the {deadline_s}s deadline"
            return None

        return watch, state

    def run_one(self, task, frozen, condition, repeat):
        run_id = f"{task.id}.{condition}.r{repeat}.{secrets.token_hex(3)}"
        run_dir = os.path.join(self.results, "runs", run_id)
        raw = os.path.join(run_dir, "raw")
        os.makedirs(raw)
        root, repo = self._workdir(task)
        os.makedirs(root)
        shutil.copytree(frozen.repo, repo, symlinks=True)
        env = self.cfg.env()
        fake = None
        if self.cfg.fake_model:
            fake = self._start_fake(task, frozen, condition, env)
        record = {
            "run_id": run_id,
            "label": self.label,
            "task": task.id,
            "kind": task.kind,
            "language": task.language,
            "condition": condition,
            "repeat": repeat,
            "host": self.cfg.host,
            "host_version": self.cfg.version(),
            "model": self.cfg.model,
            "effort": self.cfg.effort,
            "fake_model": bool(fake),
            "started_at": now_iso(),
            "workdir": repo,
            "base_commit": frozen.facts["base_commit"],
            "sessions": [],
            "notes": [],
        }
        started = time.monotonic()
        try:
            if condition == "interlock":
                out = self._run_interlock(task, repo, root, raw, env, record)
            else:
                out = self._run_plain(task, repo, root, raw, env, record, condition)
        finally:
            if fake:
                fake.close()
        record["wall_s"] = round(time.monotonic() - started, 1)
        self._finish(task, frozen, repo, raw, record, out)
        write_json(os.path.join(run_dir, "run.json"), record)
        return record

    # -- plain and skills ----------------------------------------------------

    def _run_plain(self, task, repo, root, raw, env, record, condition):
        plugin = None
        if condition == "skills":
            if not self.cfg.skills_dir:
                raise SystemExit("the skills condition needs --skills-dir")
            plugin = self._skills_plugin(root)
            record["skills_dir"] = self.cfg.skills_dir
        prompt = task.prompt + STATUS_SUFFIX
        timeout = self.cfg.session_timeout_s
        budget = self.cfg.per_run_usd
        watch, wstate = (None, {})
        if task.interrupt:
            watch, wstate = self._edit_watch(lambda: [repo], task.interrupt.get("deadline_s", 300))
        cmd, stdin = self.cfg.plain_command(prompt, plugin, budget)
        res = run_proc(cmd, repo, env, stdin, os.path.join(raw, "session-1.jsonl"),
                       os.path.join(raw, "session-1.stderr.txt"), timeout, watch)
        s = parse_session(self.cfg.host, os.path.join(raw, "session-1.jsonl"))
        s.update(role=condition, exit_code=res["exit_code"], killed=res["killed"], wall_s=res["wall_s"],
                 transcript="raw/session-1.jsonl")
        record["sessions"].append(s)
        final = s
        if task.interrupt:
            record["interruption"] = {"interrupted": bool(res["killed"]), **wstate,
                                      "killed_processes": res["killed_pids"]}
            if res["killed"]:
                # The ordinary recovery: run the task again in the same working copy.
                spent = s["cost_usd"] or 0
                cmd, stdin = self.cfg.plain_command(RECOVERY_PREFIX + prompt, plugin,
                                                    max(budget - spent, 0.5) if budget else None)
                res2 = run_proc(cmd, repo, env, stdin, os.path.join(raw, "session-2.jsonl"),
                                os.path.join(raw, "session-2.stderr.txt"), timeout)
                s2 = parse_session(self.cfg.host, os.path.join(raw, "session-2.jsonl"))
                s2.update(role=f"{condition}-recovery", exit_code=res2["exit_code"], killed=res2["killed"],
                          wall_s=res2["wall_s"], transcript="raw/session-2.jsonl")
                record["sessions"].append(s2)
                final = s2
        claimed, detail = status_claim(final.get("final_text")) if final["complete"] and not final["is_error"] \
            else (False, "the session did not finish normally")
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

    def _run_interlock(self, task, repo, root, raw, env, record):
        if self.cfg.skills_dir:
            record["notes"].append(
                "interlock run loads its own role prompts and hooks plugin; generated skills are not passed to it"
            )
        task_file = os.path.join(root, "task.toml")
        shutil.copy(os.path.join(task.dir, "task.toml"), task_file)
        self._interlock(repo, env, "init")
        self._interlock(repo, env, "task", "create", task_file)
        cmd = self.cfg.interlock_command(task.id)
        run_timeout = self.cfg.session_timeout_s * (self.cfg.max_sessions + 1) + 600
        worktrees = os.path.join(repo, ".interlock", "worktrees")

        def worker_dirs():
            if not os.path.isdir(worktrees):
                return []
            return [os.path.join(worktrees, d) for d in os.listdir(worktrees) if "-worker-" in d]

        invocations = []
        watch, wstate = (None, {})
        if task.interrupt:
            watch, wstate = self._edit_watch(worker_dirs, task.interrupt.get("deadline_s", 300))
        res = run_proc(cmd, repo, env, None, os.path.join(raw, "run-1.json"), os.path.join(raw, "run-1.stderr.txt"),
                       run_timeout, watch)
        invocations.append(res)
        if task.interrupt:
            record["interruption"] = {"interrupted": bool(res["killed"]), **wstate,
                                      "killed_processes": res["killed_pids"]}
            if res["killed"]:
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

        # One session per attempt transcript, in start order.
        tdir = os.path.join(repo, ".interlock", "transcripts")
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
        tree = task_state.get("current_tree")
        return {"tree": tree, "delivered": state == "done", "answer_text": summary}

    # -- judging -----------------------------------------------------------------

    def _finish(self, task, frozen, repo, raw, record, out):
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

        claimed = record["claim"]["claimed_done"]
        hidden_pass = bool(verdict and verdict["pass"])
        failed = len(record["judge"]["failed"])
        complete = all(s["complete"] for s in record["sessions"]) and bool(record["sessions"])
        costs = [s["cost_usd"] for s in record["sessions"] if s.get("cost_usd") is not None]
        tokens = {}
        for s in record["sessions"]:
            for k, v in (s.get("tokens") or {}).items():
                tokens[k] = tokens.get(k, 0) + v
        delivered = out["delivered"] and claimed
        record["metrics"] = {
            "accepted": bool(delivered and hidden_pass),
            "hidden_pass": hidden_pass if tree else None,
            "claimed_done": claimed,
            "unsupported_claim": bool(claimed and not hidden_pass),
            "missed_defects": failed if claimed else 0,
            "scope_violation": bool(record["output"]["scope_violations"]),
            "correct_but_not_claimed": bool(hidden_pass and not claimed),
            "recovery": None,
            "human_interventions": 0,
            "operator_needed": not claimed,
            "restarts": 1 if (record.get("interruption") or {}).get("interrupted") else 0,
            "sessions": len(record["sessions"]),
            "wall_s": record["wall_s"],
            "cost_usd": round(sum(costs), 4) if costs else None,
            "cost_complete": complete,
            "tokens": tokens or None,
            "tokens_complete": complete,
            "hidden_material_seen": hidden_seen(raw),
        }
        if task.interrupt:
            inter = record.get("interruption") or {}
            record["metrics"]["recovery"] = {
                "interrupted": bool(inter.get("interrupted")),
                "trigger": inter.get("trigger"),
                "recovered": bool(inter.get("interrupted") and record["metrics"]["accepted"]),
            }
        unreported = sum(1 for s in record["sessions"] if s.get("cost_usd") is None and s.get("events"))
        if record["fake_model"]:
            unreported = 0  # a scripted model costs nothing
        record["budget_charge_usd"] = round(sum(costs) + unreported * UNREPORTED_SESSION_RESERVE_USD, 4)
        if unreported:
            record["notes"].append(
                f"{unreported} session(s) never reported a cost; the budget reserves "
                f"${UNREPORTED_SESSION_RESERVE_USD} each, which is not a measurement"
            )

    def _start_fake(self, task, frozen, condition, env):
        sys.path.insert(0, EVAL_DIR)
        from fake_model import FakeModel

        # The scripted fix writes the reference solution from inside the
        # working directory: Copilot refuses to read paths outside it.
        fix = []
        if os.path.isdir(task.solution):
            import base64

            for root, _, files in os.walk(task.solution):
                for name in sorted(files):
                    path = os.path.join(root, name)
                    rel = os.path.relpath(path, task.solution)
                    with open(path, "rb") as f:
                        b64 = base64.b64encode(f.read()).decode()
                    fix.append(f"mkdir -p ./{os.path.dirname(rel) or '.'} && echo {b64} | base64 -d > ./{rel}")
        fix = fix or ["true"]
        if task.interrupt:
            # Give the interruption watcher time to see the first write.
            fix = fix[:1] + ["sleep 3"] + fix[1:]
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
            "plain": {"steps": fix, "final": f"Applied the scripted change.\n{answer}STATUS: DONE"},
            "worker": {"steps": worker + claims, "final": f"Applied the scripted change.\n{answer}"},
            "verifier": {"steps": verifier, "final": "Scripted verification finished."},
            "on_block": claims,
        }
        fake = FakeModel(script)
        home = tempfile.mkdtemp(prefix="copilot-home-")
        env.update(
            COPILOT_OFFLINE="true",
            COPILOT_PROVIDER_BASE_URL=fake.base_url,
            COPILOT_MODEL="gpt-4.1",
            COPILOT_HOME=home,
            NO_PROXY="127.0.0.1,localhost",
            no_proxy="127.0.0.1,localhost",
        )
        return fake


def hidden_seen(raw):
    """Whether a transcript mentions the hidden material's location, which
    would mean an agent could have read the judge's checks."""
    needles = [os.path.join(EVAL_DIR, "taskset"), "hidden/judge.py", "zz_hidden", "hidden_split", "hidden_parse",
               "hidden_csv"]
    for root, _, files in os.walk(raw):
        for f in files:
            try:
                with open(os.path.join(root, f), errors="replace") as fh:
                    text = fh.read()
            except OSError:
                continue
            if any(n in text for n in needles):
                return True
    return False


def judge(task, frozen, repo, tree, answers, results):
    """Runs a task's hidden checks on an anonymized copy of the output. The
    judged directory holds the output's files and its ANSWER lines, under a
    random id: nothing in it names the condition."""
    anon = secrets.token_hex(6)
    jdir = os.path.join(results, "judge", anon)
    export_tree(repo, tree, os.path.join(jdir, "tree"))
    with open(os.path.join(jdir, "answer.txt"), "w") as f:
        f.write("\n".join(answers) + ("\n" if answers else ""))
    scratch = tempfile.mkdtemp(prefix="judge-")
    try:
        shutil.copytree(os.path.join(jdir, "tree"), os.path.join(scratch, "tree"))
        env = dict(os.environ, GOTOOLCHAIN="local", PYTHONDONTWRITEBYTECODE="1")
        for k in list(env):
            if k.startswith("INTERLOCK_"):
                env.pop(k)
        p = subprocess.run(
            [sys.executable, task.judge, "--tree", os.path.join(scratch, "tree"), "--start", frozen.start_dir,
             "--answer", os.path.join(jdir, "answer.txt"), "--facts", frozen.facts_path],
            cwd=scratch, env=env, capture_output=True, text=True, timeout=1800,
        )
        try:
            cases = json.loads(p.stdout)["cases"]
        except (ValueError, KeyError):
            cases = [{"id": "judge", "pass": False, "detail": f"judge exited {p.returncode}: {p.stderr[-1500:]}"}]
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    words = condition_words(jdir)
    verdict = {"anon_id": anon, "task": task.id, "cases": cases, "pass": bool(cases) and all(c["pass"] for c in cases),
               "condition_words": words}
    write_json(os.path.join(jdir, "verdict.json"), verdict)
    return verdict


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
# Budget


def load_runs(results):
    runs = []
    root = os.path.join(results, "runs")
    if os.path.isdir(root):
        for d in sorted(os.listdir(root)):
            r = read_json(os.path.join(root, d, "run.json"))
            if r:
                runs.append(r)
    return runs


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


# --------------------------------------------------------------------------
# Subcommands


def cmd_build(a):
    work = os.path.abspath(a.work_dir)
    lock = read_json(LOCK_FILE) or {}
    for task in load_tasks(a.tasks):
        facts = build_repo(task, os.path.join(work, "frozen", task.id))
        write_json(os.path.join(work, "frozen", f"{task.id}.facts.json"), facts)
        start = os.path.join(work, "start", task.id)
        if os.path.exists(start):
            shutil.rmtree(start)
        export_tree(os.path.join(work, "frozen", task.id), facts["base_tree"], start)
        state = "ok" if lock.get(task.id) == facts["base_commit"] else "CHANGED"
        print(f"{task.id:22} {facts['base_commit']}  {state}")
        lock[task.id] = facts["base_commit"] if a.update_lock else lock.get(task.id)
    if a.update_lock:
        write_json(LOCK_FILE, {k: v for k, v in sorted(lock.items()) if v})
        print(f"wrote {LOCK_FILE}")


def _check_runs_as_promised(repo, criterion):
    p = subprocess.run(["sh", "-c", criterion["check"]], cwd=repo, capture_output=True, text=True, timeout=600,
                       env=dict(os.environ, GOTOOLCHAIN="local", PYTHONDONTWRITEBYTECODE="1"))
    return p.returncode, (p.stdout + p.stderr)[-800:]


def cmd_selftest(a):
    """No model calls. Proves each task is sound before any money is spent."""
    work = os.path.abspath(a.work_dir)
    report, failures = {}, []
    for task in load_tasks(a.tasks):
        r = report[task.id] = {}
        frozen = Frozen(work, task, check_lock=not a.no_lock)

        def fail(msg):
            failures.append(f"{task.id}: {msg}")
            r.setdefault("failures", []).append(msg)

        if task.spec["intent"] != task.prompt:
            fail("task.toml intent differs from prompt.md")
        if task.spec.get("scope", {}).get("paths", []) != task.scope:
            if not (task.kind == "investigation" and not task.scope):
                fail("task.toml scope differs from meta.toml scope")
        files = run_cmd(["git", "ls-files"], cwd=frozen.repo).stdout
        if re.search(r"hidden|judge", files):
            fail("hidden material in the frozen repository")
        if "interlock" in run_cmd(["git", "grep", "-il", "interlock"], cwd=frozen.repo, check=False).stdout.lower():
            fail("the word 'interlock' appears in the frozen repository")

        # Visible checks keep their baseline promises on the start state.
        scratch = tempfile.mkdtemp(prefix="selftest-")
        try:
            start_copy = os.path.join(scratch, "start")
            shutil.copytree(frozen.repo, start_copy)
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

            # Hidden checks fail on the start state.
            v0 = judge_dir(task, frozen, start_copy, ["ANSWER: " + frozen.facts["commits"][0]["sha"]])
            r["hidden_on_start"] = {c["id"]: c["pass"] for c in v0}
            if all(c["pass"] for c in v0):
                fail("every hidden check passes on the start state")

            # The reference solution passes everything and stays in scope.
            sol = os.path.join(scratch, "solution")
            shutil.copytree(frozen.repo, sol)
            if os.path.isdir(task.solution):
                _overlay(task.solution, sol)
            for c in task.spec["criterion"]:
                if c.get("check") and task.kind != "investigation":
                    code, out = _check_runs_as_promised(sol, c)
                    if code != 0:
                        fail(f"{c['id']} fails on the reference solution: {out}")
            answers = [f"ANSWER: {frozen.facts['culprit']}"] if task.wants_answer else []
            v1 = judge_dir(task, frozen, sol, answers)
            r["hidden_on_solution"] = {c["id"]: c["pass"] for c in v1}
            bad = [c for c in v1 if not c["pass"]]
            if bad:
                fail(f"hidden checks fail on the reference solution: {bad}")
            changed = changed_paths(sol, frozen.facts["base_tree"], worktree_tree(sol))
            r["solution_changes"] = changed
            if out_of_scope(changed, task.scope):
                fail(f"reference solution leaves the scope: {out_of_scope(changed, task.scope)}")

            if task.wants_answer:
                r["culprit"] = frozen.facts["culprit"]
                _selftest_history(task, frozen, start_copy, fail, r)
        finally:
            shutil.rmtree(scratch, ignore_errors=True)

        if a.interlock_bin and os.path.exists(a.interlock_bin):
            r["interlock_task_create"] = _selftest_interlock(task, frozen, a.interlock_bin, fail)
        print(f"{task.id:22} {'FAIL' if r.get('failures') else 'ok'}")
    out = os.path.join(a.out) if a.out else None
    if out:
        write_json(out, {"harness_version": HARNESS_VERSION, "at": now_iso(), "tasks": report, "failures": failures})
    for f in failures:
        print("  " + f)
    return 1 if failures else 0


def judge_dir(task, frozen, tree_dir, answers):
    scratch = tempfile.mkdtemp(prefix="judge-")
    try:
        t = os.path.join(scratch, "tree")
        shutil.copytree(tree_dir, t, ignore=shutil.ignore_patterns(".git"))
        ans = os.path.join(scratch, "answer.txt")
        with open(ans, "w") as f:
            f.write("\n".join(answers) + "\n")
        p = subprocess.run(
            [sys.executable, task.judge, "--tree", t, "--start", frozen.start_dir, "--answer", ans,
             "--facts", frozen.facts_path],
            cwd=scratch, capture_output=True, text=True, timeout=1800,
            env=dict(os.environ, GOTOOLCHAIN="local", PYTHONDONTWRITEBYTECODE="1"),
        )
        try:
            return json.loads(p.stdout)["cases"]
        except (ValueError, KeyError):
            return [{"id": "judge", "pass": False, "detail": p.stderr[-1500:]}]
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def _selftest_history(task, frozen, repo, fail, r):
    culprit = frozen.facts["culprit"]
    repro = [c for c in task.spec.get("criterion", []) if c.get("check")]
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
            return subprocess.run(["sh", copy], cwd=repo, capture_output=True).returncode == 0

        r["repro_at_culprit_parent"] = ok_at(culprit + "^")
        r["repro_at_culprit"] = ok_at(culprit)
        r["repro_at_head"] = ok_at(frozen.facts["base_commit"])
        git(repo, "checkout", "-q", frozen.facts["base_commit"])
        if not r["repro_at_culprit_parent"] or r["repro_at_culprit"] or r["repro_at_head"]:
            fail("the reproduction does not single out the culprit commit")
        wrong = judge_dir(task, frozen, repo, [f"ANSWER: {git(repo, 'rev-parse', culprit + '^')}"])
        if all(c["pass"] for c in wrong):
            fail("the judge accepts the culprit's parent")
        r["visible_criteria_with_checks"] = len(repro)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _selftest_interlock(task, frozen, interlock_bin, fail):
    """`interlock task create` accepts the task file, and interlock's own
    baseline runs (as the operator, no host involved) see each check keep its
    promise, with real tests run."""
    tmp = tempfile.mkdtemp(prefix="selftest-il-")
    try:
        repo = os.path.join(tmp, task.repo)
        shutil.copytree(frozen.repo, repo)
        env = {k: v for k, v in os.environ.items() if not k.startswith("INTERLOCK_")}
        env["GOTOOLCHAIN"] = "local"
        run_cmd([interlock_bin, "init"], cwd=repo, env=env)
        p = run_cmd([interlock_bin, "task", "create", os.path.join(task.dir, "task.toml")], cwd=repo, env=env,
                    check=False)
        if p.returncode != 0:
            fail(f"interlock task create refused the task: {p.stderr}")
            return "refused"
        run_cmd([interlock_bin, "task", "ready", task.id, "--base", frozen.facts["base_commit"]], cwd=repo, env=env)
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
    cfg = HostConfig(a)
    conditions = [c.strip() for c in a.conditions.split(",") if c.strip()]
    for c in conditions:
        if c not in CONDITIONS:
            sys.exit(f"unknown condition {c}")
    if "skills" in conditions and not cfg.skills_dir:
        sys.exit("condition 'skills' needs --skills-dir: the generated skills do not exist yet, so it is not run")
    if not cfg.bin():
        sys.exit(f"{cfg.host} is not installed")
    if "interlock" in conditions and not os.path.exists(cfg.interlock_bin):
        sys.exit(f"no interlock binary at {cfg.interlock_bin}")
    tasks = load_tasks(a.tasks)
    results = os.path.abspath(a.results)
    os.makedirs(results, exist_ok=True)
    manifest_path = os.path.join(results, "manifest.json")
    manifest = read_json(manifest_path) or {"created_at": now_iso(), "invocations": []}
    pinned = {"host": cfg.host, "host_version": cfg.version(), "model": cfg.model, "effort": cfg.effort,
              "max_turns": cfg.max_turns, "session_timeout_s": cfg.session_timeout_s,
              "max_sessions": cfg.max_sessions, "fake_model": bool(cfg.fake_model)}
    if manifest.get("pinned") and manifest["pinned"] != pinned and not a.allow_repin:
        sys.exit(f"this results directory is pinned to {manifest['pinned']}; got {pinned}")
    manifest["pinned"] = pinned
    manifest["harness_version"] = HARNESS_VERSION
    manifest["interlock_commit"] = git(EVAL_DIR, "rev-parse", "HEAD", check=False)
    manifest["interlock_bin_sha256"] = _sha256(cfg.interlock_bin) if os.path.exists(cfg.interlock_bin) else None
    manifest["budget_usd"] = a.budget_usd
    manifest["frozen"] = read_json(LOCK_FILE)
    manifest["environment"] = {
        "python": sys.version.split()[0],
        "go": run_cmd(["go", "version"], check=False).stdout.strip(),
        "git": run_cmd(["git", "--version"], check=False).stdout.strip(),
    }
    invocation = {"at": now_iso(), "argv": sys.argv[1:], "label": a.label, "runs": [], "stopped": None}
    manifest["invocations"].append(invocation)
    write_json(manifest_path, manifest)

    frozen = {t.id: Frozen(os.path.abspath(a.work_dir), t, check_lock=not a.no_lock) for t in tasks}
    runner = Runner(cfg, a.work_dir, results, a.label)
    rng = random.Random(a.seed)
    defaults = {"plain": a.default_plain_usd, "skills": a.default_plain_usd, "interlock": a.default_interlock_usd}
    plan = []
    for repeat in range(1, a.repeats + 1):
        for task in tasks:
            order = list(conditions)
            rng.shuffle(order)
            plan += [(task, c, repeat) for c in order]
    for task, condition, repeat in plan:
        runs = load_runs(results)
        have = [r for r in runs if r["task"] == task.id and r["condition"] == condition]
        if len(have) >= repeat:
            continue  # already run; repeats count existing runs, so pilots are data
        spent = round(sum(r.get("budget_charge_usd", 0) for r in runs), 4)
        est = 0.0 if cfg.fake_model else estimate_next(runs, task, condition, defaults)
        if spent + est > a.budget_usd:
            invocation["stopped"] = (f"budget: spent ${spent:.2f}; the next run ({task.id}, {condition}) is "
                                     f"estimated at ${est:.2f}, which would pass the ${a.budget_usd:.2f} cap")
            print(invocation["stopped"])
            break
        if a.dry_run:
            print(f"would run {task.id} {condition} r{repeat} (estimate ${est:.2f}, spent ${spent:.2f})")
            continue
        print(f"[{now_iso()}] {task.id} {condition} r{repeat} (spent ${spent:.2f}, estimate ${est:.2f})", flush=True)
        rec = runner.run_one(task, frozen[task.id], condition, repeat)
        m = rec["metrics"]
        print(f"    accepted={m['accepted']} claimed={m['claimed_done']} hidden={m['hidden_pass']} "
              f"scope={rec['output']['scope_violations']} cost={m['cost_usd']} wall={m['wall_s']}s", flush=True)
        invocation["runs"].append(rec["run_id"])
        write_json(manifest_path, manifest)
    write_json(manifest_path, manifest)
    return 0


def _sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# --------------------------------------------------------------------------
# Report


def _mean(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.mean(xs), 3) if xs else None


def _median(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 3) if xs else None


def aggregate(runs):
    n = len(runs)
    m = [r["metrics"] for r in runs]
    costs = [x["cost_usd"] for x in m]
    complete = [x for x in m if x["cost_complete"]]
    rec = [x["recovery"] for x in m if x.get("recovery")]
    tokens = {}
    for x in complete:
        for k, v in (x.get("tokens") or {}).items():
            tokens[k] = tokens.get(k, 0) + v
    return {
        "runs": n,
        "accepted": sum(x["accepted"] for x in m),
        "hidden_pass": sum(bool(x["hidden_pass"]) for x in m),
        "claimed_done": sum(x["claimed_done"] for x in m),
        "unsupported_claims": sum(x["unsupported_claim"] for x in m),
        "missed_defects": sum(x["missed_defects"] for x in m),
        "scope_violation_runs": sum(x["scope_violation"] for x in m),
        "correct_but_not_claimed": sum(x["correct_but_not_claimed"] for x in m),
        "recovery": {"interrupted": sum(r["interrupted"] for r in rec), "recovered": sum(r["recovered"] for r in rec)}
        if rec else None,
        "human_interventions": sum(x["human_interventions"] for x in m),
        "operator_needed": sum(x["operator_needed"] for x in m),
        "sessions_mean": _mean([x["sessions"] for x in m]),
        "wall_s_mean": _mean([x["wall_s"] for x in m]),
        "wall_s_median": _median([x["wall_s"] for x in m]),
        "cost_usd_total_observed": round(sum(c for c in costs if c is not None), 4),
        "cost_usd_mean_complete_runs": _mean([x["cost_usd"] for x in complete]),
        "runs_with_complete_cost": len(complete),
        "tokens_complete_runs": tokens or None,
        "hidden_material_seen": sum(bool(x.get("hidden_material_seen")) for x in m),
    }


def cmd_report(a):
    results = os.path.abspath(a.results)
    runs = load_runs(results)
    if a.exclude_fake:
        runs = [r for r in runs if not r.get("fake_model")]
    manifest = read_json(os.path.join(results, "manifest.json"), {})
    conds = [c for c in CONDITIONS if any(r["condition"] == c for r in runs)]
    tasks = sorted({r["task"] for r in runs})
    by_cond = {c: aggregate([r for r in runs if r["condition"] == c]) for c in conds}
    by_task = {t: {c: aggregate([r for r in runs if r["task"] == t and r["condition"] == c])
                   for c in conds if any(r["task"] == t and r["condition"] == c for r in runs)} for t in tasks}
    spend = round(sum(r.get("budget_charge_usd", 0) for r in runs), 4)
    observed = round(sum(r["metrics"]["cost_usd"] or 0 for r in runs), 4)
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
        "conditions": by_cond,
        "tasks": by_task,
        "not_run": [c for c in CONDITIONS if c not in conds],
        "runs": [
            {k: r.get(k) for k in ("run_id", "label", "task", "kind", "condition", "repeat", "wall_s")}
            | {"metrics": r["metrics"], "judge": r.get("judge"), "claim": r.get("claim"),
               "scope_violations": r["output"].get("scope_violations"),
               "interlock": {k: (r.get("interlock") or {}).get(k) for k in ("final_state", "signals",
                                                                            "stopped_because")}
               if r.get("interlock") else None,
               "interruption": r.get("interruption"), "notes": r.get("notes")}
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
    lines += [
        "## By condition",
        "",
        "| Metric | " + " | ".join(rep["conditions"]) + " |",
        "| --- | " + " | ".join("---" for _ in rep["conditions"]) + " |",
    ]
    rows = [
        ("Runs", lambda a: a["runs"]),
        ("Accepted outcomes", lambda a: _rate(a["accepted"], a["runs"])),
        ("Hidden checks pass (claimed or not)", lambda a: _rate(a["hidden_pass"], a["runs"])),
        ("Completion claimed", lambda a: _rate(a["claimed_done"], a["runs"])),
        ("Unsupported completion claims", lambda a: _rate(a["unsupported_claims"], a["runs"])),
        ("Missed defects (failed hidden cases in claimed outputs)", lambda a: a["missed_defects"]),
        ("Runs with scope violations", lambda a: _rate(a["scope_violation_runs"], a["runs"])),
        ("Correct output not claimed", lambda a: _rate(a["correct_but_not_claimed"], a["runs"])),
        ("Recovery after forced interruption",
         lambda a: _rate(a["recovery"]["recovered"], a["recovery"]["interrupted"]) if a["recovery"] else "-"),
        ("Human interventions", lambda a: a["human_interventions"]),
        ("Ended needing an operator", lambda a: _rate(a["operator_needed"], a["runs"])),
        ("Sessions per run (mean)", lambda a: a["sessions_mean"]),
        ("Wall time, mean / median (s)", lambda a: f"{a['wall_s_mean']} / {a['wall_s_median']}"),
        ("Cost reported, total (USD)", lambda a: f"{a['cost_usd_total_observed']:.2f}"),
        ("Cost per run, mean over runs with complete cost (USD)",
         lambda a: "unavailable" if a["cost_usd_mean_complete_runs"] is None else f"{a['cost_usd_mean_complete_runs']:.3f}"),
        ("Runs with complete cost", lambda a: _rate(a["runs_with_complete_cost"], a["runs"])),
        ("Output tokens, complete runs", lambda a: (a["tokens_complete_runs"] or {}).get("output", "unavailable")),
        ("Transcripts mentioning hidden material", lambda a: a["hidden_material_seen"]),
    ]
    for name, fn in rows:
        lines.append(f"| {name} | " + " | ".join(str(fn(rep["conditions"][c])) for c in rep["conditions"]) + " |")
    lines += ["", "## By task", "", "| Task | Condition | Accepted | Claimed | Unsupported | Missed defects | "
              "Scope violations | Mean wall (s) | Cost reported (USD) |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for t, conds in rep["tasks"].items():
        for c, a in conds.items():
            lines.append(f"| {t} | {c} | {_rate(a['accepted'], a['runs'])} | {_rate(a['claimed_done'], a['runs'])} | "
                         f"{a['unsupported_claims']} | {a['missed_defects']} | {a['scope_violation_runs']} | "
                         f"{a['wall_s_mean']} | {a['cost_usd_total_observed']:.2f} |")
    lines += ["", "## Runs", "", "| Run | Claimed | Hidden | Failed hidden cases | Scope violations | interlock | "
              "Wall (s) | Cost (USD) |", "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for r in rep["runs"]:
        m = r["metrics"]
        il = r.get("interlock") or {}
        sig = " ".join(il.get("signals") or []) if il else ""
        lines.append(
            f"| {r['run_id']} | {m['claimed_done']} | {m['hidden_pass']} | {', '.join((r.get('judge') or {}).get('failed') or []) or '-'} | "
            f"{', '.join(r.get('scope_violations') or []) or '-'} | {(il.get('final_state') or '') + (' ' + sig if sig else '')} | "
            f"{m['wall_s']} | {m['cost_usd'] if m['cost_usd'] is not None else 'unavailable'} |"
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
    default_work = os.environ.get("EVAL_WORK_DIR", os.path.join(tempfile.gettempdir(), "interlock-eval-work"))

    b = sub.add_parser("build", help="build frozen task repositories")
    b.add_argument("--work-dir", default=default_work)
    b.add_argument("--tasks", nargs="*")
    b.add_argument("--update-lock", action="store_true")

    s = sub.add_parser("selftest", help="check every task with no model calls")
    s.add_argument("--work-dir", default=default_work)
    s.add_argument("--tasks", nargs="*")
    s.add_argument("--interlock-bin")
    s.add_argument("--no-lock", action="store_true")
    s.add_argument("--out")

    r = sub.add_parser("run", help="run conditions on tasks")
    r.add_argument("--host", choices=["claude-code", "copilot"], required=True)
    r.add_argument("--conditions", default="plain,interlock")
    r.add_argument("--tasks", nargs="*")
    r.add_argument("--repeats", type=int, default=1)
    r.add_argument("--model")
    r.add_argument("--effort", help="pinned as CLAUDE_CODE_EFFORT_LEVEL for every condition (Claude Code)")
    r.add_argument("--max-turns", type=int, default=80)
    r.add_argument("--session-timeout-min", type=int, default=15)
    r.add_argument("--max-sessions", type=int, default=6)
    r.add_argument("--budget-usd", type=float, required=True)
    r.add_argument("--per-run-usd", type=float, default=3.0, help="--max-budget-usd for plain Claude Code runs")
    r.add_argument("--default-plain-usd", type=float, default=1.0)
    r.add_argument("--default-interlock-usd", type=float, default=2.5)
    r.add_argument("--results", required=True)
    r.add_argument("--work-dir", default=default_work)
    r.add_argument("--interlock-bin", default=os.path.join(EVAL_DIR, "..", "target", "debug", "interlock"))
    r.add_argument("--claude-bin")
    r.add_argument("--copilot-bin")
    r.add_argument("--skills-dir")
    r.add_argument("--fake-model", action="store_true", help="Copilot offline against the scripted model (plumbing)")
    r.add_argument("--label", default="main")
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
