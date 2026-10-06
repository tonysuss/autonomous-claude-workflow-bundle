import unittest

from tally.money import format_cents, parse_amount, split_evenly


class ParseAmountTest(unittest.TestCase):
    def test_two_decimal_places(self):
        self.assertEqual(parse_amount("12.50"), 1250)
        self.assertEqual(parse_amount("0.05"), 5)

    def test_whole_numbers(self):
        self.assertEqual(parse_amount("7"), 700)

    def test_thousands_separators(self):
        self.assertEqual(parse_amount("1,234.56"), 123456)

    def test_surrounding_space(self):
        self.assertEqual(parse_amount(" 3.00 "), 300)

    def test_rejects_text(self):
        for bad in ["abc", ""]:
            with self.assertRaises(ValueError):
                parse_amount(bad)


class FormatCentsTest(unittest.TestCase):
    def test_formats(self):
        self.assertEqual(format_cents(1250), "12.50")
        self.assertEqual(format_cents(-5), "-0.05")
        self.assertEqual(format_cents(0), "0.00")


class SplitEvenlyTest(unittest.TestCase):
    def test_even_split(self):
        self.assertEqual(split_evenly(90, 3), [30, 30, 30])

    def test_one_share(self):
        self.assertEqual(split_evenly(7, 1), [7])


if __name__ == "__main__":
    unittest.main()
