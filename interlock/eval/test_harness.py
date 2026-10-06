"""Unit tests for the harness's own machinery. No model calls.

    python3 -m unittest discover -s interlock/eval -p 'test_*.py'
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402
import interrupt  # noqa: E402
import leakscan  # noqa: E402


def bash(cmd):
    return "Bash " + json.dumps({"command": cmd})


class LeakScanReviewerExamples(unittest.TestCase):
    """The four transcripts from the review, which the first scanner missed,
    in the layout they were written against: runs under W/ws/<id>/<repo>,
    with W/frozen, W/start and the results next to them."""

    S = "/scratch"
    W = "/scratch/work"
    OWN = "/scratch/work/ws/dd38f750a5"
    TASKSET = "/home/user/repo/.claude/worktrees/x/interlock/eval/taskset"

    def ctx(self):
        return leakscan.Context(
            cwd=self.OWN + "/tally", own_root=self.OWN, run_base=self.W + "/ws",
            forbidden=[("the harness work directory", self.W), ("the results", self.S + "/results-claude"),
                       ("the task set", self.TASKSET)])

    def scan(self, line):
        return leakscan.scan_lines([line], self.ctx())

    def test_control_absolute_taskset_path(self):
        f = self.scan("Read " + json.dumps({"file_path": self.TASKSET + "/tasks/x/hidden/judge.py"}))
        self.assertTrue(f and f[0].strong)

    def test_earlier_runs_judged_output_and_verdict(self):
        f = self.scan(bash("cat ../../../../results-claude/judge/016b49072a36/verdict.json"))
        self.assertEqual([(x.kind, x.label) for x in f], [("read", "the results")])

    def test_earlier_runs_working_copy(self):
        f = self.scan(bash("ls ../; cat ../../92b100873c/kvconf/parser.go"))
        self.assertEqual([x.kind for x in f], ["other-run"])

    def test_reference_solution_by_relative_path_in_another_checkout(self):
        f = self.scan(bash("cd /home/user/autonomous-claude-workflow-bundle && "
                           "cat interlock/eval/taskset/tasks/py-parse-amount/hidden/solution/tally/money.py"))
        self.assertTrue(f and all(x.strong for x in f), f)

    def test_sibling_frozen_repo_of_another_task(self):
        f = self.scan(bash("cat ../../../frozen/py-csv-format/tally/money.py"))
        self.assertEqual([(x.kind, x.label) for x in f], [("read", "the harness work directory")])


class LeakScanNewLayout(unittest.TestCase):
    OWN = "/tmp/agent-ws/abc123"

    def ctx(self, **kw):
        c = leakscan.Context(
            cwd=self.OWN + "/tally", own_root=self.OWN, run_base="/tmp/agent-ws",
            forbidden=[("the harness state", "/tmp/state"), ("the results", "/data/results")],
            needles=[("another run's id", "py-split-remainder.plain.r1.abc")])
        for k, v in kw.items():
            setattr(c, k, v)
        return c

    def kinds(self, lines, **kw):
        return [(f.kind, f.strong) for f in leakscan.scan_lines(lines, self.ctx(**kw))]

    def test_own_working_copy_and_tmp_scratch_are_fine(self):
        self.assertEqual(self.kinds([bash("cat tally/money.py ../task.toml && python3 /tmp/try.py > /tmp/out.txt"),
                                     "Read " + json.dumps({"file_path": self.OWN + "/tally/tally/cli.py"})]), [])

    def test_relative_globs_and_redirections_are_not_absolute(self):
        # Found on the October 6 transcripts: "tally/*.py" once resolved to "/".
        self.assertEqual(self.kinds([bash("cat tally/*.py | head -300; wc -l *.go cmd/*/*.go 2>/dev/null")]), [])

    def test_glob_into_the_run_base(self):
        self.assertEqual(self.kinds([bash("cat ../../*/tally/tally/money.py")]), [("other-run", True)])

    def test_parent_traversal_into_state(self):
        self.assertEqual(self.kinds([bash("cat ../../../state/frozen/x/money.py")]), [("read", True)])

    def test_cd_then_relative_read_across_calls(self):
        self.assertEqual(self.kinds([bash("cd /tmp"), bash("cat state/start/x/tally/money.py")]), [("read", True)])

    def test_search_of_an_ancestor_is_weak(self):
        self.assertEqual(self.kinds([bash("find / -name judge.py")]), [("ancestor-search", False)])

    def test_needle_and_path_pattern(self):
        self.assertIn(("needle", True), self.kinds([bash("ls /data/results/runs/py-split-remainder.plain.r1.abc")]))
        self.assertEqual(self.kinds([bash("cat /opt/elsewhere/interlock/evidence/evaluation/README.md")]),
                         [("read", True)])

    def test_python_string_paths(self):
        self.assertEqual(self.kinds([bash("python3 -c \"print(open('../../../state/frozen/a').read())\"")]),
                         [("read", True)])

    def test_claude_stream_json_and_init_cwd(self):
        lines = [
            json.dumps({"type": "system", "subtype": "init", "cwd": self.OWN + "/tally/.interlock/worktrees/w-1"}),
            json.dumps({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cat ../../../../../ffff/x"}}]}}),
        ]
        # From the worktree, five levels up is the run base: another run's directory.
        self.assertEqual(self.kinds(lines), [("other-run", True)])

    def test_copilot_events(self):
        line = json.dumps({"type": "tool.execution_start",
                           "data": {"toolCallId": "c1", "toolName": "bash", "arguments": {"command": "cat /tmp/state/x"}}})
        self.assertEqual(self.kinds([line]), [("read", True)])

    def test_upstream_needles(self):
        ctx = self.ctx(needles=[("the upstream fix commit", "deadbeefcafe")])
        f = leakscan.scan_lines([bash("git show deadbeefcafe")], ctx)
        self.assertEqual([x.kind for x in f], ["needle"])


def git_repo():
    d = tempfile.mkdtemp()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=d, check=True)
    with open(os.path.join(d, "a.txt"), "w") as f:
        f.write("one\n")
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.org", GIT_COMMITTER_NAME="t",
               GIT_COMMITTER_EMAIL="t@example.org")
    subprocess.run(["git", "add", "-A"], cwd=d, check=True, env=env)
    subprocess.run(["git", "commit", "-q", "-m", "x"], cwd=d, check=True, env=env)
    return d


def tool_use(tid, command):
    return json.dumps({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": tid, "name": "Bash", "input": {"command": command}}]}}) + "\n"


def tool_result(tid):
    return json.dumps({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": tid}]}}) + "\n"


class Interruption(unittest.TestCase):
    def setUp(self):
        self.repo = git_repo()
        self.transcript = os.path.join(tempfile.mkdtemp(), "s.jsonl")
        open(self.transcript, "w").close()

    def tearDown(self):
        shutil.rmtree(self.repo, ignore_errors=True)

    def write(self, text):
        with open(self.transcript, "a") as f:
            f.write(text)

    def watcher(self, delay, inotify_on):
        return interrupt.Interrupter(delay, lambda: [self.repo], lambda: [self.transcript], use_inotify=inotify_on)

    def poll(self, w, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            r = w()
            if r:
                return r
            time.sleep(0.02)
        return None

    def test_porcelain_keeps_the_first_status_column(self):
        with open(os.path.join(self.repo, "a.txt"), "a") as f:
            f.write("two\n")
        self.assertEqual(interrupt.porcelain(self.repo), [" M a.txt"])

    def _fires_after_delay(self, inotify_on):
        w = self.watcher(0.3, inotify_on)
        self.write(tool_use("t0", "sh checks/repro.sh") + tool_result("t0"))  # a test before any edit
        self.assertIsNone(self.poll(w, 0.3), "nothing happens before an edit")
        with open(os.path.join(self.repo, "a.txt"), "a") as f:
            f.write("two\n")
        reason = self.poll(w, 3)
        self.assertIn("after the first edit", reason or "")
        rec = w.record()
        self.assertTrue(rec["valid"], rec)
        self.assertEqual(rec["changes_at_first_edit"], [" M a.txt"])

    def test_fires_after_the_delay_with_inotify(self):
        self._fires_after_delay(True)

    def test_fires_after_the_delay_with_polling(self):
        self._fires_after_delay(False)

    def test_fires_when_a_test_is_issued_after_the_edit(self):
        w = self.watcher(60, True)
        self.assertIsNone(self.poll(w, 0.2))
        with open(os.path.join(self.repo, "b.txt"), "w") as f:
            f.write("new\n")
        self.assertIsNone(self.poll(w, 0.3))
        self.write(tool_use("t1", "python3 -m unittest discover -s tests"))
        self.assertIn("test command was issued", self.poll(w, 2) or "")
        self.assertTrue(w.record()["valid"])

    def test_a_test_issued_with_the_edit_fires_at_the_edit(self):
        w = self.watcher(60, True)
        self.write(tool_use("t2", "go test ./..."))  # issued, not finished
        self.assertIsNone(self.poll(w, 0.2))
        with open(os.path.join(self.repo, "a.txt"), "a") as f:
            f.write("x\n")
        self.assertIn("already issued", self.poll(w, 2) or "")

    def test_no_edit_means_not_interrupted(self):
        w = self.watcher(0.1, True)
        self.assertIsNone(self.poll(w, 0.3))
        rec = w.record()
        self.assertFalse(rec["interrupted"])
        self.assertFalse(rec["valid"])
        self.assertEqual(rec["why_not_valid"], "no edit before the session ended")

    def test_a_test_finished_after_the_edit_makes_the_landing_invalid(self):
        w = self.watcher(60, True)
        self.write(tool_use("t3", "echo hi"))
        with open(os.path.join(self.repo, "a.txt"), "a") as f:
            f.write("x\n")
        self.assertIsNone(self.poll(w, 0.3))
        # A test whose call the watcher saw only together with its result.
        self.write(tool_use("t4", "sh checks/x.sh") + tool_result("t4"))
        reason = self.poll(w, 2)
        self.assertIsNotNone(reason)
        self.assertFalse(w.record()["valid"])


class Environment(unittest.TestCase):
    def cfg(self, host="claude-code"):
        a = SimpleNamespace(host=host, model="m", effort="medium", max_turns=5, session_timeout_min=1,
                            max_sessions=2, per_run_usd=1.0, interlock_bin="/x/interlock", claude_bin="/bin/claude",
                            copilot_bin="/bin/copilot", fake_model=False, pass_env=["EXTRA_OK"],
                            interlock_skills=False)
        c = harness.HostConfig(a, tempfile.mkdtemp())
        c._features = {"effort": False, "skills": False, "skills_generate": False}
        return c

    def test_allowlist_drops_session_bindings_and_secrets(self):
        saved = dict(os.environ)
        try:
            os.environ.update(CLAUDE_CODE_SESSION_ID="s", CLAUDE_CODE_MESSAGING_TOKEN="t", CLAUDE_CODE_NEW_BINDING="b",
                              GH_TOKEN="g", INTERLOCK_DB="/x", EXTRA_OK="yes")
            env = self.cfg().env_for("plain")
            for k in ("CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_CODE_NEW_BINDING", "GH_TOKEN",
                      "INTERLOCK_DB", "INTERLOCK_CLAUDE_BIN", "CLAUDE_CODE_EFFORT_LEVEL"):
                self.assertNotIn(k, env)
            self.assertEqual(env["EXTRA_OK"], "yes")
            self.assertIn("PATH", env)
        finally:
            os.environ.clear()
            os.environ.update(saved)

    def test_interlock_condition_gets_its_binary_and_the_effort_fallback(self):
        env = self.cfg().env_for("interlock")
        self.assertEqual(env["INTERLOCK_CLAUDE_BIN"], "/bin/claude")
        self.assertEqual(env["CLAUDE_CODE_EFFORT_LEVEL"], "medium")

    def test_plain_command_uses_the_worker_tools_and_effort(self):
        tools = {"allow": ["Read", "Bash", "WebFetch"], "deny": ["Edit", "Bash(git push:*)"]}
        cmd, stdin = self.cfg().plain_command("p", tools)
        self.assertEqual(stdin, "p")
        self.assertIn("--effort", cmd)
        self.assertEqual(cmd[cmd.index("--allowedTools") + 1 : cmd.index("--disallowedTools")], tools["allow"])
        self.assertEqual(cmd[cmd.index("--disallowedTools") + 1 :], tools["deny"])
        cmd, _ = self.cfg("copilot").plain_command("p", {"allow": ["shell"], "deny": ["write"]})
        self.assertIn("--allow-tool=shell", cmd)
        self.assertIn("--deny-tool=write", cmd)


class StubRunner:
    """Writes run records whose interruption lands only when told to."""

    def __init__(self, results, lands):
        self.results, self.lands, self.calls = results, list(lands), 0

    def run_one(self, task, frozen, condition, repeat, attempt):
        self.calls += 1
        valid = self.lands.pop(0)
        rec = {"run_id": f"{task.id}.{condition}.r{repeat}.{self.calls}", "label": "main", "task": task.id,
               "condition": condition, "repeat": repeat, "attempt": attempt, "budget_charge_usd": 0.1,
               "interruption": {"interrupted": valid, "valid": valid, "why_not_valid": None if valid else "no edit"},
               "output": {"scope_violations": []},
               "metrics": {"accepted": True, "claimed_done": True, "hidden_pass": True, "cost_usd": 0.1,
                           "wall_s": 1, "hidden_material_seen": False}}
        harness.write_json(os.path.join(self.results, "runs", rec["run_id"], "run.json"), rec)
        return rec


class InterruptionRetries(unittest.TestCase):
    task = SimpleNamespace(id="t", interrupt={"max_delay_s": 1})

    def go(self, lands, retries=2, repeat=1, budget=10.0):
        results = tempfile.mkdtemp()
        runner = StubRunner(results, lands)
        inv = {"runs": []}
        stopped = harness.run_until_counted(runner, self.task, None, "plain", repeat, results, budget,
                                            {"plain": 0.1}, {"main"}, retries, False, False, inv, lambda: None)
        return runner, inv, stopped, harness.load_runs(results)

    def test_a_run_that_did_not_land_is_kept_and_rerun(self):
        runner, inv, _, runs = self.go([False, True])
        self.assertEqual(runner.calls, 2)
        self.assertEqual([r["interruption"]["valid"] for r in sorted(runs, key=lambda r: r["run_id"])], [False, True])
        self.assertNotIn("gave_up", inv)

    def test_gives_up_after_the_retries_and_keeps_every_record(self):
        runner, inv, _, runs = self.go([False, False, False, True], retries=2)
        self.assertEqual(runner.calls, 3)
        self.assertEqual(len(runs), 3)
        self.assertEqual(inv["gave_up"], ["t plain r1"])

    def test_report_counts_not_landed_runs_separately(self):
        _, _, _, runs = self.go([False, True])
        for r in runs:
            r.update(sessions=[{"cost_usd": 0.1, "tokens": {"output": 1}}])
            r["metrics"].update(missed_defects=0, scope_violation=False, correct_but_not_claimed=False,
                                operator_needed=False, sessions=1, recovery={"interrupted": True, "valid": True,
                                                                             "recovered": True})
        agg = harness.aggregate(runs)
        self.assertEqual((agg["runs"], agg["interruption_runs_not_landed"]), (1, 1))

    def test_budget_stops_before_a_run(self):
        runner, inv, stopped, _ = self.go([True], budget=0.05)
        self.assertEqual(runner.calls, 0)
        self.assertIn("budget", stopped)


class Ordering(unittest.TestCase):
    def test_abba(self):
        conds = ["plain", "skills", "interlock"]
        r1 = harness.condition_order(conds, "t", 1, 13)
        self.assertEqual(sorted(r1), sorted(conds))
        self.assertEqual(harness.condition_order(conds, "t", 2, 13), r1[::-1])
        self.assertEqual(harness.condition_order(conds, "t", 3, 13), r1)


class Small(unittest.TestCase):
    def test_scope_in_the_plain_prompt(self):
        t = SimpleNamespace(prompt="Fix it.\n", scope=["a.py", "tests/**"])
        self.assertIn("Change only files that match these paths: `a.py`, `tests/**`.", harness.plain_prompt(t))
        t.scope = []
        self.assertIn("Do not change any files.", harness.plain_prompt(t))
        self.assertTrue(harness.plain_prompt(t, recovery=True).startswith("A previous session"))

    def test_bounds(self):
        self.assertEqual(harness.upper_bound(0, 14), 0.193)
        self.assertEqual(harness.upper_bound(0, 14, two_sided=True), 0.232)
        self.assertEqual(harness.upper_bound(0, 7), 0.348)

    def test_status_claim(self):
        self.assertEqual(harness.status_claim("x\nSTATUS: DONE\n"), (True, "DONE"))
        self.assertEqual(harness.status_claim("STATUS: DONE\nSTATUS: NOT DONE: no"), (False, "NOT DONE"))
        self.assertEqual(harness.status_claim("all good"), (False, "no STATUS line"))

    def test_globs_and_scope(self):
        self.assertTrue(harness.glob_matches("tests/**", "tests/a/b.py"))
        self.assertFalse(harness.glob_matches("*_test.go", "cmd/kvconf/main_test.go"))
        self.assertEqual(harness.out_of_scope(["a.py", "b.py"], ["a.py"]), ["b.py"])
        self.assertEqual(harness.out_of_scope(["a.py"], []), ["a.py"])

    def test_layout_must_not_overlap(self):
        base = tempfile.mkdtemp()
        with self.assertRaises(SystemExit):
            harness.check_layout(os.path.join(base, "runs"), base, os.path.join(base, "results"))
        harness.check_layout(os.path.join(base, "runs"), os.path.join(base, "state"), os.path.join(base, "res"))
        with self.assertRaises(SystemExit):
            harness.check_layout(os.path.join(harness.EVAL_DIR, "x"), os.path.join(base, "s"), os.path.join(base, "r"))


if __name__ == "__main__":
    unittest.main()
