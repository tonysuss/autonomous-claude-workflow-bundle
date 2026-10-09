#!/bin/sh
# Runs one offline Copilot session against fake.py and prints what it reported.
here=$(dirname "$0")
cd "$here" || exit 1
python3 copilot-probe-model.py 18765 &
fake=$!
sleep 1
COP=${COPILOT_BIN:?set COPILOT_BIN to the copilot binary}
mkdir -p home
COPILOT_HOME=$PWD/home COPILOT_OFFLINE=true COPILOT_MODEL=gpt-4.1 \
  COPILOT_PROVIDER_BASE_URL=http://127.0.0.1:18765/v1 NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  timeout 60 "$COP" -p "say done" --output-format json --no-ask-user --no-auto-update \
  --session-id 0cb916db-26aa-40f2-86b5-1ba81b225fd2 --usage-output-file "$PWD/usage.json" > out.jsonl 2> err.txt
echo "exit $?"
tail -n 3 out.jsonl
echo "--- usage file"
cat usage.json
echo
echo "--- stderr"
head -c 600 err.txt
kill "$fake"
