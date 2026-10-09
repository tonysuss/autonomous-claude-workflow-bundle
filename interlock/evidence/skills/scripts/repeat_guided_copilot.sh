#!/bin/sh
# repeat_guided_copilot.sh COPILOT OUT_DIR N: runs the guided Copilot tests N times in a row with
# INTERLOCK_REQUIRE_HOSTS=1 (a missing host fails), keeping each run's output and evidence in
# OUT_DIR/run-<i>/. Run from interlock/.
COPILOT="$1"; OUT="$2"; N="$3"
export INTERLOCK_COPILOT_BIN="$COPILOT" INTERLOCK_REQUIRE_HOSTS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
i=1
while [ "$i" -le "$N" ]; do
  mkdir -p "$OUT/run-$i"
  INTERLOCK_EVIDENCE_DIR="$OUT/run-$i" cargo test -p interlock-cli --test guided_copilot > "$OUT/run-$i/cargo-test.txt" 2>&1
  echo "run $i: exit $? $(grep 'test result' "$OUT/run-$i/cargo-test.txt")"
  i=$((i + 1))
done
