"""The forced interruption: kill a run between the agent's first edit and its
first test run after that edit.

The watcher learns of edits from inotify (through ctypes, so the standard
library is enough), or from a 0.1-second poll where inotify is unavailable,
and confirms each with `git status`: an edit counts only when the working
tree differs from its commit. It learns of test runs by tailing the
session's transcript as it is written: a tool call whose command runs tests
or a check. Tool calls appear in the transcript before they execute.

Once the first edit is seen, the kill comes after a delay drawn from
[0, max_delay] seconds (deterministic per seed), or as soon as a test
command is issued, whichever is first. If a test command was already
waiting when the edit landed (issued in the same turn as the edit), the
kill is immediate.

A run is interrupted *as designed* only if the kill came after an edit and
before any test command finished after that edit. Anything else is recorded
and flagged, never silently dropped.
"""

import ctypes
import ctypes.util
import json
import os
import re
import select
import struct
import subprocess
import time

TEST_RE = re.compile(
    r"\bunittest\b|\bpytest\b|\bgo\s+(test|run|vet|build)\b|\bchecks/|interlock\s+check\s+run"
    r"|python3?\s+-m\s+tally\b|\bkvconf\s+(get|dump)\b"
)
EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit", "edit", "create", "write"}

IN_MODIFY, IN_ATTRIB, IN_CLOSE_WRITE = 0x2, 0x4, 0x8
IN_MOVED_FROM, IN_MOVED_TO, IN_CREATE, IN_DELETE = 0x40, 0x80, 0x100, 0x200
IN_ISDIR = 0x40000000
IN_NONBLOCK, IN_CLOEXEC = 0o4000, 0o2000000
MASK = IN_MODIFY | IN_CLOSE_WRITE | IN_MOVED_FROM | IN_MOVED_TO | IN_CREATE | IN_DELETE
SKIP_DIRS = {".git", ".interlock", "__pycache__", ".pytest_cache"}


class Inotify:
    """A recursive watch on a few small trees, through libc."""

    def __init__(self):
        self.libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
        self.fd = self.libc.inotify_init1(IN_NONBLOCK | IN_CLOEXEC)
        if self.fd < 0:
            raise OSError(ctypes.get_errno(), "inotify_init1 failed")
        self.wds = {}
        self.roots = set()

    def add_tree(self, root):
        if root in self.roots:
            return
        self.roots.add(root)
        for dirpath, dirs, _ in os.walk(root):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            self._add(dirpath)

    def _add(self, path):
        wd = self.libc.inotify_add_watch(self.fd, os.fsencode(path), MASK)
        if wd >= 0:
            self.wds[wd] = path

    def events(self, timeout):
        """Paths touched since the last call, waiting at most `timeout` seconds."""
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return []
        try:
            data = os.read(self.fd, 65536)
        except BlockingIOError:
            return []
        out, i = [], 0
        while i + 16 <= len(data):
            wd, mask, _cookie, length = struct.unpack_from("iIII", data, i)
            name = data[i + 16 : i + 16 + length].rstrip(b"\0").decode(errors="replace")
            i += 16 + length
            base = self.wds.get(wd)
            if base is None:
                continue
            path = os.path.join(base, name) if name else base
            if mask & IN_ISDIR and mask & (IN_CREATE | IN_MOVED_TO) and name not in SKIP_DIRS:
                self._add(path)
            out.append(path)
        return out

    def close(self):
        os.close(self.fd)


def porcelain(repo):
    """`git status --porcelain` lines, with their leading status columns intact."""
    p = subprocess.run(["git", "--no-optional-locks", "status", "--porcelain", "-uall"], cwd=repo,
                       capture_output=True, text=True)
    if p.returncode != 0:
        return []
    return [l for l in p.stdout.split("\n") if l.strip()]


class TranscriptTail:
    """Reads the tool calls and results that have arrived in transcripts so far."""

    def __init__(self):
        self.offsets = {}
        self.calls = {}  # tool id -> {"test": bool, "edit": bool, "result_at": float|None}
        self.order = []

    def poll(self, paths, now):
        new_tests, finished_tests = [], []
        for p in paths:
            try:
                with open(p, "rb") as f:
                    f.seek(self.offsets.get(p, 0))
                    chunk = f.read()
            except OSError:
                continue
            # Only consume complete lines.
            end = chunk.rfind(b"\n")
            if end < 0:
                continue
            self.offsets[p] = self.offsets.get(p, 0) + end + 1
            for line in chunk[: end + 1].decode(errors="replace").splitlines():
                for kind, tid, info in _tool_events(line):
                    if kind == "use" and tid not in self.calls:
                        self.calls[tid] = {**info, "seen_at": now, "result_at": None}
                        self.order.append(tid)
                        if info["test"]:
                            new_tests.append(tid)
                    elif kind == "result" and tid in self.calls and self.calls[tid]["result_at"] is None:
                        self.calls[tid]["result_at"] = now
                        if self.calls[tid]["test"]:
                            finished_tests.append(tid)
        return new_tests, finished_tests

    def pending_tests(self):
        return [t for t in self.order if self.calls[t]["test"] and self.calls[t]["result_at"] is None]


def _tool_events(line):
    """("use", id, {"test", "edit", "command"}) and ("result", id, None) events in one line."""
    try:
        e = json.loads(line)
    except ValueError:
        return []
    out = []
    t = e.get("type") if isinstance(e, dict) else None
    if t == "assistant":
        for item in (e.get("message") or {}).get("content") or []:
            if isinstance(item, dict) and item.get("type") == "tool_use":
                out.append(("use", item.get("id"), _describe(item.get("name"), item.get("input") or {})))
    elif t == "user":
        for item in (e.get("message") or {}).get("content") or []:
            if isinstance(item, dict) and item.get("type") == "tool_result":
                out.append(("result", item.get("tool_use_id"), None))
    elif t == "tool.execution_start":
        d = e.get("data") or {}
        out.append(("use", d.get("toolCallId"), _describe(d.get("toolName"), d.get("arguments") or {})))
    elif t == "tool.execution_complete":
        out.append(("result", (e.get("data") or {}).get("toolCallId"), None))
    return out


def _describe(name, inp):
    cmd = inp.get("command") if isinstance(inp, dict) else None
    return {
        "name": name,
        "command": cmd if isinstance(cmd, str) else None,
        "test": bool(isinstance(cmd, str) and TEST_RE.search(cmd)),
        "edit": name in EDIT_TOOLS,
    }


class Interrupter:
    """A `watch` callable for run_proc: returns a reason when it is time to kill."""

    def __init__(self, delay_s, work_dirs, transcripts, use_inotify=True, clock=time.monotonic):
        self.delay_s = delay_s
        self.work_dirs = work_dirs  # callable -> list of directories to watch
        self.transcripts = transcripts  # callable -> list of transcript paths
        self.clock = clock
        self.start = clock()
        self.tail = TranscriptTail()
        self.edit_at = None
        self.fired_at = None
        self.trigger = None
        self.changes = []
        self.tests_finished_after_edit = 0
        self.inotify = None
        self.mechanism = "poll"
        if use_inotify:
            try:
                self.inotify = Inotify()
                self.mechanism = "inotify"
            except (OSError, AttributeError):
                self.inotify = None

    def _edited(self):
        dirs = [d for d in self.work_dirs() if os.path.isdir(d)]
        if self.inotify:
            for d in dirs:
                self.inotify.add_tree(d)
            touched = self.inotify.events(0.1)
            if not touched:
                return []
        else:
            time.sleep(0.1)
        for d in dirs:
            lines = porcelain(d)
            if lines:
                return lines
        return []

    def __call__(self):
        now = self.clock()
        new_tests, finished = self.tail.poll(self.transcripts(), now)
        if self.edit_at is not None:
            self.tests_finished_after_edit += len(finished)
        if self.edit_at is None:
            changes = self._edited()
            if changes:
                self.edit_at = self.clock()
                self.changes = changes[:10]
                if self.tail.pending_tests():
                    return self._fire("a test command was already issued when the first edit landed")
            return None
        if new_tests:
            return self._fire("a test command was issued after the first edit")
        if self.clock() - self.edit_at >= self.delay_s:
            return self._fire(f"{self.delay_s:.1f}s after the first edit")
        return None

    def _fire(self, why):
        self.fired_at = self.clock()
        self.trigger = why
        return f"interrupted: {why}"

    def record(self):
        valid = self.fired_at is not None and self.edit_at is not None and self.tests_finished_after_edit == 0
        if self.inotify:
            self.inotify.close()
            self.inotify = None
        return {
            "interrupted": self.fired_at is not None,
            "valid": valid,
            "why_not_valid": None if valid else (
                "no edit before the session ended" if self.edit_at is None else
                "never fired" if self.fired_at is None else
                f"{self.tests_finished_after_edit} test run(s) finished after the first edit before the kill"),
            "trigger": self.trigger,
            "planned_delay_s": round(self.delay_s, 2),
            "first_edit_after_s": None if self.edit_at is None else round(self.edit_at - self.start, 1),
            "killed_after_s": None if self.fired_at is None else round(self.fired_at - self.start, 1),
            "changes_at_first_edit": self.changes,
            "edit_detection": self.mechanism,
        }
