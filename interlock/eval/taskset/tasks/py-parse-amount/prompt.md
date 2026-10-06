`parse_amount` in `tally/money.py` misreads some amounts. A ledger row with the amount `12.5` is recorded as 12.05 instead of 12.50.

Expected behaviour of `parse_amount(text)`, which returns cents:

- One decimal digit means tenths: `parse_amount("12.5") == 1250`.
- The sign applies to the whole amount, fraction included: `parse_amount("-0.50") == -50` and `parse_amount("-3.25") == -325`.
- More than two decimal places is an error: `parse_amount("1.234")` raises `ValueError`.
- Everything that works today keeps working: thousands separators (`"1,234.56"` is 123456), whole numbers (`"7"` is 700), surrounding spaces, and `ValueError` for text that is not an amount.

`checks/parse-repro.sh` reproduces the problem. The tests run with `python3 -m unittest discover -s tests -q`.
