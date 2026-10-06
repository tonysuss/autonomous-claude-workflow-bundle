#!/bin/sh
# cc_guided.sh BIN_DIR REPO OUT PROMPT: one live guided Claude Code session (sonnet) in REPO, with
# the plugin `interlock setup --host claude-code` installed there (skills, verifier agent,
# interactive hooks) and interlock from BIN_DIR on PATH. The prompt stands in for the person;
# the stream goes to OUT, and a one-line summary of the result to stdout.
BIN="$1"; REPO="$2"; OUT="$3"; PROMPT="$4"
cd "$REPO" || exit 1
export PATH="$BIN:$PATH"
unset INTERLOCK_DB INTERLOCK_ATTEMPT INTERLOCK_TOKEN INTERLOCK_MODE INTERLOCK_TREE
printf '%s' "$PROMPT" | timeout 1800 claude -p --model sonnet --output-format stream-json --verbose \
  --no-session-persistence --setting-sources "" --plugin-dir "$REPO/.interlock/guided/claude-code-plugin" \
  --permission-mode acceptEdits --max-turns 90 \
  --allowedTools Bash Read Edit Write Grep Glob Skill Agent > "$OUT" 2> "$OUT.err"
echo "exit: $?"
jq -c 'select(.type=="result") | {is_error, num_turns, total_cost_usd, duration_ms, denials: (.permission_denials | length), result: (.result | tostring | .[0:2500])}' "$OUT"
