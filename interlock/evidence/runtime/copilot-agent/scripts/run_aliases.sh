#!/bin/sh
# One session per alias probe agent; only the tools offered matter.
S=<scratch>/agentspike
for n in names search shell agent web customagent todo empty mcp; do
  rm -rf "$S/runs/t-$n"
  sh "$S/scripts/probe.sh" "t-$n" "$S/plugin-c" "interlock-hooks:t-$n" --allow-all-tools >/dev/null
  printf '%s: ' "$n"
  python3 -I -c "
import json,sys
try:
    print(json.loads(open('$S/runs/t-$n/model.jsonl').readline())['tool_names'])
except Exception as e:
    print('no request', open('$S/runs/t-$n/stderr.txt').read()[:300])
"
done
