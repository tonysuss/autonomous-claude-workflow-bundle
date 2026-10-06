"""Hidden acceptance checks for py-thousands, on the real command line.

The prompt asks for separators in `balances` and `settle` and says nothing
else may change. The likely regression is in a module the task never
mentions: `export` prints machine-readable CSV through the same
`format_cents`, and must keep printing plain `246913.56`.
"""

import os
import subprocess
import sys
import tempfile
import unittest

ROOT = os.getcwd()
BIG = os.path.join(ROOT, "examples", "big.csv")
TRIP = os.path.join(ROOT, "examples", "trip.csv")


def tally(*args):
    p = subprocess.run([sys.executable, "-m", "tally", *args], cwd=ROOT, capture_output=True, timeout=60)
    return p.returncode, p.stdout.decode(), p.stderr.decode()


def pairs(out):
    """Each output line's words, so column padding does not matter."""
    return [line.split() for line in out.splitlines() if line.strip()]


def ledger(text):
    f = tempfile.NamedTemporaryFile("w", suffix=".csv", delete=False)
    f.write(text)
    f.close()
    return f.name


class HiddenThousands(unittest.TestCase):
    def test_balances_separators(self):
        code, out, err = tally("balances", BIG)
        self.assertEqual(code, 0, err)
        self.assertEqual(
            pairs(out), [["Ann", "122,856.78"], ["Bea", "-121,810.78"], ["Cal", "-223.00"], ["Dan", "-823.00"]]
        )

    def test_settle_separators(self):
        code, out, err = tally("settle", BIG)
        self.assertEqual(code, 0, err)
        self.assertEqual(
            pairs(out), [["Bea", "->", "Ann", "121,810.78"], ["Dan", "->", "Ann", "823.00"], ["Cal", "->", "Ann", "223.00"]]
        )

    def test_millions(self):
        path = ledger("date,payer,amount,participants\nd,Ann,2469135.78,Ann;Bea\n")
        code, out, err = tally("balances", path)
        self.assertEqual((code, pairs(out)), (0, [["Ann", "1,234,567.89"], ["Bea", "-1,234,567.89"]]), err)

    def test_small_amounts_unchanged(self):
        self.assertEqual(
            tally("balances", TRIP)[:2],
            (0, "Ann       36.00\nBea      -40.00\nCal       40.00\nDan      -36.00\n"),
        )
        self.assertEqual(tally("settle", TRIP)[:2], (0, "Bea -> Cal  40.00\nDan -> Ann  36.00\n"))

    def test_export_unchanged(self):
        code, out, err = tally("export", BIG)
        self.assertEqual(code, 0, err)
        self.assertEqual(
            out,
            "date,payer,amount,participants,memo\n"
            "2026-07-01,Ann,246913.56,Ann;Bea,flat deposit\n"
            "2026-07-02,Bea,2469.00,Bea;Cal;Dan,van hire\n"
            "2026-07-03,Cal,1200.00,Ann;Cal,tools\n",
        )
