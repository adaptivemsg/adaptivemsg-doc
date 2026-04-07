# Short-Lived API (V2)

A thin convenience layer on top of the existing connection-based API. One
`SendRecv` round-trip over a freshly dialled connection that is closed
immediately after the reply arrives.

## Motivation

Many use-cases only need a single request-reply exchange (health checks, CLI
tools, one-shot RPC). The current API requires the caller to manage Client,
Connection, and Close explicitly. The short-lived API collapses that into a
builder chain: `Once(addr)` → optional config → `SendRecv(req)`.

## Go API

### Minimal form

```go
reply, err := am.SendRecvAs[*EchoReply](am.Once("tcp://127.0.0.1:8080"), &EchoReq{Text: "hello"})
```

### With options

```go
reply, err := am.SendRecvAs[*EchoReply](
    am.Once("tcp://127.0.0.1:8080").
        WithTimeout(5*time.Second).
        WithCodecs(am.MsgpackCompact),
    &EchoReq{Text: "hello"},
)
```

### Types

```go
// Link is the sealed interface for SendRecvAs targets.
// Connection, Stream, and OnceConn implement Link.
type Link interface {
    isLink()
}

// OnceConn is a builder for a short-lived connection.
// Created by Once(addr) and configured with builder methods.
// Passed to SendRecvAs as a Link target.
type OnceConn struct {
    addr    string
    timeout time.Duration // default 5s
    codecs  []CodecID
}

// Once returns a builder for a short-lived connection to addr.
func Once(addr string) *OnceConn {
    return &OnceConn{addr: addr, timeout: 5 * time.Second}
}

func (o *OnceConn) WithTimeout(d time.Duration) *OnceConn {
    o.timeout = d
    return o
}

func (o *OnceConn) WithCodecs(codecs ...CodecID) *OnceConn {
    o.codecs = append([]CodecID(nil), codecs...)
    return o
}
```

`SendRecvAs[R]` accepts any `Link` — a sealed interface implemented by
`Connection`, `Stream`, and `OnceConn`. When the target is `*OnceConn`, it
dials, sends, receives, and closes. The sealed `isLink()` method prevents
external implementations.

### Implementation sketch

```go
func SendRecvAs[T any](v Link, msg Message) (T, error) {
    if o, ok := v.(*OnceConn); ok {
        client := NewClient().WithTimeout(o.timeout)
        if len(o.codecs) > 0 {
            client = client.WithCodecs(o.codecs...)
        }
        conn, err := client.Connect(o.addr)
        if err != nil {
            var zero T
            return zero, err
        }
        defer conn.Close()
        stream := StreamAs[T](conn)
        stream.SetRecvTimeout(o.timeout)
        return stream.SendRecv(msg)
    }
    stream := StreamAs[T](v)
    return stream.SendRecv(msg)
}
```

## Rust API

### Minimal form

```rust
let reply: EchoReply = am::once("tcp://127.0.0.1:8080")
    .send_recv(EchoReq { text: "hello".into() })
    .await?;
```

### With options

```rust
let reply: EchoReply = am::once("tcp://127.0.0.1:8080")
    .with_timeout(Duration::from_secs(10))
    .with_codecs(&[CodecMsgpackCompact])
    .send_recv(EchoReq { text: "hello".into() })
    .await?;
```

### Types

```rust
/// Builder for a short-lived SendRecv call.
pub struct OnceConn {
    addr: String,
    timeout: Duration,            // default 5s
    codecs: Option<Vec<CodecID>>, // None = client default
}

/// Create a builder that will dial addr, perform one send_recv,
/// and close the connection.
pub fn once(addr: &str) -> OnceConn {
    OnceConn {
        addr: addr.to_string(),
        timeout: Duration::from_secs(5),
        codecs: None,
    }
}

impl OnceConn {
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    pub fn with_codecs(mut self, codecs: &[CodecID]) -> Self {
        self.codecs = Some(codecs.to_vec());
        self
    }

    /// Dial, send one request, receive one reply, close.
    pub async fn send_recv<Req: Message, Rep: MessageDecode + 'static>(
        self,
        req: Req,
    ) -> Result<Rep, Error> {
        let mut client = Client::new().with_timeout(self.timeout);
        if let Some(ref codecs) = self.codecs {
            client = client.with_codecs(codecs);
        }
        let conn = client.connect(&self.addr).await?;
        conn.set_recv_timeout(self.timeout);
        let reply: Rep = conn.send_recv(req).await?;
        conn.close();
        Ok(reply)
    }
}
```

## Design decisions

| Decision | Rationale |
|----------|-----------|
| Builder pattern (both languages) | One API surface; simple case is short, options chain naturally. |
| Go: free function `SendRecvAs(Once(...), req)` | Reuses the existing generic free function; Go cannot have generic methods on structs. |
| Rust: terminal method `once(...).send_recv(req)` | Idiomatic Rust builder; generic methods on structs are allowed. |
| No recovery (V2 only) | A one-shot connection has nothing to resume. Recovery adds overhead with zero benefit here. |
| Default 5 s timeout | Long enough for most one-shot RPCs; callers can override. The timeout covers both connect and recv. |
| Uses default stream (stream 0) | Only one message exchange, no need for extra streams. |
| Connection closed after reply | `defer conn.Close()` (Go) / `conn.close()` (Rust) guarantees cleanup even on error. |
| `send_recv` consumes `self` (Rust) | Enforces one-shot semantics — the builder cannot be reused. |

## What this does NOT do

- **Connection pooling** — each call opens and closes a fresh TCP connection.
  If you need repeated calls to the same server, use the normal
  `Client.Connect` API.
- **Streaming** — this is strictly one request, one reply.
- **Recovery** — V3 recovery is not applied; there is no reconnect.

## Error semantics

Errors returned are the same as the underlying `Client.Connect` and
`SendRecvAs` / `send_recv` calls. The caller may see:

- **Connect failure** — server unreachable, DNS resolution error, connection
  refused.
- **Handshake failure** — no common codec, version mismatch.
- **Timeout** — connect or recv exceeded the configured timeout
  (`stream.recv_timeout`).
- **Decode error** — reply payload cannot be deserialized into the expected type
  (`stream.decode`).
- **Protocol error** — malformed frame or unexpected stream state
  (`stream.protocol`).

Failure codes follow the shared vocabulary defined in the main
[README](README.md#shared-failure-code-vocabulary).

## Cost model

Each call — `SendRecvAs(Once(...), req)` in Go, `once(...).send_recv(req)` in
Rust — pays:

1. TCP connect (SYN/SYN-ACK/ACK)
2. Handshake (codec negotiation, 12 + N bytes + 12 bytes)
3. Goroutine/task spawn for reader, writer, decoder (3 lightweight tasks)
4. One framed request + one framed reply
5. TCP close (FIN/ACK)

For infrequent one-shot calls (CLI tools, health probes, config fetch) this
overhead is negligible. For high-throughput use-cases, use a persistent
connection.

### Comparison with RESTful HTTP

| Step | adaptivemsg `Once` | HTTP/1.1 REST |
|------|-------------------|---------------|
| TCP connect | SYN/SYN-ACK/ACK (1 RTT) | Same |
| TLS handshake | — (not applicable) | 1–2 RTT (TLS 1.2: 2, TLS 1.3: 1) |
| Protocol handshake | 24 + N bytes, 1 RTT | — (no negotiation round-trip) |
| Request overhead | 10-byte frame header + msgpack body | ~200–800 bytes headers + JSON body |
| Reply overhead | 10-byte frame header + msgpack body | ~200–500 bytes headers + JSON body |
| Encoding | MessagePack binary (~30–60% smaller than JSON) | JSON text |
| Decode cost | Direct struct deserialize | JSON parse → reflect → struct |
| TCP close | FIN/ACK | Same |
| Total RTTs | 3 (TCP + handshake + request/reply) | 2–4 (TCP + [TLS] + request/reply) |
| Typical request/reply | ~50–200 bytes wire total | ~500–2000 bytes wire total |

Key takeaways:

- **Without TLS**: adaptivemsg pays one extra RTT (codec handshake) but sends
  5–10× less data per request/reply.
- **With TLS** (common for REST): REST adds 1–2 RTTs for TLS; adaptivemsg does
  not have TLS yet, making it 1–2 RTTs cheaper overall.
- **Encoding**: MessagePack is ~2× faster to encode/decode and ~30–60% smaller
  than JSON.
- **Headers**: REST carries repeated HTTP headers every request; adaptivemsg has
  a fixed 10-byte frame header.
- For a single small request/reply on localhost, the difference is negligible.
  Over WAN or with larger payloads, the binary encoding and minimal framing
  compound.
