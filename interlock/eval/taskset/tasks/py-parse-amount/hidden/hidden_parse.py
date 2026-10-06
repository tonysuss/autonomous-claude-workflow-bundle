"""Hidden acceptance checks for py-parse-amount. Each test is one stated
requirement from the task's prompt."""

import os
import tempfile
import unittest

from tally.csvio import read_entries
from tally.money import parse_amount


class HiddenParse(unittest.TestCase):
    def test_one_decimal_digit_is_tenths(self):
        self.assertEqual(parse_amount("12.5"), 1250)
        self.assertEqual(parse_amount("0.5"), 50)

    def test_sign_covers_the_fraction(self):
        self.assertEqual(parse_amount("-0.50"), -50)
        self.assertEqual(parse_amount("-3.25"), -325)
        self.assertEqual(parse_amount("-1,234.5"), -123450)

    def test_more_than_two_places_is_an_error(self):
        for text in ("1.234", "0.001", "-2.500"):
            with self.assertRaises(ValueError, msg=text):
                parse_amount(text)

    def test_existing_behaviour_kept(self):
        self.assertEqual(parse_amount("1,234.56"), 123456)
        self.assertEqual(parse_amount("7"), 700)
        self.assertEqual(parse_amount("-12"), -1200)
        self.assertEqual(parse_amount("  4.10 "), 410)
        for text in ("abc", "", "1.2.3"):
            with self.assertRaises(ValueError, msg=text):
                parse_amount(text)

    def test_csv_rows_use_the_fixed_parser(self):
        with tempfile.NamedTemporaryFile("w", suffix=".csv", delete=False) as f:
            f.write("date,payer,amount,participants\nd,Ann,12.5,Ann;Bea\nd,Bea,-0.50,Ann\n")
        try:
            self.assertEqual([e.amount for e in read_entries(f.name)], [1250, -50])
        finally:
            os.unlink(f.name)
