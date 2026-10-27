# Rust AdaptiveMsg Task Architecture

## Task Spawns

All `tokio::spawn` calls in the core runtime:

| #   | Task             | Spawned by                                     | Scope          | Input Channels                 | Purpose                                     |
| --- | ---------------- | ---------------------------------------------- | -------------- | ------------------------------ | ------------------------------------------- |
| A   | Decoder          | `make_stream()`, connection.rs                 | Per-stream     | `incoming_rx` (Vec\<u8\>)      | Decode envelope, route to handler or inbox  |
| B   | Handler          | `spawn_handler_task()`, connection.rs          | Per-stream     | `handler_rx` (HandlerJob)      | Execute registered handlers, send replies   |
| C   | Plain Writer     | `spawn_plain_writer()`, connection.rs          | Per-connection | `outbound_rx`, `writer_cmd_rx` | Write frames to TCP                         |
| D   | Plain Reader     | `spawn_plain_reader()`, connection.rs          | Per-transport  | TCP socket                     | Read frames, send raw bytes to decoder      |
| E   | Recovery Writer  | `spawn_recovery_writer()`, recovery\_runtime.rs | Per-connection | `outbound_rx`, `writer_cmd_rx` | Write frames with seq tracking, ACKs, pings |
| F   | Recovery Reader  | `spawn_recovery_reader()`, recovery\_runtime.rs | Per-transport  | TCP socket                     | Read frames with heartbeat timeout          |
| G   | Server Expiry    | `spawn_server_expiry()`, recovery\_runtime.rs  | Per-detach     | Timer                          | Close connection after detached\_ttl        |
| H   | Client Reconnect | `spawn_reconnect_loop()`, recovery\_runtime.rs | Per-client     | Timer                          | Reconnect with exponential backoff          |
| I   | Server Accept    | `serve_with_accept()`, server.rs               | Per-connection | TCP socket                     | Handshake and connection setup              |
| J   | User Task        | `new_task()`, context.rs                       | Per-stream     | User closure                   | User-spawned background task                |

## Channel Definitions

Created in `make_stream()`. `queue_size` is 1024 unless
`with_stream_queue_size` set it on the client or server:

```rust
// Hop 1: reader → decoder (raw bytes)
let (incoming_tx, incoming_rx) = mpsc::channel::<Vec<u8>>(queue_size);

// Hop 2: decoder → caller (decoded messages)
let (inbox_tx, inbox_rx) = mpsc::channel::<RawMessage>(queue_size);

// Handler path (optional, if handlers registered)
let (handler_tx, handler_rx) = mpsc::channel::<HandlerJob>(queue_size);
```

Created in `new_unstarted()`:

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

| Task        | Count          |
| ----------- | -------------- |
| Writer (C)  | 1              |
| Reader (D)  | 1              |
| Decoder (A) | 1 per stream   |
| Handler (B) | 0-1 per stream |
| **Total**   | **3-4**        |

### Per Connection (V3, 1 stream, with handlers)

| Task                        | Count          |
| --------------------------- | -------------- |
| Recovery Writer (E)         | 1              |
| Recovery Reader (F)         | 1              |
| Decoder (A)                 | 1 per stream   |
| Handler (B)                 | 0-1 per stream |
| Reconnect (H) or Expiry (G) | 0-1            |
| **Total**                   | **3-5**        |

## Key Design Properties

- **Decoder is per-stream, spawned on-demand** in `make_stream()` — not
  pre-allocated or pooled.
- **2 channel hops** per message: reader → decoder, decoder → inbox.
- **Writer is shared** across all streams via `outbound_tx`.
- **All tasks are lightweight async** — no thread pools.
- **Handler task is optional** — only spawned if `registry.has_handlers()`.
- **Recovery preserves the channel structure** — same 2 hops for data, extra
  reader/writer for ACK/seq tracking.
