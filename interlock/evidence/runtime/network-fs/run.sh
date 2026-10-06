#!/bin/sh
# Mounts fuse_probe.py as fuse.<subtype> (root and /dev/fuse needed), then runs
# `interlock init` with the store on it, without and with the override.
# Usage: INTERLOCK_BIN=<interlock> sh run.sh <subtype> > <subtype>.txt
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
mkdir "$work/mnt"
cd "$work" || exit 1
timeout 60 python3 -I "$here/fuse_probe.py" "$work/mnt" "$1" 15 > probe.log 2>&1 &
sleep 2
echo "mount type: $(awk -v m="$work/mnt" '$5 == m { for (i = 7; i <= NF; i++) if ($i == "-") { print $(i + 1); exit } }' /proc/self/mountinfo)"
echo "statfs magic: 0x$(stat -f -c '%t' mnt)"
echo "== interlock init, no override"
unset INTERLOCK_ALLOW_NETWORK_FS
INTERLOCK_DB=mnt/.interlock/state.db "$INTERLOCK_BIN" init 2>&1
echo "exit $?"
echo "== interlock init, INTERLOCK_ALLOW_NETWORK_FS=1"
INTERLOCK_ALLOW_NETWORK_FS=1 INTERLOCK_DB=mnt/.interlock/state.db "$INTERLOCK_BIN" init 2>&1
echo "exit $?"
wait
cd / && rm -rf "${work:?}"
