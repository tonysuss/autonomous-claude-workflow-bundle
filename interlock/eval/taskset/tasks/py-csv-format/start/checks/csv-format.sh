#!/bin/sh
# Checks `balances --format csv` for the trip example, byte for byte.
expected=$(mktemp)
actual=$(mktemp)
printf 'person,balance\nAnn,36.00\nBea,-40.00\nCal,40.00\nDan,-36.00\n' > "$expected"
python3 -m tally balances --format csv examples/trip.csv > "$actual" || exit 1
if ! cmp -s "$expected" "$actual"; then
	echo "unexpected output:"
	od -c "$actual" | head -20
	exit 1
fi
echo "ok: balances --format csv"
