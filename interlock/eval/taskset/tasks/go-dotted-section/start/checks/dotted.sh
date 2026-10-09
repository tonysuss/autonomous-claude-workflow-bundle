#!/bin/sh
# Reproduces the report: a key in a section whose name has a dot.
out=$(go run ./cmd/kvconf get checks/dotted.conf db.eu.host 2>&1)
if [ "$out" != "db1.example.org" ]; then
	echo "expected db1.example.org, got: $out"
	exit 1
fi
echo "ok: $out"
