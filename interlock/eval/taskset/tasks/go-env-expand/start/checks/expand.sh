#!/bin/sh
# checks/expand.conf sets paths.data from $DATA_ROOT.
out=$(DATA_ROOT=/srv go run ./cmd/kvconf get checks/expand.conf paths.data 2>&1)
if [ "$out" != "/srv/data" ]; then
	echo "expected /srv/data, got: $out"
	exit 1
fi
echo "ok: paths.data expanded to $out"
