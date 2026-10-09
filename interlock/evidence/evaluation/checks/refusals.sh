#!/bin/sh
# Things `harness.py run` must refuse before any session starts. No model calls.
# Usage: refusals.sh HARNESS INTERLOCK_BIN MISNAMED_INTERLOCK_BIN COPILOT_BIN SCRATCH
H=$1; IL=$2; MISNAMED=$3; COP=$4; S=$5
common="--host copilot --fake-model --tasks py-split-remainder --repeats 1 --budget-usd 1 --copilot-bin $COP --state-dir $S/state --results $S/refusal-results"

try() {
	label=$1; shift
	out=$(python3 "$H" run $common "$@" 2>&1)
	echo "$label: exit $? :: $(printf '%s' "$out" | tail -1)"
}

try "misnamed interlock binary" --interlock-bin "$MISNAMED"
try "--interlock-skills without interlock run --skills" --interlock-bin "$IL" --skills-dir "$S/dummy-skills" --interlock-skills
try "--skills-generate without interlock skills generate" --interlock-bin "$IL" --conditions skills --skills-generate
try "skills condition without a skills directory" --interlock-bin "$IL" --conditions skills
try "run base inside the state dir" --interlock-bin "$IL" --run-base "$S/state/runs"
try "results inside the run base" --interlock-bin "$IL" --run-base "$S/rb" --results "$S/rb/results"
