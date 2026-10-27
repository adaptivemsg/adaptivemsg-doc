# Go AdaptiveMsg Goroutine Architecture

## Goroutine Spawns

All `go` spawns in the core runtime:

| #   | Goroutine          | File                     | Scope          | Input Channels             | Purpose                                     |
| --- | ------------------ | ------------------------ | -------------- | -------------------------- | ------------------------------------------- |
| A   | decodeLoop         | connection.go            | Per-stream     | `incoming` ([]byte)        | Decode envelope, route to handler or inbox  |
| B   | handlerLoop        | connection.go            | Per-stream     | `handlerCh` (handlerJob)   | Execute registered handlers, send replies   |
| C   | writerLoop         | connection.go            | Per-connection | `outbound` (outboundFrame) | Write frames to TCP                         |
| D   | readerLoop         | connection.go            | Per-connection | TCP socket                 | Read frames, send raw bytes to decoder      |
| E   | recoveryWriterLoop | recovery\_runtime.go     | Per-connection | `outbound`, recovery state | Write frames with seq tracking, ACKs, pings |
| F   | recoveryReaderLoop | recovery\_runtime.go     | Per-connection | TCP socket                 | Read frames with heartbeat timeout          |
| G   | reconnectLoop      | recovery\_runtime.go     | Per-client     | Timer                      | Reconnect with exponential backoff          |
| H   | handleConn         | server.go                | Per-connection | TCP socket                 | Handshake and connection setup              |

The Goroutine column names the function that runs; `start()` and
`startStream()` in connection.go spawn A–F.

## Channel Definitions

Created in `newStreamCtx()` before the stream is published. `queueSize` is
1024 unless `WithStreamQueueSize` set it on the client or server:

```go
// Hop 1: reader → decoder (raw bytes)
incoming := make(chan []byte, queueSize)

// Hop 2: decoder → caller (decoded messages)
inbox := make(chan rawMessage, queueSize)

// Handler path (optional, if handlers registered)
handlerCh := make(chan handlerJob, queueSize)
```

Created in `newPendingConnection()`:

```go
// All streams → writer
outbound: make(chan outboundFrame, streamQueueSize)

// Broadcast close
closeCh: make(chan struct{})
```

## Data Flow: TCP → Caller

### Non-Recovery (V2)

```
TCP Socket
    ↓
[readerLoop (D)]  — readFrameFrom(bufio.Reader)
    ↓
acceptFrame() → stream.incomingQ(payload)   ← Hop 1: []byte
    ↓
[decodeLoop (A)]  — <-stream.incoming
    ↓
newRawMessageFromPayload(codec, payload)    → rawMessage
    ↓
dispatchMessage(stream, raw)
    ├── handler registered → stream.handlerQ() ← Hop 2a: handlerJob
    │       ↓
    │   [handlerLoop (B)]
    │       ↓
    │   handler(streamCtx, msg) → reply
    │
    └── no handler → stream.inboxQ()         ← Hop 2b: rawMessage
            ↓
        Caller's stream.Recv[T]()
```

### Recovery (V3)

Same 2-hop structure. recoveryReaderLoop (F) replaces readerLoop (D) and adds:
- Heartbeat timeout detection
- Control stream handling (ACKs, pings)
- Sequence number tracking via `recoveryState.noteReceived(seq)`

## Task Counts

### Per Connection (V2, 1 stream, with handlers)

| Goroutine       | Count          |
| --------------- | -------------- |
| writerLoop (C)  | 1              |
| readerLoop (D)  | 1              |
| decodeLoop (A)  | 1 per stream   |
| handlerLoop (B) | 0-1 per stream |
| **Total**       | **3-4**        |

### Per Connection (V3, 1 stream, with handlers)

| Goroutine              | Count             |
| ---------------------- | ----------------- |
| recoveryWriterLoop (E) | 1                 |
| recoveryReaderLoop (F) | 1                 |
| decodeLoop (A)         | 1 per stream      |
| handlerLoop (B)        | 0-1 per stream    |
| reconnectLoop (G)      | 0-1 (client only) |
| **Total**              | **3-5**           |

## Key Design Properties

- **Decoder is per-stream, spawned on-demand** in `startStream()` after the
  stream is published — not pre-allocated or pooled.
- **2 channel hops** per message: reader → decoder, decoder → inbox.
- **Writer is shared** across all streams via `outbound` channel.
- **Handler goroutine is optional** — only spawned if the connection snapshot
  contained handlers when the connection was created.
- **Recovery preserves the channel structure** — same 2 hops for data, extra
  reader/writer for ACK/seq tracking.
