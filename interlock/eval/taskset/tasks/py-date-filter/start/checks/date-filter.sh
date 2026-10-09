#!/bin/sh
# Checks balances --since 2026-06-02 for the trip example. Order and people
# with a zero balance do not matter.
out=$(python3 -m tally balances --since 2026-06-02 examples/trip.csv) || exit 1
printf '%s\n' "$out" | python3 -c '
import sys
got = {}
for line in sys.stdin:
    if line.strip():
        name, amount = line.split()
        if amount != "0.00":
            got[name] = amount
want = {"Bea": "-10.00", "Cal": "70.00", "Ann": "-24.00", "Dan": "-36.00"}
assert got == want, f"expected {want}, got {got}"
print("ok: balances --since 2026-06-02")
'
