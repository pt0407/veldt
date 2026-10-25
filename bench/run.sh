#!/bin/bash
# Usage: ./bench/run.sh [veldt_binary]  — runs each workload, prints timings
BIN=${1:-cargo run -q --}
cd "$(dirname "$0")/.."
for b in bench/*.veldt; do
  echo "--- $b"
  $BIN run --steps 50000000 "$b" 2>/dev/null
done
