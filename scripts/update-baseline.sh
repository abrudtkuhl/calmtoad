#!/usr/bin/env bash
# Refresh the committed baseline. Run this on the machine (or CI runner) that will gate.
#
#   scripts/update-baseline.sh
#   scripts/update-baseline.sh --samples 9
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo bench --bench crawl -- --save-baseline benches/perf-baseline.json "$@"
