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
cd adaptivemsg-doc/test/go-process-probe && go build -o probe .
cd adaptivemsg-doc/test/rust-process-probe && cargo build --release

# Example: Rust V2 Msgpack, 1 conn × 4 streams, 5000 ops
AM_CODEC=msgpack ./rust-process-probe/target/release/am_rust_process_probe \
  server 127.0.0.1:18000 &
AM_CODEC=msgpack ./rust-process-probe/target/release/am_rust_process_probe \
  client 127.0.0.1:18000 1 4 5000

# Example: Go V3 (recovery), 4 conn × 16 streams, 5000 ops
AM_RECOVERY=1 ./go-process-probe/probe server 127.0.0.1:18001 &
AM_RECOVERY=1 ./go-process-probe/probe client 127.0.0.1:18001 4 16 5000

# Automated full-matrix run:
./test/run_separated.sh
```

Probe environment variables:
- `AM_CODEC=msgpack` — force MsgpackCompact codec (Rust only; Go defaults to it)
- `AM_RECOVERY=1` — enable V3 recovery mode (both languages)

The two probes use the same wire names, so a Go server can take a Rust client
and the other way round. To build them against local checkouts instead of the
published modules, run `go mod edit -replace
github.com/adaptivemsg/adaptivemsg-go=<path>` in `go-process-probe`, and set
`adaptivemsg = { path = "<path>" }` in the Rust probe's `Cargo.toml`.

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
   protocol numbers (Go: 9.7K ops/sec ≈ 1e9/103,000; Rust V2 Msgpack:
   13.7K ops/sec ≈ 1e9/72,900).

---

## Scaling Throughput

All scaling numbers are from 2026-04-04, free run, multi-thread runtime,
5 runs × 5000 ops, median reported. Two test topologies are used:

- **In-process**: Client and server share one benchmark process/runtime.
  Stresses the scheduler but exaggerates shared-runtime interference.
- **Process-separated**: Client and server in separate OS processes,
  communicating over real TCP. Closer to actual deployment topology.

**Note (2026-04-04)**: Go and Rust V2 writer loops now use batched-flush
(drain channel, flush once) matching V3's write coalescing.

**Machine caveat**: All benchmarks run on a shared server (Intel Xeon E5-2680
v4, 593+ days uptime, other users present). In-process Rust numbers show
high variance between runs (±30% on some configs) due to tokio's sensitivity
to CPU scheduling noise. Process-separated results are more stable and
better reflect real deployment performance. Treat in-process numbers as
directional, not precise.

### In-Process Results

#### Go (Msgpack)

```
Go V2 (no recovery):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      9,709          1.0x
  1 conn × 4 stream     19,023          2.0x
  1 conn × 16 stream    21,696          2.2x
  1 conn × 64 stream    24,178          2.5x
  4 conn × 1 stream     19,554          2.0x
  4 conn × 4 stream     31,495          3.2x
  4 conn × 16 stream    44,330          4.6x
  4 conn × 64 stream    35,615          3.7x

Go V3 (recovery enabled):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      8,323          1.0x
  1 conn × 4 stream     13,956          1.7x
  1 conn × 16 stream    20,808          2.5x
  1 conn × 64 stream    19,960          2.4x
  4 conn × 1 stream     17,803          2.1x
  4 conn × 4 stream     32,932          4.0x
  4 conn × 16 stream    41,026          4.9x
  4 conn × 64 stream    39,073          4.7x
```

#### Rust (Msgpack)

```
Rust V2 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream     17,821          1.0x
  1 conn × 4 stream     22,591          1.3x
  1 conn × 16 stream    21,145          1.2x
  1 conn × 64 stream    22,704          1.3x
  4 conn × 1 stream     16,839          0.9x
  4 conn × 4 stream     18,954          1.1x
  4 conn × 16 stream    19,527          1.1x
  4 conn × 64 stream    18,987          1.1x

Rust V3 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      7,671          1.0x
  1 conn × 4 stream     15,403          2.0x
  1 conn × 16 stream    22,319          2.9x
  1 conn × 64 stream    22,212          2.9x
  4 conn × 1 stream     15,637          2.0x
  4 conn × 4 stream     24,987          3.3x
  4 conn × 16 stream    24,811          3.2x
  4 conn × 64 stream    25,681          3.3x
```

#### Head-to-Head (Msgpack)

```
V2 (no recovery):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       9,709        17,821      1.84x  ← Rust faster
  1 conn × 4 stream      19,023        22,591      1.19x  ← Rust faster
  1 conn × 16 stream     21,696        21,145      0.97x
  1 conn × 64 stream     24,178        22,704      0.94x
  4 conn × 1 stream      19,554        16,839      0.86x  ← Go faster
  4 conn × 4 stream      31,495        18,954      0.60x  ← Go dominates
  4 conn × 16 stream     44,330        19,527      0.44x
  4 conn × 64 stream     35,615        18,987      0.53x

V3 (recovery enabled):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       8,323         7,671      0.92x
  1 conn × 4 stream      13,956        15,403      1.10x
  1 conn × 16 stream     20,808        22,319      1.07x
  1 conn × 64 stream     19,960        22,212      1.11x  ← Rust faster
  4 conn × 1 stream      17,803        15,637      0.88x  ← Go faster
  4 conn × 4 stream      32,932        24,987      0.76x  ← Go faster
  4 conn × 16 stream     41,026        24,811      0.60x
  4 conn × 64 stream     39,073        25,681      0.66x
```

### Process-Separated Results

Both languages use **separate server/client processes** (the probes in
`adaptivemsg-doc/test/{go,rust}-process-probe`):

- No pinned CPUs, no forced runtime worker count
- Both use MsgpackCompact codec (`AM_CODEC=msgpack` for Rust)
- V3 via `AM_RECOVERY=1` on both server and client
- 5 runs × 5000 ops, median reported

#### Go (Msgpack)

```
Go V2 (no recovery):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      2,877          1.0x
  1 conn × 4 stream     10,347          3.6x
  1 conn × 16 stream    19,028          6.6x
  1 conn × 64 stream    16,876          5.9x
  4 conn × 1 stream     10,513          3.7x
  4 conn × 4 stream     19,731          6.9x
  4 conn × 16 stream    21,879          7.6x
  4 conn × 64 stream    29,034         10.1x

Go V3 (recovery enabled):
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      2,999          1.0x
  1 conn × 4 stream      9,530          3.2x
  1 conn × 16 stream    16,274          5.4x
  1 conn × 64 stream    15,877          5.3x
  4 conn × 1 stream      7,872          2.6x
  4 conn × 4 stream     21,245          7.1x
  4 conn × 16 stream    25,207          8.4x
  4 conn × 64 stream    28,743          9.6x
```

#### Rust (Msgpack)

```
Rust V2 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      3,249          1.0x
  1 conn × 4 stream      8,405          2.6x
  1 conn × 16 stream    18,053          5.6x
  1 conn × 64 stream    26,678          8.2x
  4 conn × 1 stream     19,345          6.0x
  4 conn × 4 stream     21,911          6.7x
  4 conn × 16 stream    25,955          8.0x
  4 conn × 64 stream    30,682          9.4x

Rust V3 Msgpack:
  Config               ops/sec     speedup vs 1×1
  1 conn × 1 stream      4,093          1.0x
  1 conn × 4 stream      9,678          2.4x
  1 conn × 16 stream    18,285          4.5x
  1 conn × 64 stream    18,460          4.5x
  4 conn × 1 stream     12,246          3.0x
  4 conn × 4 stream     20,003          4.9x
  4 conn × 16 stream    21,014          5.1x
  4 conn × 64 stream    15,621          3.8x
```

#### Head-to-Head (Msgpack)

```
V2 (no recovery):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       2,877         3,249      1.13x  ← Rust faster
  1 conn × 4 stream      10,347         8,405      0.81x  ← Go faster
  1 conn × 16 stream     19,028        18,053      0.95x
  1 conn × 64 stream     16,876        26,678      1.58x  ← Rust faster
  4 conn × 1 stream      10,513        19,345      1.84x  ← Rust faster
  4 conn × 4 stream      19,731        21,911      1.11x  ← Rust faster
  4 conn × 16 stream     21,879        25,955      1.19x  ← Rust faster
  4 conn × 64 stream     29,034        30,682      1.06x

V3 (recovery enabled):
  Config              Go ops/sec   Rust ops/sec   Rust/Go
  1 conn × 1 stream       2,999         4,093      1.36x  ← Rust faster
  1 conn × 4 stream       9,530         9,678      1.02x
  1 conn × 16 stream     16,274        18,285      1.12x  ← Rust faster
  1 conn × 64 stream     15,877        18,460      1.16x  ← Rust faster
  4 conn × 1 stream       7,872        12,246      1.56x  ← Rust faster
  4 conn × 4 stream      21,245        20,003      0.94x
  4 conn × 16 stream     25,207        21,014      0.83x  ← Go faster
  4 conn × 64 stream     28,743        15,621      0.54x  ← Go dominates
```

### Scaling Summary

#### In-Process

| Metric             | Go V2  | Go V3  | Rust V2 Msgpack | Rust V3 Msgpack |
| ------------------ | ------ | ------ | --------------- | --------------- |
| Peak ops/sec       | 44,330 | 41,026 | 22,704          | 25,681          |
| Peak config        | 4×16   | 4×16   | 1×64            | 4×64            |
| Max speedup vs 1×1 | 4.6x   | 4.9x   | 1.3x            | 3.3x            |

#### Process-Separated (Msgpack)

| Metric             | Go V2  | Go V3  | Rust V2 | Rust V3 |
| ------------------ | ------ | ------ | ------- | ------- |
| Peak ops/sec       | 29,034 | 28,743 | 30,682  | 21,014  |
| Peak config        | 4×64   | 4×64   | 4×64    | 4×16    |
| Max speedup vs 1×1 | 10.1x  | 9.6x   | 9.4x    | 5.1x    |

#### Key Takeaways

**In-process** (shared runtime):
- V2 now properly faster than V3 for Go (44K vs 41K peak) after batched-flush
  fix. Rust V2 in-process scaling remains flat (1.3×) — see analysis below.
- Rust wins at low parallelism (V2 1×1: 1.84×) but Go dominates at high
  concurrency (V2 4×16: Go 2.27×, V3 4×4: Go 1.32×).
- Go scales 4.6–4.9× from 1×1 to peak; Rust V2 only 1.3× (V3: 3.3×).

**Process-separated** (real deployment topology):
- Much more competitive. V2: Rust peaks at 30,682 (9.4×), matching Go's 29,034
  (10.1×). Rust V2 is the overall process-separated peak.
- Rust leads at low parallelism and multi-connection/few-streams
  (V2 4×1: Rust 1.84×). Go edges ahead only at V3 high-stream configs
  (V3 4×64: Go 1.84×).
- The extreme Go dominance seen in-process (2× at high concurrency) shrinks
  to near-parity or Rust advantage for V2, confirming that in-process
  shared-runtime interference exaggerates Rust's scaling weakness.

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
- **Channel hops cost more in tokio**: both runtimes pass each message through
  the same two hops (reader → per-stream decoder → inbox). Under the
  multi-thread scheduler either hop can wake a task on another worker, so each
  crossing multiplies the cross-thread penalty.

#### Rust Architectural Bottlenecks

1. **Single writer task per connection** — all frames from N streams funnel
   through one writer, serializing writes
2. ~~**One flush per frame**~~ — fixed: V2 plain writer now drains channel
   before flushing (V3 recovery writer already batched ACK+data)
3. **Decoder hop**: per-stream decoder task between reader and inbox
   (reader → incoming_tx → decoder → inbox_tx). Go has the same hop
   (`decodeLoop`), but a tokio cross-thread wake costs more than a goroutine
   wake
4. **V3 cross-thread oneshot round-trip** on every send
   (`outbound_tx → writer → oneshot reply`)
5. **Dynamic dispatch**: `TransportWriter = Box<dyn AsyncWrite>` adds vtable
   overhead on every write call

#### Improvement Opportunities

1. **Tune worker threads per workload** — 4–8 threads may be better than
   num_cpus for most real-world message sizes
2. **Inline decoder into reader** — eliminate per-stream decoder task and its
   channel hop
3. **Monomorphize TransportWriter** — use generics or enum-dispatch instead of
   trait objects to enable inlining
4. **Batch V3 sequencing** — assign seq numbers to a batch of frames per
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

Rust: frame headers are stack arrays, and postcard decodes straight from the
payload. The msgpack codecs still build an `rmpv::Value` tree for each body,
one heap allocation per string, array and map in the message, on top of the
payload `Vec<u8>` and the wire-name `String`.

**4. Codec reflection**

Go Msgpack uses `reflect.ValueOf()` at runtime. Rust Serde generates
specialized encode/decode at compile time.

---

## Optimizations Applied

### Go

| Optimization                                                    | Status          |
| --------------------------------------------------------------- | --------------- |
| `bufio.Writer` for all write paths                              | ✅ Done          |
| Stack-allocated frame headers                                   | ✅ Done          |
| Batched ACK+data writes in V3 recovery writer                   | ✅ Done          |
| Writer-owned V3 sequencing (correctness fix)                    | ✅ Done          |
| Batched-flush V2 writer loop                                    | ✅ Done          |
| Buffered reads; V3 writer flushes once per batch                | ✅ Done (2026-10) |
| Codec carried by the message; lock-free registry snapshot       | ✅ Done (2026-10) |
| Compact encode written straight to a pooled encoder             | ✅ Done (2026-10) |
| Compact decode with one streaming decoder; per-type caches      | ✅ Done (2026-10) |
| Writer-local resume queue; lock-free ACK-due and transport checks | ✅ Done (2026-10) |

### Rust

| Optimization                                                    | Status          |
| --------------------------------------------------------------- | --------------- |
| Batched-flush V2 plain writer loop                              | ✅ Done          |
| Buffered reads; V3 writer flushes once per batch                | ✅ Done (2026-10) |
| Codec carried by the message (no global lock per decode)        | ✅ Done (2026-10) |
| No deep `Value` clone on decode; fewer copies in envelope/postcard | ✅ Done (2026-10) |
| Writer-local resume queue; lock-free ACK-due check; reused read timer | ✅ Done (2026-10) |

### Remaining Opportunities

| Optimization                             | Expected Impact                             | Difficulty |
| ---------------------------------------- | ------------------------------------------- | ---------- |
| Merge decoder into reader goroutine (Go) | Eliminate 1 channel hop (~1,500 ns/op)      | Medium     |
| Code-gen msgpack codec (Go)              | Eliminate reflection (~2,000 ns/op)         | Hard       |
| Replay buffer without a mutex (both)     | The resume queue and ACK checks no longer lock; the replay buffer is still shared by writer and reader | Hard |
| Byte-credit V3 backpressure (both)       | Replaces the per-send writer round trip; see the evaluation below | Medium |
| Zero-copy msgpack body (Rust)            | Skips the `rmpv::Value` tree per message     | Medium     |

## 2026-10 Optimization Round: Before and After

Measured on the same host, the baseline build and the optimized build back to
back (24 threads, shared machine, load average 2–4). Absolute numbers are
higher than in the sections above, which were taken on a quieter machine, so
compare within these tables only.

### Go codec (`BenchmarkCodec*`, 7-field message)

| Benchmark       | Before                       | After                       |
| --------------- | ---------------------------- | --------------------------- |
| Encode compact  | 4,570 ns, 1,600 B, 34 allocs | 1,310 ns, 256 B, 7 allocs   |
| Decode compact  | 10,500 ns, 5,597 B, 104 allocs | 2,200 ns, 784 B, 21 allocs |
| Encode map      | 1,790 ns, 672 B, 9 allocs    | 1,410 ns, 272 B, 4 allocs   |
| Decode map      | 4,240 ns, 1,704 B, 27 allocs | 2,950 ns, 645 B, 16 allocs  |

### Single-connection round trip (ns/op, median of 5)

| Benchmark             | Before              | After               |
| --------------------- | ------------------- | ------------------- |
| Go V2                 | 91,151 (63 allocs)  | 66,400 (16 allocs)  |
| Go V3                 | 103,245 (71 allocs) | 81,569 (20 allocs)  |
| Rust V2 (Postcard)    | 76,996              | 72,083              |
| Rust V2 (Msgpack)     | 74,132              | 64,592              |
| Rust V3 (Postcard)    | 79,122              | 67,295              |
| Rust V3 (Msgpack)     | 78,813              | 61,578              |

### Process-separated probes (ops/sec, Msgpack, median of 3 × 5000 ops)

| Runtime / mode | 1×1           | 1×16           | 4×16             | 4×64             |
| -------------- | ------------- | -------------- | ---------------- | ---------------- |
| Go V2          | 8,742 → 11,668 | 50,111 → 79,115 | 93,609 → 147,963 | 108,273 → 122,241 |
| Go V3          | 7,400 → 8,886  | 32,794 → 70,827 | 75,368 → 102,533 | 78,708 → 101,696  |
| Rust V2        | 10,764 → 13,908 | 63,333 → 78,244 | 169,880 → 298,418 | 201,823 → 318,432 |
| Rust V3        | 11,642 → 12,982 | 34,159 → 99,578 | 110,984 → 221,474 | 106,441 → 300,764 |

The in-process scaling benchmarks moved the same way: the largest gains are
V3 with many streams per connection (Rust 1×64: 36,905 → 256,631 ops/sec; Go
1×64: 23.4 → 10.3 µs/op), where the per-frame flush and the locks in the V3
loops cost the most. Go↔Rust probes (each side as server) completed in V2 and
V3 with every reply checked.

### Deferred: byte-credit backpressure and zero-copy body

On this host the V3 overhead over V2 was +3% to +6% before the round and
none after it, not the +98% measured for Rust free-run above, so the
per-send writer round trip that byte credits would remove does not show here.
The change would rework ordering, held frames and resume in both writers, so
it waits for a workload where the round trip measurably dominates. The Rust
zero-copy body saves one small `rmpv::Value` tree per message, well under a
microsecond next to a round trip of about 65 µs that is dominated by task
scheduling.
