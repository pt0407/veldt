#!/bin/bash
# The Living Garden demo — code that gets measurably better while running.
#
#   1. plant a deliberately wasteful `score` (dead work inside its loop)
#   2. real calls record traces — the garden's ground truth
#   3. evolve: mutants trialled on tests + probes + traces + step-cost
#   4. a simplified variant wins: same behavior, fewer steps per call
#   5. call_best dispatches to it — no code change, no redeploy
#
# Usage: ./demo/run_demo.sh [veldt_binary]
BIN=${1:-./target/debug/veldt}
cd "$(dirname "$0")/.."

echo "== 0. fresh garden"
$BIN clear >/dev/null 2>&1

echo "== 1. plant + real calls (traces recorded)"
$BIN run examples/live_demo.veldt

echo
echo "== 2. evolve 8 cycles"
$BIN evolve 8 2>/dev/null | grep -E "BIRTH|TRIAL|DIED|PROBE" | head -12

echo
echo "== 3. lineage — the leaner strain takes over"
$BIN lineage score

echo
echo "== 4. same program, better code — call_best now uses the winner"
$BIN run examples/live_demo.veldt
