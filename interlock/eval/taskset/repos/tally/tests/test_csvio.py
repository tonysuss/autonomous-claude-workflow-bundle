import os
import tempfile
import unittest

from tally.csvio import read_entries

EXAMPLE = os.path.join(os.path.dirname(__file__), "..", "examples", "trip.csv")


def write_csv(text):
    f = tempfile.NamedTemporaryFile("w", suffix=".csv", delete=False)
    f.write(text)
    f.close()
    return f.name


class ReadEntriesTest(unittest.TestCase):
    def test_reads_the_example(self):
        entries = read_entries(EXAMPLE)
        self.assertEqual(len(entries), 4)
        self.assertEqual(entries[0].payer, "Ann")
        self.assertEqual(entries[0].amount, 9000)
        self.assertEqual(entries[2].participants, ["Ann", "Bea", "Cal", "Dan"])
        self.assertEqual(entries[3].amount, -1200)

    def test_names_may_contain_commas(self):
        path = write_csv('date,payer,amount,participants,memo\nd,"Smith, J",10.00,"Smith, J;Bo",x\n')
        self.assertEqual(read_entries(path)[0].participants, ["Smith, J", "Bo"])

    def test_missing_columns(self):
        path = write_csv("date,payer,amount\nd,Ann,1.00\n")
        with self.assertRaisesRegex(ValueError, "missing columns: participants"):
            read_entries(path)

    def test_bad_amount_names_the_line(self):
        path = write_csv("date,payer,amount,participants\nd,Ann,1.00,Ann\nd,Bea,lots,Bea\n")
        with self.assertRaisesRegex(ValueError, ":3:"):
            read_entries(path)


if __name__ == "__main__":
    unittest.main()
