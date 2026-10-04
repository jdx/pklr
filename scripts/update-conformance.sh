#!/usr/bin/env bash
# Regenerate the expected output of tests/conformance/*.pkl with the reference
# Pkl implementation: <case>.json, or <case>.error when Pkl reports an error.
set -euo pipefail
cd "$(dirname "$0")/../tests/conformance"
pkl_version="${PKL_VERSION:-0.32.1}"
# Fail before touching any expected output if Pkl itself can't run.
mise x "pkl@$pkl_version" -- pkl --version >/dev/null
for case in *.pkl; do
  name="${case%.pkl}"
  stdout="$(mktemp)"
  stderr="$(mktemp)"
  if mise x "pkl@$pkl_version" -- pkl eval -f json "$case" >"$stdout" 2>"$stderr"; then
    mv "$stdout" "$name.json"
    rm -f "$name.error"
  else
    # Pkl reports `–– Pkl Error ––` followed by the message. Anything else is
    # a failure to run Pkl, not an expected error.
    message="$(sed -n '2p' "$stderr")"
    if [[ "$(sed -n '1p' "$stderr")" != *"Pkl Error"* || -z "$message" ]]; then
      echo "$case: pkl failed without reporting an error:" >&2
      cat "$stderr" >&2
      exit 1
    fi
    printf '%s\n' "$message" >"$name.error"
    rm -f "$name.json" "$stdout"
  fi
  rm -f "$stderr"
done
