#!/usr/bin/env bash
# Runs a command; when it fails, repeats the tail of its output as an error
# annotation so the cause is visible on the run summary page.
set -u

log="$(mktemp)"
"$@" 2>&1 | tee "$log"
code=${PIPESTATUS[0]}

if [ "$code" -ne 0 ]; then
  tail_text="$(grep -v -E '^\s*(Compiling|Checking|Downloading|Downloaded|Locking|Adding|Finished|Running|Blocking) ' "$log" \
    | tail -n 80 | sed -e 's/%/%25/g' -e 's/\r$//' | awk '{ printf "%s%%0A", $0 }')"
  echo "::error title=$*::${tail_text}"
fi
exit "$code"
