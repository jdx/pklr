#!/usr/bin/env bash
# Regenerate the expected output of tests/conformance/*.pkl with the reference
# Pkl implementation: <case>.json, or <case>.error when Pkl reports an error.
set -euo pipefail
cd "$(dirname "$0")/../tests/conformance"
pkl_version="${PKL_VERSION:-0.32.1}"
for case in *.pkl; do
  name="${case%.pkl}"
  rm -f "$name.json" "$name.error"
  if ! mise x "pkl@$pkl_version" -- pkl eval -f json "$case" >"$name.json" 2>"$name.stderr"; then
    # Keep only the error message, the line after the `–– Pkl Error ––` header.
    sed -n '2p' "$name.stderr" >"$name.error"
    rm "$name.json"
  fi
  rm "$name.stderr"
done
