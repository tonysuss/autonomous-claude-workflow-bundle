"""Hidden acceptance checks for py-date-filter, on the real command line.
Balances are compared as name -> amount, ignoring order and people whose
balance is zero, since the prompt does not say whether they are listed."""

import os
import subprocess
import sys
import tempfile
import unittest

ROOT = os.getcwd()
TRIP = os.path.join(ROOT, "examples", "trip.csv")


def tally(*args):
    p = subprocess.run([sys.executable, "-m", "tally", *args], cwd=ROOT, capture_output=True, timeout=60)
    return p.returncode, p.stdout.decode(), p.stderr.decode()


def balances(out):
    got = {}
    for line in out.splitlines():
        if line.strip() and line.strip() != "no entries":
            name, amount = line.split()
            if amount != "0.00":
                got[name] = amount
    return got


def transfers(out):
    return sorted(tuple(l.split()) for l in out.splitlines() if l.strip() and "->" in l)


def ledger(text):
    f = tempfile.NamedTemporaryFile("w", suffix=".csv", delete=False)
    f.write(text)
    f.close()
    return f.name


class HiddenDates(unittest.TestCase):
    def test_since(self):
        code, out, err = tally("balances", "--since", "2026-06-02", TRIP)
        self.assertEqual((code, balances(out)), (0, {"Bea": "-10.00", "Cal": "70.00", "Ann": "-24.00", "Dan": "-36.00"}), err)

    def test_until(self):
        code, out, err = tally("balances", "--until", "2026-06-01", TRIP)
        self.assertEqual((code, balances(out)), (0, {"Ann": "60.00", "Bea": "-30.00", "Cal": "-30.00"}), err)

    def test_both_bounds_are_inclusive(self):
        code, out, err = tally("balances", "--since", "2026-06-02", "--until", "2026-06-02", TRIP)
        self.assertEqual((code, balances(out)), (0, {"Bea": "-10.00", "Cal": "70.00", "Ann": "-30.00", "Dan": "-30.00"}), err)

    def test_settle_is_filtered(self):
        code, out, err = tally("settle", "--since", "2026-06-03", TRIP)
        self.assertEqual((code, transfers(out)), (0, [("Dan", "->", "Ann", "6.00")]), err)

    def test_invalid_option_date_exits_2(self):
        for bad in ("2026-02-30", "yesterday"):
            code, out, err = tally("balances", "--since", bad, TRIP)
            self.assertEqual(code, 2, (bad, out, err))
        code, out, err = tally("settle", "--until", "2026-13-01", TRIP)
        self.assertEqual(code, 2, (out, err))

    def test_invalid_entry_date_names_file_and_line(self):
        path = ledger("date,payer,amount,participants\n2026-06-01,Ann,10.00,Ann;Bea\n2026-13-01,Bea,10.00,Bea\n")
        code, out, err = tally("balances", "--since", "2026-01-01", path)
        self.assertEqual(code, 1, (out, err))
        self.assertIn(f"{os.path.basename(path)}:3:", err)
        self.assertIn("2026-13-01", err)

    def test_without_options_nothing_changes(self):
        self.assertEqual(
            tally("balances", TRIP)[:2], (0, "Ann       36.00\nBea      -40.00\nCal       40.00\nDan      -36.00\n")
        )
        # Dates are not checked when no option is given.
        path = ledger("date,payer,amount,participants\nsoon,Ann,10.00,Ann;Bea\n")
        self.assertEqual(tally("balances", path)[0], 0)
