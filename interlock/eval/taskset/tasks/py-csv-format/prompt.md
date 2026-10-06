Add a `--format` option to `python3 -m tally balances` and `python3 -m tally settle`. It takes `text` (the default: today's output, unchanged) or `csv`.

- `balances --format csv` prints the header `person,balance`, then one row per person in the same order as the text output, with the balance written the way the text output writes it (`36.00`, `-40.00`).
- `settle --format csv` prints the header `from,to,amount`, then one row per transfer in the same order as the text output.
- Fields are quoted the way Python's `csv` module quotes them by default (a name that contains a comma is wrapped in double quotes), and every line ends with `\n`.
- When there is nothing to list, the CSV output is the header line alone.
- An unknown format is rejected by the argument parser, which exits with status 2.

`checks/csv-format.sh` checks the `balances` case for `examples/trip.csv`. The tests run with `python3 -m unittest discover -s tests -q`.
