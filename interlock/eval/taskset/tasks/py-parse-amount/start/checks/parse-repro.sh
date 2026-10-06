#!/bin/sh
# Reproduces the report: 12.5 is read as 12.05.
python3 - <<'PY'
from tally.money import parse_amount

cents = parse_amount("12.5")
assert cents == 1250, f"parse_amount('12.5') = {cents}, expected 1250"
print("ok: 12.5 is 1250 cents")
PY
