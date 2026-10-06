#!/bin/sh
# Mounts fuse_probe.py twice: as fuse.sshfs, and as fuse.gocryptfs whose
# source is the first mount, as gocryptfs names its cipher directory. Then
# runs `interlock init` with the store on the second (root and /dev/fuse needed).
# Usage: INTERLOCK_BIN=<interlock> sh layered.sh > layered.txt
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
mkdir "$work/remote" "$work/plain"
cd "$work" || exit 1
timeout 60 python3 -I "$here/fuse_probe.py" "$work/remote" sshfs 15 > remote.log 2>&1 &
sleep 2
timeout 60 python3 -I "$here/fuse_probe.py" "$work/plain" gocryptfs 12 "$work/remote" > plain.log 2>&1 &
sleep 2
echo "remote: $(awk -v m="$work/remote" '$5 == m { for (i = 7; i <= NF; i++) if ($i == "-") { print $(i + 1); exit } }' /proc/self/mountinfo)"
echo "plain: $(awk -v m="$work/plain" '$5 == m { for (i = 7; i <= NF; i++) if ($i == "-") { print $(i + 1), "from", $(i + 2); exit } }' /proc/self/mountinfo | sed "s#$work#<work>#")"
echo "== interlock init, store on plain/"
unset INTERLOCK_ALLOW_NETWORK_FS
INTERLOCK_DB=plain/.interlock/state.db "$INTERLOCK_BIN" init 2>&1
echo "exit $?"
wait
cd / && rm -rf "${work:?}"
