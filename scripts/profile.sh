#!/usr/bin/env bash
# Profile the synthetic workload and write a flamegraph.
#
#   scripts/profile.sh
#   scripts/profile.sh audit --seconds 5
#
# Requires cargo-flamegraph: cargo install flamegraph
# On Linux, install perf (linux-tools) as well.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! cargo flamegraph --version >/dev/null 2>&1; then
  echo "cargo-flamegraph is not installed." >&2
  echo "Install it with: cargo install flamegraph" >&2
  echo "On Linux you also need perf (for example: sudo apt install linux-tools-generic)." >&2
  exit 1
fi

exec cargo flamegraph --example profile -- "$@"
