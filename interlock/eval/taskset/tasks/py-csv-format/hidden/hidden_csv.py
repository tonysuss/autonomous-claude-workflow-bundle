"""Hidden acceptance checks for py-csv-format, run against the real command
line (`python3 -m tally`). Each test is one stated requirement."""

import os
import subprocess
import sys
import tempfile
import unittest

ROOT = os.getcwd()
TRIP = os.path.join(ROOT, "examples", "trip.csv")


def tally(*args):
    p = subprocess.run(
        [sys.executable, "-m", "tally", *args], cwd=ROOT, capture_output=True, timeout=60
    )
    return p.returncode, p.stdout.decode(), p.stderr.decode()


def ledger_file(text):
    f = tempfile.NamedTemporaryFile("w", suffix=".csv", delete=False)
    f.write(text)
    f.close()
    return f.name


class HiddenCsv(unittest.TestCase):
    def test_balances_csv(self):
        code, out, err = tally("balances", "--format", "csv", TRIP)
        self.assertEqual((code, out), (0, "person,balance\nAnn,36.00\nBea,-40.00\nCal,40.00\nDan,-36.00\n"), err)

    def test_settle_csv(self):
        code, out, err = tally("settle", "--format", "csv", TRIP)
        self.assertEqual((code, out), (0, "from,to,amount\nBea,Cal,40.00\nDan,Ann,36.00\n"), err)

    def test_names_with_commas_are_quoted(self):
        path = ledger_file('date,payer,amount,participants\nd,"Smith, J",10.00,"Smith, J;Bo"\n')
        code, out, err = tally("balances", "--format", "csv", path)
        self.assertEqual((code, out), (0, 'person,balance\n"Smith, J",5.00\nBo,-5.00\n'), err)
        code, out, err = tally("settle", "--format", "csv", path)
        self.assertEqual((code, out), (0, 'from,to,amount\nBo,"Smith, J",5.00\n'), err)

    def test_nothing_to_list_is_header_only(self):
        empty = ledger_file("date,payer,amount,participants\n")
        self.assertEqual(tally("balances", "--format", "csv", empty)[:2], (0, "person,balance\n"))
        even = ledger_file("date,payer,amount,participants\nd,Ann,10.00,Ann\n")
        self.assertEqual(tally("settle", "--format", "csv", even)[:2], (0, "from,to,amount\n"))

    def test_text_is_default_and_unchanged(self):
        expected = "Ann       36.00\nBea      -40.00\nCal       40.00\nDan      -36.00\n"
        self.assertEqual(tally("balances", TRIP)[:2], (0, expected))
        self.assertEqual(tally("balances", "--format", "text", TRIP)[:2], (0, expected))
        self.assertEqual(tally("settle", "--format", "text", TRIP)[:2], (0, "Bea -> Cal  40.00\nDan -> Ann  36.00\n"))

    def test_unknown_format_exits_2(self):
        code, out, err = tally("balances", "--format", "xml", TRIP)
        self.assertEqual(code, 2, (out, err))
