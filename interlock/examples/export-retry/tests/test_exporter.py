import unittest

from exporter import Sink, TransientError, export


class ExportTest(unittest.TestCase):
    def test_writes_every_row(self):
        sink = Sink()
        self.assertEqual(export([{"id": 1}, {"id": 2}], sink), 2)
        self.assertEqual([r["id"] for r in sink.rows], [1, 2])

    def test_gives_up_after_retries(self):
        class AlwaysFails(Sink):
            def write(self, row):
                raise TransientError("down")

        with self.assertRaises(TransientError):
            export([{"id": 1}], AlwaysFails(), retries=2)


if __name__ == "__main__":
    unittest.main()
