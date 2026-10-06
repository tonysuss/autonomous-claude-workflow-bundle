#!/bin/sh
# Tool lists and filters with no agent (a0), a worker agent with a tools list (a3),
# and an agent with no tools list (a4). Each denies `git push` with --deny-tool.
S=<scratch>/agentspike
deny='--deny-tool=shell(git push:*)'
for r in a0 a3 a4; do mkdir -p "$S/runs/$r"; rm -f "$S/runs/$r/hook.jsonl" "$S/runs/$r/model.jsonl"; done
HOOK_LOG=$S/runs/a0/hook.jsonl sh "$S/scripts/probe.sh" a0 "$S/plugin-b" - --allow-tool=shell --allow-tool=write "$deny"
HOOK_LOG=$S/runs/a3/hook.jsonl sh "$S/scripts/probe.sh" a3 "$S/plugin-b" interlock-hooks:interlock-worker --allow-tool=shell --allow-tool=write "$deny"
HOOK_LOG=$S/runs/a4/hook.jsonl sh "$S/scripts/probe.sh" a4 "$S/plugin-b" interlock-hooks:interlock-open --allow-tool=shell --allow-tool=write "$deny"
