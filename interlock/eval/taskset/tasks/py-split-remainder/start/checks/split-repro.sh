#!/bin/sh
# Reproduces the report: splitting 1.00 three ways loses a cent.
python3 - <<'PY'
from tally.ledger import Entry, Ledger
from tally.money import split_evenly

shares = split_evenly(100, 3)
assert shares == [34, 33, 33], f"split_evenly(100, 3) = {shares}, expected [34, 33, 33]"
balances = Ledger([Entry("2026-06-01", "Ann", 100, ["Ann", "Bea", "Cal"])]).balances()
assert sum(balances.values()) == 0, f"balances do not sum to zero: {balances}"
print("ok: 1.00 split three ways keeps every cent")
PY
