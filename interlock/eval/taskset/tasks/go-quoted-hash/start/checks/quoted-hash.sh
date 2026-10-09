#!/bin/sh
# Reproduces the report: a '#' inside a quoted value breaks `kvconf get`.
out=$(go run ./cmd/kvconf get checks/quoted.conf app.title 2>&1)
if [ "$out" != "Issue #42: retry" ]; then
	echo "expected 'Issue #42: retry', got: $out"
	exit 1
fi
echo "ok: $out"
