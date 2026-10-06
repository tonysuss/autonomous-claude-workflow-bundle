#!/bin/sh
# Spike hook: logs each payload; denies any shell command containing DENY-ME.
payload=$(cat)
printf '%s\n' "$payload" >> "$HOOK_LOG"
case "$payload" in
  *DENY-ME*)
    echo '{"permissionDecision": "deny", "permissionDecisionReason": "spike hook denied it"}'
    echo "spike hook denied it" >&2
    exit 2 ;;
esac
exit 0
