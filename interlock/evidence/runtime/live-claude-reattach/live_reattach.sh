#!/bin/sh
# Live demonstration: kill -9 the interlock supervisor during a real Claude
# Code session, restart it, and record what interlock did.
# Usage: live_reattach.sh <evidence-dir> <scratch-demo-dir> <seconds-before-kill>
set -u
BIN=/home/user/autonomous-claude-workflow-bundle/.claude/worktrees/agent-a015a201024863c38/interlock/target/debug/interlock
OUT=$1
DEMO=$2
WAIT=$3
rm -rf "${DEMO:?}"
mkdir -p "$DEMO" "$OUT"
cd "$DEMO" || exit 1

printf 'def add(a, b):\n    return a - b\n' > calc.py
printf "python3 -c 'import calc; r = calc.add(2, 3); assert r == 5, r'\n" > check.sh
printf '__pycache__/\n' > .gitignore
cat > task.toml <<'EOF'
id = "fix-add"
repository = "."
workflow = "bug-fix"
intent = "add() returns the difference instead of the sum"

[budget]
max_attempts = 3
max_cost_usd = 1.0

[scope]
paths = ["calc.py", "tests/**"]

[[criterion]]
id = "fixed"
statement = "add(2, 3) returns 5"
check = "sh check.sh"
min_strength = "tested"
producer = "self"
baseline = "fails"

[[criterion]]
id = "verified"
statement = "An independent run of the check passes"
check = "sh check.sh"
min_strength = "observed"
producer = "independent"
baseline = "fails"
EOF
git init -q -b main
git add -A
git -c user.name=demo -c user.email=demo@example.invalid commit -q -m init

"$BIN" init > /dev/null
printf '[pins]\nclaude-code = "2.1.289"\n' > .interlock/config.toml
"$BIN" task create task.toml > "$OUT/01-task-create.json"
"$BIN" host inspect --host claude-code > "$OUT/02-host-inspect.json"

echo "$(date -u +%FT%TZ) starting supervisor 1" | tee "$OUT/00-timeline.txt"
"$BIN" run fix-add --host claude-code --model haiku --timeout 10m > "$OUT/03-run-1-report.json" 2> "$OUT/03-run-1-stderr.txt" &
SUP=$!
echo "$(date -u +%FT%TZ) supervisor 1 pid $SUP" | tee -a "$OUT/00-timeline.txt"

HOST=""
i=0
while [ $i -lt 900 ]; do
  HOST=$("$BIN" attempt list fix-add 2>/dev/null | python3 -c 'import json,sys
a=[x for x in json.load(sys.stdin) if x.get("handoff") and x["status"]=="running"]
print(a[0]["handoff"]["pid"] if a else "")')
  [ -n "$HOST" ] && break
  sleep 0.2
  i=$((i + 1))
done
echo "$(date -u +%FT%TZ) worker session started, host pid $HOST (handoff recorded)" | tee -a "$OUT/00-timeline.txt"
sleep "$WAIT"
ps -o pid,pgid,ppid,stat,etime,args -p "$SUP,$HOST" | cut -c1-200 > "$OUT/04-ps-before-kill.txt"
kill -9 "$SUP"
wait "$SUP"
echo "$(date -u +%FT%TZ) kill -9 supervisor 1 (exit status $?)" | tee -a "$OUT/00-timeline.txt"
sleep 1
if ps -p "$HOST" -o stat= | grep -qv Z; then
  echo "$(date -u +%FT%TZ) host pid $HOST is still running" | tee -a "$OUT/00-timeline.txt"
else
  echo "$(date -u +%FT%TZ) host pid $HOST is gone" | tee -a "$OUT/00-timeline.txt"
fi
ps -o pid,pgid,ppid,stat,etime,args -p "$HOST" | cut -c1-200 > "$OUT/05-ps-after-kill.txt"
"$BIN" attempt list fix-add > "$OUT/06-attempts-while-no-supervisor.json"

echo "$(date -u +%FT%TZ) starting supervisor 2" | tee -a "$OUT/00-timeline.txt"
"$BIN" run fix-add --host claude-code --model haiku --timeout 10m > "$OUT/07-run-2-report.json" 2> "$OUT/07-run-2-stderr.txt"
echo "$(date -u +%FT%TZ) supervisor 2 exited with status $?" | tee -a "$OUT/00-timeline.txt"

"$BIN" task log fix-add > "$OUT/08-task-log.json"
"$BIN" attempt list fix-add > "$OUT/09-attempts.json"
"$BIN" task events fix-add > "$OUT/10-events.json"
"$BIN" status fix-add > "$OUT/11-status.json"
TREE=$("$BIN" task show fix-add | python3 -c 'import json,sys; print(json.load(sys.stdin).get("current_tree") or "")')
{
  echo "accepted output tree: $TREE"
  [ -n "$TREE" ] && git show "$TREE:calc.py"
  echo "user's branch: $(git branch --show-current) at $(git rev-parse HEAD)"
  echo "user's calc.py:"
  cat calc.py
  echo "git status --porcelain:"
  git status --porcelain
} > "$OUT/12-output-and-user-branch.txt" 2>&1
mkdir -p "$OUT/transcripts"
cp .interlock/transcripts/*.jsonl "$OUT/transcripts/" 2>/dev/null
echo "$(date -u +%FT%TZ) done" | tee -a "$OUT/00-timeline.txt"
