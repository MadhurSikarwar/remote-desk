#!/usr/bin/env bash
# Runs a command; when it fails, repeats the tail of its output as an error
# annotation so the cause is visible on the run summary page.
set -u

log="$(mktemp)"
"$@" 2>&1 | tee "$log"
code=${PIPESTATUS[0]}

if [ "$code" -ne 0 ]; then
  # A test run lists every test before the reasons for the failures, so a
  # plain tail would show the list and cut the reasons off: start at them.
  shown="$(grep -v -E '^\s*(Compiling|Checking|Downloading|Downloaded|Locking|Adding|Finished|Running|Blocking) ' "$log" \
    | sed -e 's/\r$//')"
  if printf '%s\n' "$shown" | grep -q '^failures:$'; then
    shown="$(printf '%s\n' "$shown" | sed -n '/^failures:$/,$p' | head -n 80)"
  else
    shown="$(printf '%s\n' "$shown" | tail -n 80)"
  fi
  tail_text="$(printf '%s\n' "$shown" | sed -e 's/%/%25/g' | awk '{ printf "%s%%0A", $0 }')"
  echo "::error title=$*::${tail_text}"
fi
exit "$code"
