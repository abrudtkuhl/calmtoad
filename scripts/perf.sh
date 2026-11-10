#!/usr/bin/env bash
# Run the performance harness. Arguments are forwarded to the benchmark.
#
#   scripts/perf.sh
#   scripts/perf.sh --json
#   scripts/perf.sh --samples 9
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo bench --bench crawl -- "$@"
