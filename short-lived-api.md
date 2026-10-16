# Short-Lived API

A thin convenience layer on top of the existing connection-based API. One
`SendRecv` round-trip over a freshly dialled connection that is closed
immediately after the reply arrives.

## Motivation

Many use-cases only need a single request-reply exchange (health checks, CLI
tools, one-shot RPC). The current API requires the caller to manage Client,
Connection, and Close explicitly. The short-lived API collapses that into a
single client operation: configure `Client` → `SendRecv[T](addr, req)`.

## Go API

### Minimal form

```go
reply, err := am.NewClient().SendRecv[*EchoReply](
    "tcp://127.0.0.1:8080",
    &EchoReq{Text: "hello"},
)
```

### With options

```go
reply, err := am.NewClient().
    WithTimeout(5*time.Second).
    WithCodecs(am.CodecMsgpackCompact).
    SendRecv[*EchoReply](
        "tcp://127.0.0.1:8080",
        &EchoReq{Text: "hello"},
    )
```

### Method

```go
func (c *Client) SendRecv[T any](addr string, msg Message) (T, error) {
    var zero T
    conn, err := c.Connect(addr)
    if err != nil {
        return zero, err
    }
    defer conn.Close()
    conn.SetRecvTimeout(c.timeout)
    return conn.SendRecv[T](msg)
}
```

Go 1.27 generic methods let `Client` own the typed terminal operation directly.
No package-level helper, sealed `Link` interface, or one-shot builder type is
needed.

## Rust API

### Minimal form

```rust
let reply: EchoReply = Client::new()
    .send_recv(
        "tcp://127.0.0.1:8080",
        EchoReq { text: "hello".into() },
    )
    .await?;
```

### With options

```rust
let reply: EchoReply = Client::new()
    .with_timeout(Duration::from_secs(5))
    .with_codecs(&[CodecMsgpackCompact])
    .send_recv(
        "tcp://127.0.0.1:8080",
        EchoReq { text: "hello".into() },
    )
    .await?;
```

### Method

```rust
impl Client {
    pub async fn send_recv<Req: Message, Rep: MessageDecode + 'static>(
        &self,
        addr: &str,
        req: Req,
    ) -> Result<Rep, Error> {
        let conn = self.connect(addr).await?;
        if let Some(timeout) = self.timeout {
            conn.set_recv_timeout(timeout);
        }
        let reply = conn.send_recv(req).await;
        conn.close();
        reply
    }
}
```

## Design decisions

| Decision | Rationale |
|----------|-----------|
| Go: `Client.SendRecv[T](addr, req)` | Reuses all existing client configuration without another public type or duplicated `WithX` methods. |
| Rust: `Client::send_recv(addr, req)` | Mirrors Go while allowing the reply type to be inferred from context. |
| Recovery disabled by default | `Client.SendRecv` honors explicit recovery configuration, although it rarely benefits a one-shot call. |
| No default timeout | Matches `Client.Connect`; `WithTimeout` applies to both connect and reply wait for this operation. |
| Uses default stream (stream 0) | Only one message exchange, no need for extra streams. |
| Connection closed after reply | `defer conn.Close()` (Go) / `conn.close()` (Rust) guarantees cleanup even on error. |

## What this does NOT do

- **Connection pooling** — each call opens and closes a fresh TCP connection.
  If you need repeated calls to the same server, use the normal
  `Client.Connect` API.
- **Streaming** — this is strictly one request, one reply.

## Error semantics

Errors returned are the same as the underlying `Client.Connect` and
`SendRecv` / `send_recv` calls. The caller may see:

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

Each call — `Client.SendRecv[T](addr, req)` in Go and
`Client::send_recv(addr, req)` in Rust — pays:

1. TCP connect (SYN/SYN-ACK/ACK)
2. Handshake (codec negotiation, 12 + N bytes + 12 bytes)
3. Goroutine/task spawn for reader, writer, decoder (3 lightweight tasks)
4. One framed request + one framed reply
5. TCP close (FIN/ACK)

For infrequent one-shot calls (CLI tools, health probes, config fetch) this
overhead is negligible. For high-throughput use-cases, use a persistent
connection.

### Comparison with RESTful HTTP

| Step | adaptivemsg `Client.SendRecv` | HTTP/1.1 REST |
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
