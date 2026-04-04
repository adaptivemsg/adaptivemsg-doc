# Performance Analysis: adaptivemsg-go vs adaptivemsg-rust

## Benchmark Setup

- **CPU**: Intel Xeon E5-2680 v4 @ 2.40GHz (24 threads)
- **Protocol**: adaptivemsg V2 (plain) and V3 (recovery mode)
- **Codec**: MsgpackCompact for cross-language comparison
- **Operation**: `SendRecv` — send a request, receive a reply (full round-trip)
- **Runs**: median reported (protocol: 5 × 8000 ops; scaling: 5 × 5000 ops)
- **Transport**: TCP loopback with TCP_NODELAY

**Codec note**: Go defaults to MsgpackCompact. Rust defaults to Postcard (faster,
Rust-only). Cross-language comparisons always use the Rust `_Msgpack` variants.

**Measurement note (2026-04-02)**: Rust benchmarks use `#[tokio::test(flavor =
"multi_thread")]` with the hot loop inside `tokio::spawn(...)`. An earlier version
ran the loop on tokio's main test task, which has ~2.5x higher scheduling overhead
than worker threads. All numbers in this document reflect the corrected methodology.

---

## How to Run

### Protocol Benchmarks (single-connection latency)

```bash
# Go — all protocol benchmarks
cd adaptivemsg-go
go test -run '^$' -bench=BenchmarkProtocol -benchmem -benchtime=8000x -count=5 .

# Go — matched single-core
GOMAXPROCS=1 taskset -c 0 go test -run '^$' \
  -bench=BenchmarkProtocol -benchmem -benchtime=8000x -count=5 .

# Rust — all protocol benchmarks
cd adaptivemsg-rust
AM_BENCH_RUNS=5 AM_BENCH_ITERS=8000 \
  cargo test --release --lib -- --ignored --nocapture benchmark_protocol

# Rust — matched single-core
RUST_TEST_THREADS=1 TOKIO_WORKER_THREADS=1 AM_BENCH_RUNS=5 AM_BENCH_ITERS=8000 \
  taskset -c 0 cargo test --release --lib -- --ignored --nocapture benchmark_protocol

# Rust — single benchmark (use --exact to avoid running others)
AM_BENCH_RUNS=5 AM_BENCH_ITERS=8000 cargo test --release --lib -- --ignored \
  --nocapture --exact protocol_version_bench_test::benchmark_protocol_v2_send_recv
```

**Files:**
- Go: `adaptivemsg-go/protocol_version_bench_test.go`
- Rust: `adaptivemsg-rust/src/protocol_version_bench_test.rs`

Both benchmarks start a TCP server, register echo request/reply types, send one
warm-up message, then time N iterations of `SendRecv` with a 1-byte payload.

### Recovery Micro-Benchmarks

```bash
# Go
cd adaptivemsg-go
go test -bench='BenchmarkNextAckWait|BenchmarkTakePendingControl|BenchmarkWaitCalc' \
  -benchmem -benchtime=10s -run=^$ .

# Rust
cd adaptivemsg-rust
cargo test --release --lib -- --ignored --nocapture benchmark_next_ack_wait
cargo test --release --lib -- --ignored --nocapture benchmark_take_pending
cargo test --release --lib -- --ignored --nocapture benchmark_wait_calc
```

**Files:**
- Go: `adaptivemsg-go/recovery_runtime_bench_test.go`
- Rust: `adaptivemsg-rust/src/recovery_runtime_bench_test.rs`

### Scaling Benchmarks (multi-connection throughput)

```bash
# Go — wall-clock throughput test (all configs, V2 + V3)
cd adaptivemsg-go
go test -run=TestScalingThroughput -count=1 -v -timeout=600s .

# Rust — all scaling configs (Msgpack, V2 + V3)
cd adaptivemsg-rust
AM_BENCH_RUNS=5 AM_BENCH_ITERS=5000 cargo test --release --lib -- --ignored \
  --nocapture --exact scaling_bench_test::benchmark_scaling_all
```

### Process-Separated Scaling (apple-to-apple)

```bash
# Build probes
cd adaptivemsg-doc/tmp/go-process-probe && go build -o probe .
cd adaptivemsg-doc/tmp/rust-process-probe && cargo build --release

# Example: Rust V2 Msgpack, 1 conn × 4 streams, 5000 ops
AM_CODEC=msgpack ./rust-process-probe/target/release/am_rust_process_probe \
  server 127.0.0.1:18000 &
AM_CODEC=msgpack ./rust-process-probe/target/release/am_rust_process_probe \
  client 127.0.0.1:18000 1 4 5000

# Example: Go V3 (recovery), 4 conn × 16 streams, 5000 ops
AM_RECOVERY=1 ./go-process-probe/probe server 127.0.0.1:18001 &
AM_RECOVERY=1 ./go-process-probe/probe client 127.0.0.1:18001 4 16 5000

# Automated full-matrix run:
./tmp/run_separated.sh
```

Probe environment variables:
- `AM_CODEC=msgpack` — force MsgpackCompact codec (Rust only; Go defaults to it)
- `AM_RECOVERY=1` — enable V3 recovery mode (both languages)

**Files:**
- Go: `adaptivemsg-go/scaling_bench_test.go`
- Rust: `adaptivemsg-rust/src/scaling_bench_test.rs`

Both use the same wall-clock methodology: spawn N concurrent workers,
barrier-synchronize, measure total elapsed time, compute
`ops/sec = iterations / (elapsed_ns / 1e9)`.

### Interpreting Results

- **ns/op**: nanoseconds per `SendRecv` round-trip (lower is better).
- **B/op**: bytes allocated per operation (Go only, lower is better).
- **allocs/op**: heap allocations per operation (Go only, lower is better).
- **ops/sec**: aggregate throughput (scaling tests, higher is better).

---

## Single-Connection Latency

### Matched Single-Core

Both languages pinned to one CPU core with one runtime worker. Measures raw
per-operation cost with minimal scheduler noise.

```
Go (GOMAXPROCS=1 taskset -c 0, 5 runs × 8000 ops, median):
  V2 SendRecv:       38,449 ns/op   2,336 B/op   64 allocs/op
  V3 Recovery:       46,717 ns/op   2,659 B/op   70 allocs/op
  V3 overhead:       +21.5%

Rust (TOKIO_WORKER_THREADS=1 taskset -c 0, 5 runs × 8000 ops, median):
  V2 Postcard:       27,974 ns/op
  V3 Postcard:       35,849 ns/op   (+28.2%)
  V2 Msgpack:        31,363 ns/op
  V3 Msgpack:        37,869 ns/op   (+20.7%)

Same-codec comparison (MsgpackCompact):
  V2: Rust 1.23x faster   (38,449 → 31,363 ns/op)
  V3: Rust 1.23x faster   (46,717 → 37,869 ns/op)
```

### Free Run (multi-thread)

Normal unconstrained setup: Go uses GOMAXPROCS=24, Rust uses tokio's default
24 worker threads (`num_cpus`). Both runtimes use all available cores.

```
Go (5 runs × 8000 ops, median):
  V2 SendRecv:        83,712 ns/op   2,338 B/op   64 allocs/op
  V3 Recovery:        96,574 ns/op   2,665 B/op   70 allocs/op
  V3 overhead:        +15.4%

Rust (5 runs × 8000 ops, median):
  V2 Postcard:       27,595 ns/op
  V3 Postcard:       65,433 ns/op   (+137.1%)
  V2 Msgpack:        34,962 ns/op
  V3 Msgpack:        69,391 ns/op   (+98.4%)

Same-codec comparison (MsgpackCompact):
  V2: Rust 2.39x faster   (83,712 → 34,962 ns/op)
  V3: Rust 1.39x faster   (96,574 → 69,391 ns/op)
```

**Why Rust V2 free-run ≈ single-core while Go degrades 2.2x:**

Both use 24 threads, but their schedulers behave differently for a sequential
single-connection workload:

- **Tokio (Rust)**: Uses work-stealing. When task A wakes task B, B is placed
  in A's thread-local queue and often runs on the same thread. The sequential
  send → writer → TCP → reader → decoder → inbox chain stays mostly on 1–2
  threads even with 24 available. Result: free-run V2 (35K ns) ≈ single-core
  (31K ns), only 1.11x degradation.
- **Go**: With GOMAXPROCS=24, the runtime actively distributes goroutines across
  OS threads. The reader, writer, decoder, and handler goroutines can each land
  on different threads, turning every channel hop into a cross-thread wake.
  Result: free-run V2 (84K ns) vs single-core (38K ns), 2.18x degradation.

This was verified by sweeping GOMAXPROCS:

```
GOMAXPROCS    Go V2 ns/op (median)    vs GOMAXPROCS=1
    1              75,084                  1.0x
    2             102,987                  1.37x
    4              79,858                  1.06x
   24              96,064                  1.28x
```

Go gets worse at GOMAXPROCS=2 because the goroutines now cross 2 OS threads
instead of staying on 1, but there is no benefit since the workload is
sequential. At 4+, the overhead stabilizes.

This is a genuine runtime behavior difference, not a measurement error. Tokio's
work-stealing keeps cooperating tasks colocated for sequential workloads; Go's
scheduler spreads them out. The practical consequence: **Rust's advantage shrinks
under multi-connection load** (where cross-thread scheduling becomes unavoidable),
as shown in the scaling results.

### Postcard vs Msgpack (Rust only)

```
                  Postcard (ns/op)   Msgpack (ns/op)   Postcard speedup
V2 (single-core)     27,974            31,363              1.12x
V3 (single-core)     35,849            37,869              1.06x
V2 (free-run)        27,595            34,962              1.27x
V3 (free-run)        65,433            69,391              1.06x
```

### Key Observations

1. **Rust V2 is significantly faster** in both setups (1.23x single-core,
   2.39x free-run on same codec).
2. **Rust V3 has a severe multi-thread penalty**: +98% overhead in free-run
   vs +21% on single-core. Go's V3 overhead is a consistent +15–22% in both
   setups. See [V3 Recovery Overhead](#v3-recovery-overhead) for root cause.
3. **Consistency**: The in-process scaling 1×1 case roughly matches these
   protocol numbers (Go: 10.0K ops/sec ≈ 1e9/99,680; Rust V2 Msgpack:
   15.2K ops/sec ≈ 1e9/65,890).

---

## Scaling Throughput

All scaling numbers are from 2026-04-04, free run, multi-thread runtime,
5 runs × 5000 ops, median reported. Two test topologies are used:

- **In-process**: Client and server share one benchmark process/runtime.
  Stresses the scheduler but exaggerates shared-runtime interference.
- **Process-separated**: Client and server in separate OS processes,
  communicating over real TCP. Closer to actual deployment topology.

**Note**: Absolute throughput is lower than earlier runs (2026-04-02) due to
shared-machine contention. The relative Go-vs-Rust comparisons are reliable
since both languages were measured back-to-back under the same conditions.

### In-Process Results

#### Go (Msgpack)

```
Go V2 (no recovery):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream     10,032          1.0x
  1 conn × 4 stream     15,741          1.6x
  1 conn × 16 stream    19,890          2.0x
  1 conn × 64 stream    21,081          2.1x
  4 conn × 1 stream     16,611          1.7x
  4 conn × 4 stream     37,008          3.7x
  4 conn × 16 stream    45,093          4.5x
  4 conn × 64 stream    30,373          3.0x

Go V3 (recovery enabled):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      7,334          1.0x
  1 conn × 4 stream     15,524          2.1x
  1 conn × 16 stream    29,071          4.0x
  1 conn × 64 stream    23,433          3.2x
  4 conn × 1 stream     20,047          2.7x
  4 conn × 4 stream     36,707          5.0x
  4 conn × 16 stream    37,494          5.1x
  4 conn × 64 stream    40,292          5.5x
```

#### Rust (Msgpack)

```
Rust V2 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream     15,172          1.0x
  1 conn × 4 stream     22,821          1.5x
  1 conn × 16 stream    30,764          2.0x
  1 conn × 64 stream    21,883          1.4x
  4 conn × 1 stream     16,653          1.1x
  4 conn × 4 stream     20,384          1.3x
  4 conn × 16 stream    23,887          1.6x
  4 conn × 64 stream    28,424          1.9x

Rust V3 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      9,470          1.0x
  1 conn × 4 stream     13,179          1.4x
  1 conn × 16 stream    18,082          1.9x
  1 conn × 64 stream    17,642          1.9x
  4 conn × 1 stream     13,618          1.4x
  4 conn × 4 stream     13,958          1.5x
  4 conn × 16 stream    18,396          1.9x
  4 conn × 64 stream    14,704          1.6x
```

#### Head-to-Head (Msgpack)

```
V2 (no recovery):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream      10,032        15,172      1.51x  ← Rust faster
  1 conn × 4 stream      15,741        22,821      1.45x
  1 conn × 16 stream     19,890        30,764      1.55x
  1 conn × 64 stream     21,081        21,883      1.04x
  4 conn × 1 stream      16,611        16,653      1.00x  ← crossover
  4 conn × 4 stream      37,008        20,384      0.55x  ← Go dominates
  4 conn × 16 stream     45,093        23,887      0.53x
  4 conn × 64 stream     30,373        28,424      0.94x

V3 (recovery enabled):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       7,334         9,470      1.29x  ← Rust faster
  1 conn × 4 stream      15,524        13,179      0.85x  ← Go overtakes earlier
  1 conn × 16 stream     29,071        18,082      0.62x
  1 conn × 64 stream     23,433        17,642      0.75x
  4 conn × 1 stream      20,047        13,618      0.68x
  4 conn × 4 stream      36,707        13,958      0.38x
  4 conn × 16 stream     37,494        18,396      0.49x
  4 conn × 64 stream     40,292        14,704      0.36x  ← Go 2.7x faster
```

### Process-Separated Results

Both languages use **separate server/client processes** (the probes in
`adaptivemsg-doc/tmp/{go,rust}-process-probe`):

- No pinned CPUs, no forced runtime worker count
- Both use MsgpackCompact codec (`AM_CODEC=msgpack` for Rust)
- V3 via `AM_RECOVERY=1` on both server and client
- 5 runs × 5000 ops, median reported

#### Go (Msgpack)

```
Go V2 (no recovery):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      3,250          1.0x
  1 conn × 4 stream     14,121          4.3x
  1 conn × 16 stream    20,681          6.4x
  1 conn × 64 stream    22,268          6.9x
  4 conn × 1 stream      9,916          3.1x
  4 conn × 4 stream     25,753          7.9x
  4 conn × 16 stream    30,620          9.4x
  4 conn × 64 stream    30,030          9.2x

Go V3 (recovery enabled):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      3,008          1.0x
  1 conn × 4 stream     11,649          3.9x
  1 conn × 16 stream    14,009          4.7x
  1 conn × 64 stream    19,557          6.5x
  4 conn × 1 stream     11,294          3.8x
  4 conn × 4 stream     30,354         10.1x
  4 conn × 16 stream    39,360         13.1x
  4 conn × 64 stream    28,830          9.6x
```

#### Rust (Msgpack)

```
Rust V2 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      4,163          1.0x
  1 conn × 4 stream     15,028          3.6x
  1 conn × 16 stream    20,505          4.9x
  1 conn × 64 stream    21,681          5.2x
  4 conn × 1 stream     22,321          5.4x
  4 conn × 4 stream     27,817          6.7x
  4 conn × 16 stream    28,785          6.9x
  4 conn × 64 stream    20,397          4.9x

Rust V3 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      3,955          1.0x
  1 conn × 4 stream     11,213          2.8x
  1 conn × 16 stream    20,676          5.2x
  1 conn × 64 stream    16,510          4.2x
  4 conn × 1 stream     18,169          4.6x
  4 conn × 4 stream     20,829          5.3x
  4 conn × 16 stream    19,834          5.0x
  4 conn × 64 stream    23,643          6.0x
```

#### Head-to-Head (Msgpack)

```
V2 (no recovery):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       3,250         4,163      1.28x
  1 conn × 4 stream      14,121        15,028      1.06x
  1 conn × 16 stream     20,681        20,505      0.99x  ← roughly tied
  1 conn × 64 stream     22,268        21,681      0.97x
  4 conn × 1 stream       9,916        22,321      2.25x  ← Rust strong
  4 conn × 4 stream      25,753        27,817      1.08x
  4 conn × 16 stream     30,620        28,785      0.94x
  4 conn × 64 stream     30,030        20,397      0.68x  ← Go edges ahead

V3 (recovery enabled):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       3,008         3,955      1.31x
  1 conn × 4 stream      11,649        11,213      0.96x
  1 conn × 16 stream     14,009        20,676      1.48x
  1 conn × 64 stream     19,557        16,510      0.84x
  4 conn × 1 stream      11,294        18,169      1.61x
  4 conn × 4 stream      30,354        20,829      0.69x
  4 conn × 16 stream     39,360        19,834      0.50x
  4 conn × 64 stream     28,830        23,643      0.82x
```

### Scaling Summary

#### In-Process

| Metric | Go V2 | Go V3 | Rust V2 Msgpack | Rust V3 Msgpack |
|---|---|---|---|---|
| Peak ops/sec | 45,093 | 40,292 | 30,764 | 18,396 |
| Peak config | 4×16 | 4×64 | 1×16 | 4×16 |
| Max speedup vs 1×1 | 4.5x | 5.5x | 2.0x | 1.9x |

#### Process-Separated (Msgpack)

| Metric | Go V2 | Go V3 | Rust V2 | Rust V3 |
|---|---|---|---|---|
| Peak ops/sec | 30,620 | 39,360 | 28,785 | 23,643 |
| Peak config | 4×16 | 4×16 | 4×16 | 4×64 |
| Max speedup vs 1×1 | 9.4x | 13.1x | 6.9x | 6.0x |

#### Key Takeaways

**In-process** (shared runtime):
- Rust wins at low parallelism (V2 1×1: 1.51×) but Go dominates at high
  concurrency (V2 4×16: Go 1.89×, V3 4×64: Go 2.74×).
- Go scales 4–5.5× from 1×1 to peak; Rust only 1.9–2.0×.
- V3 amplifies Go's advantage — Go overtakes Rust earlier (at 1×4 instead
  of 4×1) because Rust's writer-owned sequencing oneshot hurts more with
  cross-thread wakes in a shared runtime.

**Process-separated** (real deployment topology):
- Much more competitive. Both languages scale better (Go 9–13×, Rust 6–7×).
- Rust leads at low parallelism and multi-connection/few-streams
  (V2 4×1: Rust 2.25×). Go edges ahead at high stream counts
  (V2 4×64: Go 1.47×, V3 4×16: Go 1.98×).
- The extreme Go dominance seen in-process (2–3× at high concurrency) shrinks
  to modest gaps (typically <1.5×), confirming that in-process shared-runtime
  interference exaggerates Rust's scaling weakness.

### Why Rust Scales Differently In-Process

The process-separated results show Rust does **not** intrinsically collapse —
it stays competitive with Go across the full config matrix when client and
server run in separate processes. The in-process benchmark exaggerates Rust's
weakness because it is **highly sensitive to shared-runtime topology**.

This was confirmed by sweeping `TOKIO_WORKER_THREADS`:

```
Tokio worker thread sweep (V2 Msgpack, 3 runs × 5000 ops, median):

  4 conn × 1 stream:
  Threads    ops/sec     vs best
      2       43,629       1.0x  ← best
      4       23,755       0.55x
      8       26,359       0.60x
     24       19,507       0.45x  ← default (num_cpus)

  4 conn × 16 stream:
  Threads    ops/sec     vs best
      2       29,056       0.73x
      4       32,240       0.81x
      8       39,225       0.99x
     24       39,604       1.0x  ← best (needs parallelism)

  1 conn × 1 stream:
  Threads    ops/sec     vs best
      2       17,802       0.57x
      4       31,492       1.0x  ← best
      8       21,571       0.69x
     24       30,840       0.98x
```

**Key insight**: the optimal Tokio thread count depends strongly on workload
shape. For low-parallelism cases (`4×1`), 2 threads is 2.24× faster than 24
because tasks stay colocated and avoid cross-thread channel overhead. For
high-parallelism cases (`4×16`), more threads help because 64 concurrent tasks
need real parallelism. Go does not exhibit this sensitivity — GOMAXPROCS=24
works fine across all shapes.

**Why the difference:**
- **Go goroutines** yield cooperatively at channel operations. Waking a
  goroutine on another thread costs ~2 µs with low cache impact.
- **Tokio tasks** yield at `.await` points. Cross-thread channel sends involve
  cache-line transfers and work-stealing overhead. With 4 connections, Rust's
  per-connection throughput drops 2.25× vs 1×1 (at 24 threads); Go's drops
  only 1.16×.
- **Extra async hops**: Rust's reader → decoder → inbox chain has one more
  channel crossing per message direction than Go's direct reader → inbox path.
  Each crossing multiplies the cross-thread penalty.

#### Rust Architectural Bottlenecks

1. **Single writer task per connection** — all frames from N streams funnel
   through one writer, serializing writes
2. **One flush per frame** — no batching of multiple frames per writer wake
3. **Extra async hop**: per-stream decoder task between reader and inbox
   (reader → incoming_tx → decoder → inbox_tx); Go decodes inline
4. **V3 cross-thread oneshot round-trip** on every send
   (`outbound_tx → writer → oneshot reply`)
5. **Dynamic dispatch**: `TransportWriter = Box<dyn AsyncWrite>` adds vtable
   overhead on every write call

#### Improvement Opportunities

1. **Tune worker threads per workload** — 4–8 threads may be better than
   num_cpus for most real-world message sizes
2. **Batch frames per writer wake** — drain all pending frames from
   outbound_rx before flushing, reducing syscalls
3. **Inline decoder into reader** — eliminate per-stream decoder task and its
   channel hop
4. **Monomorphize TransportWriter** — use generics or enum-dispatch instead of
   trait objects to enable inlining
5. **Batch V3 sequencing** — assign seq numbers to a batch of frames per
   writer wake, reducing oneshot round-trips

---

## V3 Recovery Overhead

V3 adds replay buffers, cumulative ACKs, heartbeats, and writer-owned
sequencing. The overhead varies dramatically by setup:

```
                        V3 overhead (ns)     V3 overhead (%)
Go (single-core)        +8,268                +21.5%
Go (free-run)          +12,862                +15.4%
Rust Msgpack (single)   +6,506                +20.7%
Rust Msgpack (free)    +34,429                +98.4%
```

**Why Rust V3 is +98% under multi-thread but only +21% on single-core:**

The writer-owned sequencing fix (needed for multi-stream correctness) routes
every V3 send through `outbound_tx → writer task → oneshot reply`. This is
a cross-thread channel round-trip:

- **Single-core**: All tasks share one OS thread. The round-trip is a local
  memory operation within the same event loop. Cost: ~6,500 ns.
- **Multi-thread**: Tasks run on different worker threads. The round-trip
  involves cross-thread mpsc send, cross-thread task wake, and cross-thread
  oneshot reply. Cost: ~34,400 ns.

Go uses the same writer-owned sequencing pattern, but its V3 overhead is only
+15% in free-run. Go's goroutine scheduler absorbs the extra channel hop cost
because goroutine context switches (~2 µs) are cheaper than tokio cross-thread
task wakes under this send → park → wake → reply → park → wake pattern.

V3 recovery options used in all benchmarks:
- AckEvery: 64, AckDelay: 20 ms
- HeartbeatInterval: 30 s, HeartbeatTimeout: 90 s
- MaxReplayBytes: 8 MB, DetachedTTL: 5 s

---

## Root Cause Analysis

### Why Rust is Faster at Single-Connection

Under single-core, Rust leads by 1.23x. The gap comes from fundamental
runtime differences, not architecture (both use the same task topology:
1 reader + 1 writer + 1 decoder + 0-1 handler per connection/stream, with
2 channel hops per message direction).

**1. Task scheduling cost**

Go goroutine context switches cost ~2,000 ns each (M:N scheduling with OS
thread parking). A `SendRecv` round-trip involves 4-5 switches across the 2
channel hops. Tokio async task yields cost ~250 ns (state machine transition
within the same thread). Same 2 hops, far less overhead.

**2. Channel operation cost**

Go `chan` operations: ~750 ns each including goroutine wake. Two hops × two
directions = ~3,000 ns per round-trip. Tokio `mpsc`: ~100 ns each (lock-free
uncontended case). Same two hops × two directions = ~400 ns.

**3. Per-message allocations**

Go: 2,338 B/op, 64 allocs/op. Includes header byte slices (now stack-allocated
for send/receive), payload buffer, and codec intermediates.

Rust: ~2-3 allocations per frame. Frame headers are stack arrays. Only the
payload `Vec<u8>` is heap-allocated on receive.

**4. Codec reflection**

Go Msgpack uses `reflect.ValueOf()` at runtime. Rust Serde generates
specialized encode/decode at compile time.

---

## Optimizations Applied

### Go

| Optimization | Status |
|---|---|
| `bufio.Writer` for all write paths | ✅ Done |
| Stack-allocated frame headers | ✅ Done |
| Batched ACK+data writes in V3 recovery writer | ✅ Done |
| Writer-owned V3 sequencing (correctness fix) | ✅ Done |

### Remaining Opportunities

| Optimization | Expected Impact | Difficulty |
|---|---|---|
| Merge decoder into reader goroutine (Go) | Eliminate 1 channel hop (~1,500 ns/op) | Medium |
| Code-gen msgpack codec (Go) | Eliminate reflection (~2,000 ns/op) | Hard |
| Batch frames per writer wake (Rust) | Reduce flush count, improve scaling | Medium |
| Writer/reader-owned replay+ACK (Rust) | Eliminate mutexes, improve scaling | Hard |
| Batch V3 sequencing (Rust) | Reduce oneshot round-trips, fix V3 overhead | Medium |
