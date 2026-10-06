import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
here = os.path.dirname(__file__)
cases = judgelib.go_cases(a.tree, a.start, here, ["zz_hidden_test.go"])

cli, err = judgelib.go_build_cli(a.tree)
with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False) as f:
    f.write("name = demo\n[server]\nport = 8080\n[db.eu]\nhost = db1.example.org\n")


def get(name):
    return judgelib.run([cli, "get", f.name, name], cwd=a.tree)


if cli is None:
    cases.append(judgelib.case("cli_get_dotted_section", False, "build failed:\n" + err))
    cases.append(judgelib.case("cli_get_plain_names", False, "build failed"))
else:
    code, out = get("db.eu.host")
    cases.append(judgelib.case("cli_get_dotted_section", code == 0 and out == "db1.example.org\n", f"exit {code} {out!r}"))
    c1, o1 = get("server.port")
    c2, o2 = get("name")
    ok = (c1, o1, c2, o2) == (0, "8080\n", 0, "demo\n")
    cases.append(judgelib.case("cli_get_plain_names", ok, f"server.port: {c1} {o1!r}; name: {c2} {o2!r}"))
judgelib.emit(cases)
