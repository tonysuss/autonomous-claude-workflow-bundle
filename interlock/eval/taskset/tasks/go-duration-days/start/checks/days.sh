#!/bin/sh
# Reproduces the report: GetDuration rejects 30d.
out=$(go run ./checks/days 2>&1)
if [ "$out" != "720h0m0s" ]; then
	echo "expected 720h0m0s, got: $out"
	exit 1
fi
echo "ok: 30d is $out"
