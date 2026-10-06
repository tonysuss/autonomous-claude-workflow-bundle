import os
import unittest

from tally.csvio import load_ledger
from tally.ledger import Entry, Ledger

EXAMPLE = os.path.join(os.path.dirname(__file__), "..", "examples", "trip.csv")


class LedgerTest(unittest.TestCase):
    def test_balances(self):
        ledger = Ledger([Entry("2026-06-01", "Ann", 9000, ["Ann", "Bea", "Cal"])])
        self.assertEqual(ledger.balances(), {"Ann": 6000, "Bea": -3000, "Cal": -3000})

    def test_people_in_first_seen_order(self):
        ledger = Ledger([Entry("d", "Cal", 100, ["Ann", "Cal"]), Entry("d", "Bea", 100, ["Bea"])])
        self.assertEqual(ledger.people(), ["Cal", "Ann", "Bea"])

    def test_settle(self):
        ledger = Ledger([Entry("2026-06-01", "Ann", 9000, ["Ann", "Bea", "Cal"])])
        self.assertEqual(ledger.settle(), [("Bea", "Ann", 3000), ("Cal", "Ann", 3000)])

    def test_example_balances_sum_to_zero(self):
        self.assertEqual(sum(load_ledger(EXAMPLE).balances().values()), 0)

    def test_settling_the_example_clears_every_balance(self):
        ledger = load_ledger(EXAMPLE)
        balances = ledger.balances()
        for debtor, creditor, cents in ledger.settle():
            balances[debtor] += cents
            balances[creditor] -= cents
        self.assertTrue(all(v == 0 for v in balances.values()), balances)


if __name__ == "__main__":
    unittest.main()
