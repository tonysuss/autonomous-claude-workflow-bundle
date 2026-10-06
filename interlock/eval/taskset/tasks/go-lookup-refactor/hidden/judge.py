import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
here = os.path.abspath(os.path.dirname(__file__))
cases = judgelib.go_cases(a.tree, a.start, here, ["zz_hidden_test.go"])

# The refactor's shape, read from the syntax tree rather than judged by eye.
code, out = judgelib.run(
    ["go", "run", os.path.join(here, "structure", "main.go"), os.path.abspath(a.tree)],
    cwd=os.path.join(here, "structure"),
    env=judgelib.GO_ENV,
)
found = [json.loads(l) for l in out.splitlines() if l.startswith("{")]
if not found:
    found = [{"id": "structure", "pass": False, "detail": f"exit {code}: {out}"}]
cases += [judgelib.case(c["id"], c["pass"], c["detail"]) for c in found]
judgelib.emit(cases)
