"""Helpers for hidden acceptance checks.

A task's hidden judge is called as

    python3 judge.py --tree DIR --start DIR [--answer FILE] [--facts FILE]

where DIR under --tree is a scratch copy of the output being judged (the judge
may change it), --start is the task's starting tree, --answer holds the ANSWER
lines from the final message, and --facts is the frozen repository's facts
file. It prints {"cases": [{"id", "pass", "detail"}]} on stdout. The judge
never learns which condition produced the output.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

TIMEOUT = 300


def args():
    p = argparse.ArgumentParser()
    p.add_argument("--tree", required=True)
    p.add_argument("--start", required=True)
    p.add_argument("--answer")
    p.add_argument("--facts")
    return p.parse_args()


def emit(cases):
    json.dump({"cases": cases}, sys.stdout, indent=1)
    sys.stdout.write("\n")


def case(case_id, ok, detail=""):
    return {"id": case_id, "pass": bool(ok), "detail": detail[-1500:]}


def run(cmd, cwd, env=None, timeout=TIMEOUT, stdin=None):
    """Runs a command; returns (exit code or None on timeout, combined output)."""
    full_env = dict(os.environ)
    full_env.update(env or {})
    try:
        p = subprocess.run(
            cmd,
            cwd=cwd,
            env=full_env,
            input=stdin,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return p.returncode, p.stdout + p.stderr
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        return None, out + f"\n(timed out after {timeout}s)"


# --- Python -------------------------------------------------------------

_PY_RUNNER = r"""
import json, sys, unittest
start, pattern = sys.argv[1], sys.argv[2]
suite = unittest.defaultTestLoader.discover(start, pattern=pattern, top_level_dir=".")
results = {}
class R(unittest.TextTestResult):
    def addSuccess(self, t):
        super().addSuccess(t); results[t.id()] = [True, ""]
    def addFailure(self, t, err):
        super().addFailure(t, err); results[t.id()] = [False, self._exc_info_to_string(err, t)]
    def addError(self, t, err):
        super().addError(t, err); results[t.id()] = [False, self._exc_info_to_string(err, t)]
    def addSkip(self, t, reason):
        super().addSkip(t, reason); results[t.id()] = [False, "skipped: " + reason]
import io
unittest.TextTestRunner(stream=io.StringIO(), resultclass=R).run(suite)
for err in getattr(suite, "_errors", []) or []:
    results["(import)"] = [False, str(err)]
print("JSON:" + json.dumps(results))
"""


def py_unittest(tree, start_dir, pattern="test*.py"):
    """Runs unittest discovery in `tree`; returns {test_id: (ok, detail)}."""
    code, out = run([sys.executable, "-c", _PY_RUNNER, start_dir, pattern], cwd=tree)
    for line in out.splitlines():
        if line.startswith("JSON:"):
            data = json.loads(line[5:])
            # Loader failures show up as tests named unittest.loader._FailedTest.*
            return {k: (v[0], v[1]) for k, v in data.items()}
    return {"(runner)": (False, f"exit {code}: {out}")}


def restore_python_tests(tree, start, tests_dir="tests"):
    """Replaces the output's tests with the task's original ones, so a weakened
    or deleted test cannot make the output look better."""
    shutil.rmtree(os.path.join(tree, tests_dir), ignore_errors=True)
    shutil.copytree(os.path.join(start, tests_dir), os.path.join(tree, tests_dir))


def install_hidden_python(tree, hidden_dir, files):
    dest = os.path.join(tree, "zz_hidden")
    os.makedirs(dest, exist_ok=True)
    open(os.path.join(dest, "__init__.py"), "w").close()
    for f in files:
        shutil.copy(os.path.join(hidden_dir, f), dest)
    return "zz_hidden"


def python_cases(tree, start, hidden_dir, hidden_files):
    """The usual Python judgement: the original test suite as one case, then
    one case per hidden test method."""
    restore_python_tests(tree, start)
    cases = []
    existing = py_unittest(tree, "tests")
    bad = [f"{k}: {v[1]}" for k, v in existing.items() if not v[0]]
    cases.append(case("existing_tests", existing and not bad, "\n".join(bad) or f"{len(existing)} passed"))
    pkg = install_hidden_python(tree, hidden_dir, hidden_files)
    hidden = py_unittest(tree, pkg, pattern="hidden_*.py")
    for test_id, (ok, detail) in sorted(hidden.items()):
        cases.append(case(test_id.split(".")[-1], ok, detail))
    return cases


# --- Go -----------------------------------------------------------------

GO_ENV = {"GOTOOLCHAIN": "local", "CGO_ENABLED": "0"}


def restore_go_tests(tree, start):
    """Deletes every _test.go file in the output and puts back the original ones."""
    for root, _, files in os.walk(tree):
        for f in files:
            if f.endswith("_test.go"):
                os.remove(os.path.join(root, f))
    for root, _, files in os.walk(start):
        for f in files:
            if f.endswith("_test.go"):
                rel = os.path.relpath(os.path.join(root, f), start)
                os.makedirs(os.path.dirname(os.path.join(tree, rel)), exist_ok=True)
                shutil.copy(os.path.join(root, f), os.path.join(tree, rel))


def go_test_json(tree, pkgs="./..."):
    """Runs go test -json; returns ({test: ok}, raw output)."""
    code, out = run(["go", "test", "-json", "-count=1", pkgs], cwd=tree, env=GO_ENV)
    results, text = {}, []
    for line in out.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            text.append(line)
            continue
        if e.get("Output"):
            text.append(e["Output"].rstrip("\n"))
        if e.get("Test") and "/" not in e["Test"] and e.get("Action") in ("pass", "fail", "skip"):
            results[e["Test"]] = e["Action"] == "pass"
    return results, "\n".join(text)


def go_cases(tree, start, hidden_dir, hidden_files):
    """The original Go tests as one case, then one case per hidden TestHidden*
    function. Hidden test files are copied to the paths their names give,
    with '__' standing for '/'."""
    restore_go_tests(tree, start)
    wanted = []
    for f in hidden_files:
        rel = f.replace("__", "/")
        dest = os.path.join(tree, rel)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.copy(os.path.join(hidden_dir, f), dest)
        with open(dest) as fh:
            wanted += re.findall(r"^func (TestHidden\w+)\(", fh.read(), re.M)
    results, text = go_test_json(tree)
    existing = {k: v for k, v in results.items() if not k.startswith("TestHidden")}
    failed = [k for k, v in existing.items() if not v]
    tail = "\n".join(text.splitlines()[-40:])
    cases = [
        case(
            "existing_tests",
            existing and not failed,
            (f"failed: {', '.join(failed)}\n" if failed else f"{len(existing)} passed\n") + ("" if existing else tail),
        )
    ]
    for name in wanted:
        ok = results.get(name)
        cases.append(case(name, ok is True, "" if ok else (tail if ok is None else _go_failure(text, name))))
    return cases


def _go_failure(text, name):
    lines = [l for l in text.splitlines() if name in l or l.startswith("    ")]
    return "\n".join(lines[-20:])


def go_build_cli(tree, pkg="./cmd/kvconf"):
    """Builds the command into a temporary directory; returns (path, error)."""
    out_dir = tempfile.mkdtemp(prefix="judge-bin-")
    path = os.path.join(out_dir, "cli")
    code, out = run(["go", "build", "-o", path, pkg], cwd=tree, env=GO_ENV)
    if code != 0:
        return None, out
    return path, ""


# --- Answers -------------------------------------------------------------


def answer_lines(path):
    if not path or not os.path.exists(path):
        return []
    with open(path) as f:
        return [l.strip() for l in f if l.strip()]
