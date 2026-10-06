#!/bin/sh
# Captures what Copilot CLI prints when it has no sign-in: an empty home and no
# token variables. No model is called. Usage: copilot-auth-failure.sh <copilot binary> <output file>
home=$(mktemp -d)
env -i PATH="$PATH" HOME="$home" NO_COLOR=1 COPILOT_AUTO_UPDATE=false \
  "$1" -p "say done" --output-format json --no-ask-user < /dev/null > "$2" 2>&1
echo "exit $?" >> "$2"
rm -rf "${home:?}"
