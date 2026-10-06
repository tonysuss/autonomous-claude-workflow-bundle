import contextlib
import io
import os
import unittest

from tally.cli import main

EXAMPLE = os.path.join(os.path.dirname(__file__), "..", "examples", "trip.csv")


def run(*argv):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = main(list(argv))
    return code, out.getvalue()


class CliTest(unittest.TestCase):
    def test_balances(self):
        code, out = run("balances", EXAMPLE)
        self.assertEqual(code, 0)
        self.assertEqual(
            out,
            "Ann       36.00\n"
            "Bea      -40.00\n"
            "Cal       40.00\n"
            "Dan      -36.00\n",
        )

    def test_missing_file(self):
        with contextlib.redirect_stderr(io.StringIO()):
            code, _ = run("balances", "no-such-file.csv")
        self.assertEqual(code, 1)


if __name__ == "__main__":
    unittest.main()
