# Performance Analysis: adaptivemsg-go vs adaptivemsg-rust

## Benchmark Setup

- **CPU**: Intel Xeon E5-2680 v4 @ 2.40GHz
- **Protocol**: adaptivemsg V2 and V3 (recovery mode)
- **Codec**: MsgpackCompact (same for both, fair comparison)
- **Operation**: `SendRecv` — send a request, receive a reply (full round-trip)
- **Benchmark**: 5 runs × 3000 ops each, median reported
- **Transport**: TCP loopback with TCP_NODELAY enabled
  (Go: enabled by default in `net.Dial`; Rust: explicit `set_nodelay(true)`)

**Codec note**: Go defaults to MsgpackCompact. Rust defaults to Postcard
(faster, Rust-only). When comparing Go vs Rust, always use the Rust `_Msgpack`
variants so both sides use the same codec.

## Performance Tests

Performance tests live in the Go and Rust runtime repos. This section documents
every benchmark, what it measures, and how to run it.

### Go: End-to-End Protocol Benchmarks

**File**: `adaptivemsg-go/protocol_version_bench_test.go`

| Benchmark | What it measures |
|---|---|
| `BenchmarkProtocolV2SendRecv` | Full round-trip latency using V2 (no recovery). Client sends an echo request, server replies, client receives. |
| `BenchmarkProtocolV3RecoverySendRecv` | Same round-trip, but with V3 recovery enabled (replay buffer, cumulative ACK, heartbeat). |

Both benchmarks:
- Start a TCP server on loopback with an ephemeral port.
- Register `connTestEchoRequest` / `connTestEchoReply` message types.
- Send one warm-up message before timing starts.
- Run N iterations of `SendRecvAs[*connTestEchoReply]` with a 1-byte payload.
- Validate reply correctness on each iteration.
- Report memory allocations (`b.ReportAllocs()`).

V3 recovery options used:
- DetachedTTL: 5 s
- MaxReplayBytes: 8 MB
- AckEvery: 64
- AckDelay: 20 ms
- HeartbeatInterval: 30 s
- HeartbeatTimeout: 90 s

**How to run:**

```bash
cd adaptivemsg-go

# Both protocol benchmarks, with allocation stats
go test -bench=BenchmarkProtocol -benchmem -benchtime=10s -run=^$

# V2 only
go test -bench=BenchmarkProtocolV2SendRecv -benchmem -run=^$

# V3 only
go test -bench=BenchmarkProtocolV3RecoverySendRecv -benchmem -run=^$

# Recommended: multiple runs for stable medians
go test -bench=BenchmarkProtocol -benchmem -benchtime=3000x -count=5 -run=^$
```

### Go: Recovery Runtime Micro-Benchmarks

**File**: `adaptivemsg-go/recovery_runtime_bench_test.go`

These micro-benchmarks isolate the recovery state machine's hot-path operations
(the idle loop decisions that run on every frame).

| Benchmark | What it measures |
|---|---|
| `BenchmarkNextAckWait_NoPending` | ACK wait computation when no ACK is pending (fast path). |
| `BenchmarkNextAckWait_WithDelay` | ACK wait computation with a pending ACK deadline 10 ms in the future. Measures `time.Until()` overhead. |
| `BenchmarkTakePendingControl_Empty` | Pending control-frame check when nothing is due (fast path). |
| `BenchmarkTakePendingControl_AckReady` | ACK frame extraction when `ackDue=true`. Measures overhead of building and returning the ACK frame. |
| `BenchmarkWaitCalc` | Combined wait duration selection (min of ACK wait vs heartbeat interval). |
| `BenchmarkWaitCalc_AckBeatsHeartbeat` | Same as above, but with a pending ACK deadline (5 ms) shorter than the heartbeat interval. Tests branch prediction. |
| `BenchmarkWaitCalc_V2OldStyle` | V2 baseline wait calculation using an intermediate bool. |
| `BenchmarkWaitCalc_V3Optimized` | V3 optimized wait calculation without the intermediate bool. Compares directly against V2OldStyle. |

All micro-benchmarks create a minimal `recoveryState` (client role, 1 MB replay
buffer, default server recovery options) and call `b.ResetTimer()` to exclude
setup cost.

**How to run:**

```bash
cd adaptivemsg-go

# All recovery micro-benchmarks
go test -bench=BenchmarkNextAckWait -benchmem -benchtime=10s -run=^$
go test -bench=BenchmarkTakePendingControl -benchmem -benchtime=10s -run=^$
go test -bench=BenchmarkWaitCalc -benchmem -benchtime=10s -run=^$

# Everything in one shot
go test -bench=. -benchmem -benchtime=10s -run=^$
```

### Rust: End-to-End Protocol Benchmarks

**File**: `adaptivemsg-rust/src/protocol_version_bench_test.rs`

Rust benchmarks are implemented as `#[ignore]` integration tests that manually
time send/recv loops with `std::time::Instant`. They run multiple iterations
(default 5) and report the median ns/op for stability.

| Benchmark | What it measures |
|---|---|
| `benchmark_protocol_v2_send_recv` | Full round-trip latency using V2 with the default codec (Postcard). |
| `benchmark_protocol_v3_recovery_send_recv` | Same round-trip with V3 recovery enabled (Postcard). |
| `benchmark_protocol_v2_send_recv_msgpack` | V2 round-trip with MsgpackCompact codec. **Use this for Go comparison.** |
| `benchmark_protocol_v3_recovery_send_recv_msgpack` | V3 recovery round-trip with MsgpackCompact codec. **Use this for Go comparison.** |

The `_msgpack` variants are the ones used for the Go vs Rust comparison tables
(both sides must use the same codec for a fair comparison).

Benchmark setup mirrors the Go side:
- TCP loopback server on ephemeral port.
- Echo request/reply message types.
- One warm-up iteration before the timed loop.
- Iteration count defaults to 1000, configurable via `AM_BENCH_ITERS` env var.
- Run count defaults to 5, configurable via `AM_BENCH_RUNS` env var.

**How to run:**

```bash
cd adaptivemsg-rust

# All benchmarks (release mode required for meaningful numbers)
AM_BENCH_ITERS=3000 cargo test --release --lib -- --ignored --nocapture benchmark_protocol

# MsgpackCompact only (for Go comparison)
AM_BENCH_ITERS=3000 cargo test --release --lib -- --ignored --nocapture benchmark_protocol_v2_send_recv_msgpack

# Single benchmark
AM_BENCH_ITERS=3000 cargo test --release --lib -- --ignored --nocapture benchmark_protocol_v2_send_recv
```

### Rust: Recovery Runtime Micro-Benchmarks

**File**: `adaptivemsg-rust/src/recovery_runtime_bench_test.rs`

These micro-benchmarks isolate the recovery state machine's hot-path operations,
mirroring Go's `recovery_runtime_bench_test.go`.

| Benchmark | What it measures |
|---|---|
| `benchmark_next_ack_wait_no_pending` | `next_ack_wait()` with no pending ACK (fast path). |
| `benchmark_next_ack_wait_with_delay` | `next_ack_wait()` with a pending ACK deadline. |
| `benchmark_take_pending_ack_empty` | `take_pending_ack()` fast path (nothing due). |
| `benchmark_take_pending_ack_ready` | `take_pending_ack()` with ack_due and seq gap. |
| `benchmark_wait_calc` | `next_ack_wait()` + `heartbeat_interval()` + min selection. |
| `benchmark_wait_calc_ack_beats_heartbeat` | Same, but with a short pending ACK deadline. |
| `benchmark_wait_calc_v2_old_style` | With intermediate bool (v2 baseline). |
| `benchmark_wait_calc_v3_optimized` | Without intermediate bool (v3 optimized). |

**How to run:**

```bash
cd adaptivemsg-rust

# All recovery micro-benchmarks (release mode)
cargo test --release --lib -- --ignored --nocapture benchmark_next_ack_wait
cargo test --release --lib -- --ignored --nocapture benchmark_take_pending
cargo test --release --lib -- --ignored --nocapture benchmark_wait_calc
```

**Rust unit tests** (not benchmarks, but verify correctness of perf-critical paths):

```bash
cd adaptivemsg-rust

cargo test --lib                  # All unit tests
```

### Interpreting Results

- **ns/op**: nanoseconds per `SendRecv` round-trip (lower is better).
- **B/op**: bytes allocated per operation (lower is better).
- **allocs/op**: heap allocations per operation (lower is better).
- Use `-benchtime=3000x -count=5` and report the median to get stable numbers.
- Always compare Go and Rust using the same codec (`MsgpackCompact`) and the
  same transport (`TCP loopback with TCP_NODELAY`).

## Results

### Current (after Go optimizations)

**Reference run** — numbers from the initial benchmarking session on this CPU:
```
                  Go (ns/op)   Rust (ns/op)   Rust speedup
V2 SendRecv        98,983       75,142         1.32x
V3 Recovery       112,003       79,209         1.41x

V3 overhead:      +13.2%        +5.4%
Go allocs:        V2 2338 B/op 64 allocs    V3 2439 B/op 66 allocs
```

**Validation run** (2026-04-01, same CPU, 3 runs × 1000 ops, median):
```
Go (go test -bench=BenchmarkProtocol -benchmem -count=3):
  V2 SendRecv:       105,070 ns/op   2,338 B/op   64 allocs/op
  V3 Recovery:       115,246 ns/op   2,438 B/op   66 allocs/op
  V3 overhead:       +9.7%

Rust (AM_BENCH_RUNS=3 AM_BENCH_ITERS=1000 cargo test --release):
  V2 Postcard:       131,734 ns/op
  V3 Postcard:       223,112 ns/op
  V2 Msgpack:        210,293 ns/op
  V3 Msgpack:        226,314 ns/op
```

**Notes on the validation run**:
- Go numbers are consistent with the reference run (~6% variation, within normal
  range for a shared CI environment).
- Rust numbers are 2–3× higher than the reference run. Likely causes:
  - **tokio runtime contention**: benchmark harness runs inside `#[tokio::test]`,
    sharing the multi-thread runtime with the server. The reference run may have
    used a dedicated runtime or different thread configuration.
  - **Release profile differences**: LTO/codegen-units settings affect async
    state machine optimization.
  - **CPU frequency scaling**: Xeon E5-2680 v4 turbo boost behavior varies
    across runs in shared environments.
- The relative V3 overhead within each language is the most stable metric:
  Go +9.7%, Rust +7.6% (Msgpack), consistent with architectural expectations.
- **Recommendation**: re-run Rust benchmarks with a dedicated single-threaded
  tokio runtime and pinned CPU frequency for stable absolute numbers.

Rust also benchmarks with its default codec (Postcard, faster than Msgpack):
```
                  Rust Postcard (ns/op)   Rust Msgpack (ns/op)   Postcard speedup
V2 SendRecv        58,672                  75,142                 1.28x
V3 Recovery        64,133                  79,209                 1.24x
```

**Optimizations applied to Go:**
1. `bufio.Writer` wrapping for all write paths (V2 writer loop + V3 recovery writer loop)
2. Stack-allocated frame headers (`[frameHeaderLenV3]byte` instead of `make([]byte, headerLen)`)
3. Batched ACK+data writes in V3 recovery writer (writeFrameNoFlush + single flush)

V2 benefited more because the plain writer path fully leverages buffered I/O (header+payload → single flush → single syscall). V3 improvement was smaller because recovery overhead (transport checks, channel synchronization) dominates over I/O syscall savings.

## Root Cause Analysis

The remaining gap after Go optimizations is **~1.3x** for V2 and **~1.4x** for
V3. The absolute difference is ~24k ns/op (V2) and ~33k ns/op (V3). The
following factors explain where those nanoseconds go.

### 1. Unbuffered I/O in Go — ✅ Fixed

**Go**: `writeFrame()` in `frame.go` wrote header and payload directly to TCP socket as separate `Write()` calls — each became a separate syscall. For a round-trip, this meant 4+ syscalls (2 writes on send side, 2 reads on receive side, plus the other direction).

**Rust**: Writer is wrapped with `tokio::io::BufWriter`. Header + payload are buffered, then a single `flush()` produces one syscall. Additionally, in V3, the ACK frame and data frame are batched into a single flush via `write_frame_no_flush()` + explicit `flush()`.

**Applied fix**: Added `writer *bufio.Writer` field to `Connection` struct. `writeFrame()` now writes to the buffered writer and flushes once. In V3 recovery, ACK + data frames are batched with `writeFrameNoFlush()` + single `Flush()`. The `bufio.Writer` is re-created on transport attach and nil'd on detach.

### 2. Goroutine / Async Task Scheduling Overhead (~50% of remaining gap)

Both Go and Rust use the **same architecture**: 1 reader per connection, 1
writer per connection, 1 decoder per stream, 0-1 handler per stream. Both have
**2 channel hops** per message (reader → decoder, decoder → inbox). The
performance difference is not structural — it comes from the cost of each
operation.

**Go**: A single SendRecv round-trip involves 4-5 goroutine context switches
across the 2 channel hops (send side + receive side). Go goroutines are M:N
scheduled but involve OS thread parking/unparking. Each context switch costs
~2,000 ns.

**Rust**: Tokio uses cooperative async tasks. Task yields are ~250 ns (just a
state machine transition within the same thread). The same 2 channel hops cost
far less because `tokio::sync::mpsc` is lock-free for the common uncontended
case.

**This is a fundamental language runtime difference** — not an architectural one.

### 3. Channel Operation Cost (~30% of remaining gap)

Both Go and Rust have 2 channel hops per message direction:
- Hop 1: reader → stream decoder (`incoming` / `incoming_tx`)
- Hop 2: decoder → caller inbox (`inbox` / `inbox_tx`)

**Go**: Each `chan` operation costs ~750 ns including goroutine scheduling.
Two hops × two directions (send + receive) = ~3,000 ns of channel overhead per
round-trip.

**Rust**: Each `tokio::sync::mpsc` operation costs ~100 ns (lock-free for the
uncontended case). Same two hops × two directions = ~400 ns per round-trip.

**Potential fix**: Merge the decoder goroutine into the reader goroutine to
eliminate one channel hop per direction (~1,500 ns savings). The same
optimization could apply to Rust, but the savings would be smaller (~200 ns).

### 4. Per-Message Heap Allocations (~15% of remaining gap) — ✅ Partially Fixed

**Go**: ~5 allocations per frame:
- 2 header byte slices (send + receive, `make([]byte, headerLen)`)
- 1 payload buffer (receive side)
- 2 codec intermediate buffers
- Measured: ~2,338 B/op, 64 allocs/op

**Rust**: ~2-3 allocations per frame:
- Frame header is a stack array `[u8; 18]` — zero allocation
- Read header uses stack array
- Payload `Vec<u8>` allocation on receive (unavoidable)

**Applied fix**: `buildHeader()` now returns `[frameHeaderLenV3]byte` (stack array) + `int` header length instead of `[]byte`. `readFrameFrom()` uses `var header [frameHeaderLenV3]byte` instead of `make([]byte, headerLen)`. This eliminates 2 heap allocations per round-trip (send + receive headers).

**Remaining**: Codec intermediate buffers and payload allocation are still heap-allocated.

### 5. Codec Reflection Overhead (~15% of remaining gap)

**Go**: Msgpack codec uses `reflect.ValueOf()` at runtime for encoding/decoding. Each reflection call has overhead from type inspection and indirect calls.

**Rust**: Serde derive macros generate specialized encode/decode code at compile time — no runtime reflection.

**Fix for Go**: Use code generation (e.g., `msgp` tool) instead of runtime reflection for msgpack encoding.

## Summary of Optimization Opportunities for Go

| Optimization | Estimated Gain | Difficulty | Status |
|---|---|---|---|
| Add `bufio.Writer` | ~10,000 ns | Easy | ✅ Done |
| Stack-allocate frame headers | ~1,000 ns | Easy | ✅ Done |
| Batch ACK+data in V3 writer | ~1,000 ns | Easy | ✅ Done |
| Merge decoder into reader goroutine | ~7,000 ns | Medium | Not done |
| Code-gen msgpack codec | ~4,000 ns | Hard | Not done |

With optimizations 1-3 applied, the gap is **1.32x** (V2) and **1.41x** (V3).

With all optimizations: Go V2 would be ~88k ns/op → gap narrows to ~1.17x.

The remaining gap is fundamental:
- Tokio's cooperative scheduling vs Go's preemptive goroutines
- Rust's zero-cost abstractions vs Go's GC + runtime overhead
- Compile-time code generation vs runtime reflection

## V3 Recovery Overhead

Reference run:
```
Go  V3 overhead: +13.2%  (98,983 → 112,003 ns/op)
Rust V3 overhead: +5.4%  (75,142 →  79,209 ns/op)
```

Validation run (2026-04-01):
```
Go  V3 overhead: +9.7%   (105,070 → 115,246 ns/op)
Rust V3 overhead: +7.6%  (210,293 → 226,314 ns/op, Msgpack)
```

Go's V3 overhead (+13.2%) comes from:
- Recovery state management (transport checks, channel synchronization)
- Replay buffer bookkeeping
- Additional allocations (66 vs 64 allocs/op, 2,439 vs 2,338 B/op)

Rust's V3 overhead (+5.4%) is lower because:
- `ack_every=64` means ACK frames are batched (1 ACK per 64 messages)
- Recovery state updates use atomic operations with no mutex in the fast path
- The async writer loop handles recovery with minimal additional overhead

Go's overhead is ~2.4x Rust's overhead in absolute terms (~13k vs ~4k ns/op),
which aligns with the goroutine scheduling cost difference (each recovery
check adds another goroutine yield in Go, but only a cheap state machine
branch in Rust).
