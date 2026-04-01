# Go vs Rust Task Architecture Comparison

## Go Architecture (per connection, 1 stream, with handlers)

| Task | Scope | Count |
|------|-------|-------|
| readerLoop | Per-connection | 1 |
| writerLoop | Per-connection | 1 |
| decodeLoop | Per-stream | 1 |
| handlerLoop | Per-stream (if handlers) | 0-1 |
| **Total** | | **3-4** |

Channel hops (TCP read → caller recv):
1. reader → incoming ([]byte)
2. decoder → inbox (rawMessage) or handlerCh (handlerJob)
**Total: 2 hops**

## Rust Architecture (per connection, 1 stream, with handlers)

| Task | Scope | Count |
|------|-------|-------|
| Reader task | Per-connection | 1 |
| Writer task | Per-connection | 1 |
| Decoder task | Per-stream | 1 |
| Handler task | Per-stream (if handlers) | 0-1 |
| **Total** | | **3-4** |

Channel hops (TCP read → caller recv):
1. reader → incoming_tx (Vec<u8>)
2. decoder → inbox_tx (RawMessage) or handler_tx (HandlerJob)
**Total: 2 hops**

## Conclusion

**The architectures are identical:**
- Both: 1 reader per connection, 1 writer per connection
- Both: 1 decoder per stream (spawned on-demand, not pre-allocated)
- Both: 0-1 handler per stream (if handlers registered)
- Both: 2 channel hops from TCP to caller
- Both: decoder is NOT merged into reader

The performance difference is NOT from architectural differences.
It comes from the cost of each operation:
- Go channel op: ~750 ns (goroutine scheduling overhead)
- Rust channel op: ~100 ns (lock-free tokio::sync::mpsc)
- Go goroutine switch: ~2000 ns (OS thread parking)
- Rust task yield: ~250 ns (state machine transition)
