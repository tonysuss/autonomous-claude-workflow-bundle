import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
here = os.path.dirname(__file__)
cases = judgelib.go_cases(a.tree, a.start, here, ["zz_hidden_test.go"])

# The reported command, on the real CLI surface.
cli, err = judgelib.go_build_cli(a.tree)
if cli is None:
    cases.append(judgelib.case("cli_get_reported_value", False, "build failed:\n" + err))
else:
    with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False) as f:
        f.write('[app]\ntitle = "Issue #42: retry"\nport = 8080 # http\n')
    code, out = judgelib.run([cli, "get", f.name, "app.title"], cwd=a.tree)
    code2, out2 = judgelib.run([cli, "get", f.name, "app.port"], cwd=a.tree)
    ok = code == 0 and out == "Issue #42: retry\n" and code2 == 0 and out2 == "8080\n"
    cases.append(judgelib.case("cli_get_reported_value", ok, f"title: exit {code} {out!r}; port: exit {code2} {out2!r}"))
judgelib.emit(cases)
