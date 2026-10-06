#!/bin/sh
# Checks that balances prints large amounts with thousands separators.
out=$(python3 -m tally balances examples/big.csv) || exit 1
for want in "122,856.78" "-121,810.78" "-223.00" "-823.00"; do
	if ! printf '%s\n' "$out" | grep -q -- " $want\$"; then
		echo "missing $want in:"
		printf '%s\n' "$out"
		exit 1
	fi
done
echo "ok: thousands separators in balances"
