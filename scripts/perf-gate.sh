#!/usr/bin/env bash
# Fail when a scenario is slower than the committed baseline by more than the threshold.
#
#   scripts/perf-gate.sh
#   scripts/perf-gate.sh --threshold 25
#   scripts/perf-gate.sh --json
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo bench --bench crawl -- --compare benches/perf-baseline.json "$@"
