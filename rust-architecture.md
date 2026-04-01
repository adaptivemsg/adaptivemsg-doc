# Rust AdaptiveMsg Task Architecture

## Task Spawns

All `tokio::spawn` calls in the core runtime:

| # | Task | File | Scope | Input Channels | Purpose |
|---|------|------|-------|----------------|---------|
| A | Decoder | connection.rs:455 | Per-stream | `incoming_rx` (Vec\<u8\>) | Decode envelope, route to handler or inbox |
| B | Handler | connection.rs:486 | Per-stream | `handler_rx` (HandlerJob) | Execute registered handlers, send replies |
| C | Plain Writer | connection.rs:590 | Per-connection | `outbound_rx`, `writer_cmd_rx` | Write frames to TCP |
| D | Plain Reader | connection.rs:625 | Per-transport | TCP socket | Read frames, send raw bytes to decoder |
| E | Recovery Writer | recovery\_runtime.rs:62 | Per-connection | `live_rx`, `writer_cmd_rx` | Write frames with seq tracking, ACKs, pings |
| F | Recovery Reader | recovery\_runtime.rs:283 | Per-transport | TCP socket | Read frames with heartbeat timeout |
| G | Server Expiry | recovery\_runtime.rs:375 | Per-detach | Timer | Close connection after detached\_ttl |
| H | Client Reconnect | recovery\_runtime.rs:408 | Per-client | Timer | Reconnect with exponential backoff |
| I | Server Accept | server.rs:158 | Per-connection | TCP socket | Handshake and connection setup |
| J | User Task | context.rs:98 | Per-stream | User closure | User-spawned background task |

## Channel Definitions

Created in `make_stream()` (connection.rs:424-431):

```rust
// Hop 1: reader → decoder (raw bytes)
let (incoming_tx, incoming_rx) = mpsc::channel::<Vec<u8>>(STREAM_QUEUE_SIZE);  // 1024

// Hop 2: decoder → caller (decoded messages)
let (inbox_tx, inbox_rx) = mpsc::channel::<RawMessage>(STREAM_QUEUE_SIZE);     // 1024

// Handler path (optional, if handlers registered)
let (handler_tx, handler_rx) = mpsc::channel::<HandlerJob>(STREAM_QUEUE_SIZE); // 1024
```

Created in `new_unstarted()` (connection.rs:194-195):

```rust
// All streams → writer
let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundFrame>(STREAM_QUEUE_SIZE);

// Writer control (attach/detach transport)
let (writer_cmd_tx, writer_cmd_rx) = mpsc::unbounded_channel();
```

## Data Flow: TCP → Caller

### Non-Recovery (V2)

```
TCP Socket
    ↓
[Plain Reader Task (D)]  — read_frame()
    ↓
stream.incoming_tx.send(payload)          ← Hop 1: Vec<u8>
    ↓
[Decoder Task (A)]  — incoming_rx.recv()
    ↓
connection.decode_envelope(&payload)      → RawMessage
    ↓
dispatch_raw(&stream, raw)
    ├── handler registered → handler_tx   ← Hop 2a: HandlerJob
    │       ↓
    │   [Handler Task (B)]
    │       ↓
    │   handler.handle(msg) → reply
    │
    └── no handler → inbox_tx             ← Hop 2b: RawMessage
            ↓
        Caller's stream.recv::<T>()
```

### Recovery (V3)

Same 2-hop structure. Recovery Reader (F) replaces Plain Reader (D) and adds:
- Heartbeat timeout detection
- Control stream handling (ACKs, pings)
- Sequence number tracking via `recovery.note_received(seq)`

## Task Counts

### Per Connection (V2, 1 stream, with handlers)

| Task | Count |
|------|-------|
| Writer (C) | 1 |
| Reader (D) | 1 |
| Decoder (A) | 1 per stream |
| Handler (B) | 0-1 per stream |
| **Total** | **3-4** |

### Per Connection (V3, 1 stream, with handlers)

| Task | Count |
|------|-------|
| Recovery Writer (E) | 1 |
| Recovery Reader (F) | 1 |
| Decoder (A) | 1 per stream |
| Handler (B) | 0-1 per stream |
| Reconnect (H) or Expiry (G) | 0-1 |
| **Total** | **3-5** |

## Key Design Properties

- **Decoder is per-stream, spawned on-demand** in `make_stream()` — not
  pre-allocated or pooled.
- **2 channel hops** per message: reader → decoder, decoder → inbox.
- **Writer is shared** across all streams via `outbound_tx`.
- **All tasks are lightweight async** — no thread pools.
- **Handler task is optional** — only spawned if `registry.has_handlers()`.
- **Recovery preserves the channel structure** — same 2 hops for data, extra
  reader/writer for ACK/seq tracking.
