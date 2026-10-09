#!/bin/sh
# Usage: probe.sh <run-name> <plugin-dir> <agent-name|-> [extra copilot args...]
# Runs one offline Copilot session against the spike model and keeps what it sent.
set -u
S=<scratch>/agentspike
COP=<scratch>/cop/node_modules/.bin/copilot
name=$1; plugin=$2; agent=$3; shift 3
out="$S/runs/$name"
mkdir -p "$out/home" "$out/repo"
[ -f "$out/repo/a.txt" ] || echo hi > "$out/repo/a.txt"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
FAKE_LOG="$out/model.jsonl" FAKE_LOG_SYSTEM=1 FAKE_SCRIPT_FILE="$S/script.json" python3 -I "$S/scripts/spike_model.py" "$port" &
mpid=$!
sleep 1
set -- -p "AGENT-PROBE run the probe" --output-format json --no-ask-user --no-auto-update --plugin-dir "$plugin" "$@"
[ "$agent" = "-" ] || set -- "$@" --agent "$agent"
(cd "$out/repo" && env COPILOT_HOME="$out/home" COPILOT_OFFLINE=true COPILOT_MODEL=gpt-4.1 COPILOT_AUTO_UPDATE=false \
  COPILOT_PROVIDER_BASE_URL="http://127.0.0.1:$port/v1" NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  timeout 120 "$COP" "$@" > "$out/events.jsonl" 2> "$out/stderr.txt")
echo "exit $?" > "$out/exit.txt"
kill "$mpid" 2>/dev/null
cat "$out/exit.txt"
