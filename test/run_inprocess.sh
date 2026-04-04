#!/usr/bin/env bash
# Apple-to-apple IN-PROCESS scaling benchmark
# Runs existing in-process scaling tests for both Go and Rust
# Both use: 5 runs × 5000 ops, median, free-run (default threads)
set -euo pipefail

INFRA=/repo/yingjieb/rebornlinux/infra
RESULTS_DIR="$(dirname "$0")/inprocess_results"
mkdir -p "$RESULTS_DIR"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

echo "=== Apple-to-Apple In-Process Scaling Benchmark ==="
echo "Timestamp: $TIMESTAMP"
echo "Results dir: $RESULTS_DIR"
echo ""

# --- Go: in-process scaling (V2 + V3, Msgpack) ---
echo ">>> Go: in-process scaling (TestScalingThroughput) ..."
cd "$INFRA/adaptivemsg-go"
go test -run=TestScalingThroughput -count=1 -v -timeout=600s . 2>&1 \
  | tee "$RESULTS_DIR/go_inprocess_${TIMESTAMP}.log"
echo ""

# --- Rust: in-process scaling (V2 + V3, Postcard + Msgpack) ---
echo ">>> Rust: in-process scaling (benchmark_scaling_all) ..."
cd "$INFRA/adaptivemsg-rust"
AM_BENCH_RUNS=5 AM_BENCH_ITERS=5000 cargo test --release --lib -- --ignored \
  --nocapture --exact scaling_bench_test::benchmark_scaling_all 2>&1 \
  | tee "$RESULTS_DIR/rust_inprocess_${TIMESTAMP}.log"
echo ""

echo "=== Done. Results in $RESULTS_DIR ==="
