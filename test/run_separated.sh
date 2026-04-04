#!/usr/bin/env bash
# Apple-to-apple PROCESS-SEPARATED scaling benchmark
# Runs Go and Rust probes with separate server/client processes
# Both use Msgpack codec, 5 runs × 5000 ops, default env (no pinning)
# Tests V2 (plain) and V3 (recovery) across the standard config matrix
set -euo pipefail

PROBE_DIR="$(cd "$(dirname "$0")" && pwd)"
RESULTS_DIR="$PROBE_DIR/separated_results"
mkdir -p "$RESULTS_DIR"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

GO_PROBE="$PROBE_DIR/go-process-probe/probe"
RUST_PROBE="$PROBE_DIR/rust-process-probe/target/release/am_rust_process_probe"

RUNS=5
ITERS=5000
CONFIGS="1:1 1:4 1:16 1:64 4:1 4:4 4:16 4:64"

GO_PORT_BASE=18100
RUST_PORT_BASE=18200

echo "=== Apple-to-Apple Process-Separated Scaling Benchmark ==="
echo "Timestamp: $TIMESTAMP"
echo "Runs: $RUNS, Iterations: $ITERS"
echo "Configs: $CONFIGS"
echo ""

# Build probes if needed
if [ ! -x "$GO_PROBE" ]; then
    echo "Building Go probe..."
    (cd "$PROBE_DIR/go-process-probe" && go build -o probe .)
fi
if [ ! -x "$RUST_PROBE" ]; then
    echo "Building Rust probe..."
    (cd "$PROBE_DIR/rust-process-probe" && cargo build --release 2>/dev/null)
fi

run_probe() {
    local lang=$1 mode=$2 conns=$3 streams=$4 run_num=$5 port=$6
    local probe server_pid addr="127.0.0.1:$port"
    local env_vars=()

    if [ "$lang" = "go" ]; then
        probe="$GO_PROBE"
    else
        probe="$RUST_PROBE"
        env_vars+=(AM_CODEC=msgpack)
    fi
    if [ "$mode" = "v3" ]; then
        env_vars+=(AM_RECOVERY=1)
    fi

    # Start server
    env "${env_vars[@]}" "$probe" server "$addr" &
    server_pid=$!
    sleep 0.5

    # Run client
    local output
    output=$(env "${env_vars[@]}" "$probe" client "$addr" "$conns" "$streams" "$ITERS" 2>&1) || true

    # Kill server
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true

    echo "$output"
}

collect_median() {
    # Extract ops_per_sec from all runs, compute median
    local -a values=()
    while IFS= read -r line; do
        local val
        val=$(echo "$line" | grep -oP 'ops_per_sec=\K[0-9.]+' || true)
        if [ -n "$val" ]; then
            values+=("$val")
        fi
    done
    if [ ${#values[@]} -eq 0 ]; then
        echo "0"
        return
    fi
    printf '%s\n' "${values[@]}" | sort -n | awk 'NR==int((NR+1)/2){printf "%.0f\n", $1}'
}

run_matrix() {
    local lang=$1 mode=$2 port_base=$3 logfile=$4
    echo ">>> $lang $mode (Msgpack, $RUNS runs × $ITERS ops)" | tee -a "$logfile"

    local port_offset=0
    for cfg in $CONFIGS; do
        local conns=${cfg%%:*}
        local streams=${cfg##*:}
        local port=$((port_base + port_offset))
        port_offset=$((port_offset + 1))
        local all_output=""

        for run in $(seq 1 $RUNS); do
            local output
            output=$(run_probe "$lang" "$mode" "$conns" "$streams" "$run" "$port")
            all_output+="$output"$'\n'
            # Stagger ports to avoid bind conflicts
            port=$((port + 80))
        done

        local median
        median=$(echo "$all_output" | collect_median)
        printf "  %d conn × %-2d stream  %6s ops/sec  (median of %d runs)\n" \
            "$conns" "$streams" "$median" "$RUNS" | tee -a "$logfile"
    done
    echo "" | tee -a "$logfile"
}

LOGFILE="$RESULTS_DIR/separated_${TIMESTAMP}.log"
echo "Results: $LOGFILE"
echo ""

# V2 (no recovery)
run_matrix "go"   "v2" "$GO_PORT_BASE"   "$LOGFILE"
run_matrix "rust" "v2" "$RUST_PORT_BASE"  "$LOGFILE"

# V3 (recovery enabled)
GO_PORT_BASE=$((GO_PORT_BASE + 1000))
RUST_PORT_BASE=$((RUST_PORT_BASE + 1000))
run_matrix "go"   "v3" "$GO_PORT_BASE"   "$LOGFILE"
run_matrix "rust" "v3" "$RUST_PORT_BASE"  "$LOGFILE"

echo "=== Done. Full log: $LOGFILE ==="
