"""Hidden acceptance checks for py-split-remainder. Each test is one stated
requirement from the task's prompt."""

import unittest

from tally.ledger import Entry, Ledger
from tally.money import split_evenly


class HiddenSplit(unittest.TestCase):
    def test_reported_case(self):
        self.assertEqual(split_evenly(100, 3), [34, 33, 33])

    def test_negative_total_mirrors(self):
        self.assertEqual(split_evenly(-100, 3), [-34, -33, -33])
        self.assertEqual(split_evenly(-5, 2), [-3, -2])

    def test_sum_spread_and_order(self):
        for total in range(-60, 61):
            for n in range(1, 8):
                shares = split_evenly(total, n)
                self.assertEqual(len(shares), n, (total, n))
                self.assertEqual(sum(shares), total, (total, n, shares))
                self.assertLessEqual(max(shares) - min(shares), 1, (total, n, shares))
                mags = [abs(s) for s in shares]
                self.assertEqual(mags, sorted(mags, reverse=True), (total, n, shares))

    def test_rejects_non_positive_n(self):
        for n in (0, -2):
            with self.assertRaises(ValueError):
                split_evenly(100, n)

    def test_ledger_balances_sum_to_zero(self):
        ledger = Ledger(
            [
                Entry("d", "Ann", 100, ["Ann", "Bea", "Cal"]),
                Entry("d", "Bea", -1001, ["Cal", "Ann", "Bea"]),
            ]
        )
        balances = ledger.balances()
        self.assertEqual(sum(balances.values()), 0, balances)
        # 1.00 splits [34, 33, 33]; the -10.01 refund splits [-334, -334, -333].
        self.assertEqual(balances, {"Ann": 66 + 334, "Bea": -33 - 1001 + 333, "Cal": -33 + 334})
