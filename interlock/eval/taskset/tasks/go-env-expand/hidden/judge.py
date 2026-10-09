import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))
import judgelib  # noqa: E402

a = judgelib.args()
here = os.path.dirname(__file__)
cases = judgelib.go_cases(a.tree, a.start, here, ["zz_hidden_test.go"])

# The command line, with and without --no-expand.
cli, err = judgelib.go_build_cli(a.tree)
with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False) as f:
    f.write("[paths]\ndata = ${DATA_ROOT}/data\nraw = $$HOME\n")
set_env = {"DATA_ROOT": "/srv"}


def cli_case(case_id, argv, env, want_code, want_out=None, want_err=None):
    if cli is None:
        return judgelib.case(case_id, False, "build failed:\n" + err)
    full_env = dict(os.environ)
    full_env.pop("DATA_ROOT", None)
    full_env.update(env)
    import subprocess

    p = subprocess.run([cli, *argv], capture_output=True, text=True, env=full_env, timeout=60)
    ok = p.returncode == want_code
    if want_out is not None:
        ok = ok and p.stdout == want_out
    if want_err is not None:
        ok = ok and want_err in p.stderr
    return judgelib.case(case_id, ok, f"exit {p.returncode}; stdout {p.stdout!r}; stderr {p.stderr!r}")


cases.append(cli_case("cli_get_expands", ["get", f.name, "paths.data"], set_env, 0, "/srv/data\n"))
cases.append(
    cli_case("cli_get_no_expand", ["get", "--no-expand", f.name, "paths.data"], set_env, 0, "${DATA_ROOT}/data\n")
)
cases.append(
    cli_case(
        "cli_dump_no_expand",
        ["dump", "--no-expand", f.name],
        {},
        0,
        "paths.data=${DATA_ROOT}/data\npaths.raw=$$HOME\n",
    )
)
cases.append(
    cli_case("cli_undefined_variable_fails", ["dump", f.name], {}, 1, None, 'undefined variable "DATA_ROOT"')
)
judgelib.emit(cases)
