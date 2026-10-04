#!/usr/bin/env bash
# Regenerate tests/conformance/*.json with the reference Pkl implementation.
set -euo pipefail
cd "$(dirname "$0")/../tests/conformance"
pkl_version="${PKL_VERSION:-0.32.1}"
for case in *.pkl; do
  mise x "pkl@$pkl_version" -- pkl eval -f json "$case" >"${case%.pkl}.json"
done
