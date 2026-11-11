#!/usr/bin/env bash
# Refresh the committed baseline. Run this on the local machine that will run the gate.
#
#   scripts/update-baseline.sh
#   scripts/update-baseline.sh --samples 9
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo bench --bench crawl -- --save-baseline benches/perf-baseline.json "$@"
