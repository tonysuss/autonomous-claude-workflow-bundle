#!/bin/sh
# Times `copilot --help` repeatedly to see whether it ever hangs on its own.
bin=${COPILOT_BIN:?set COPILOT_BIN to the copilot binary}
i=0
while [ "$i" -lt "$1" ]; do
  i=$((i + 1))
  s=$(date +%s%N)
  NO_COLOR=1 COPILOT_AUTO_UPDATE=false timeout 40 "$bin" --help < /dev/null > /dev/null 2>&1
  rc=$?
  e=$(date +%s%N)
  echo "run $i rc=$rc ms=$(( (e - s) / 1000000 ))"
done
