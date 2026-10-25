# Short-Lived API

A thin convenience layer on top of the existing connection-based API.
`Client.ConnectOnce` (Go) and `Client::connect_once` (Rust) return a
`OnceConn` whose `SendRecv` / `send_recv` makes one request-reply round trip
over a freshly dialled connection that is closed right after the reply
arrives.

## Motivation

Many use-cases only need a single request-reply exchange (health checks, CLI
tools, one-shot RPC). The connection-based API requires the caller to manage
Client, Connection, and Close explicitly. The short-lived API collapses that
into a single chain: configure `Client` → `ConnectOnce(addr)` →
`SendRecv[T](req)`.

## Go API

### Minimal form

```go
reply, err := am.NewClient().
    WithRecvTimeout(5*time.Second).
    ConnectOnce("tcp://127.0.0.1:8080").
    SendRecv[*EchoReply](&EchoReq{Text: "hello"})
```

### With options

```go
reply, err := am.NewClient().
    WithConnectTimeout(10*time.Second).
    WithRecvTimeout(5*time.Second).
    WithCodecs(am.CodecMsgpackCompact).
    ConnectOnce("tcp://127.0.0.1:8080").
    SendRecv[*EchoReply](&EchoReq{Text: "hello"})
```

### Methods

```go
func (c *Client) ConnectOnce(addr string) *OnceConn {
    return &OnceConn{client: c, addr: addr} // does not dial
}

func (o *OnceConn) SendRecv[T any](msg Message) (T, error) {
    var zero T
    if o.used.Swap(true) {
        return zero, ErrClosed{} // strictly single use
    }
    conn, err := o.client.connect(o.addr, false) // never uses recovery
    if err != nil {
        return zero, err
    }
    defer conn.markClosed() // closes at once, see "Connection closed after reply"
    return conn.SendRecv[T](msg)
}
```

Go 1.27 generic methods let `OnceConn` offer the same typed `SendRecv[T]` as
`Connection` and `Stream`.

## Rust API

### Minimal form

```rust
let reply: EchoReply = Client::new()
    .with_recv_timeout(Duration::from_secs(5))
    .connect_once("tcp://127.0.0.1:8080")
    .send_recv(EchoReq { text: "hello".into() })
    .await?;
```

### With options

```rust
let reply: EchoReply = Client::new()
    .with_connect_timeout(Duration::from_secs(10))
    .with_recv_timeout(Duration::from_secs(5))
    .with_codecs(&[CodecMsgpackCompact])
    .connect_once("tcp://127.0.0.1:8080")
    .send_recv(EchoReq { text: "hello".into() })
    .await?;
```

### Methods

```rust
impl Client {
    pub fn connect_once<'a>(&'a self, addr: &'a str) -> OnceConn<'a> {
        OnceConn { client: self, addr } // does not dial
    }
}

impl OnceConn<'_> {
    pub async fn send_recv<Rep: MessageDecode + 'static>(
        self, // consumed: strictly single use
        msg: impl Message,
    ) -> Result<Rep, Error> {
        let conn = self.client.connect_with(self.addr, false).await?; // never uses recovery
        let reply = conn.send_recv(msg).await;
        conn.close_now(); // see "Connection closed after reply"
        reply
    }
}
```

## Design decisions

| Decision | Rationale |
|----------|-----------|
| Go: `ConnectOnce(addr).SendRecv[T](req)` | `ConnectOnce` pairs with `Connect`, and `SendRecv` is the same call as on `Connection` and `Stream`. `ConnectOnce` does not dial and cannot fail; every error comes from `SendRecv`. |
| Rust: `connect_once(addr).send_recv(req)` | Mirrors Go. The request is `impl Message`, so `send_recv::<Rep>(req)` names only the reply type. |
| Strictly single use | A `OnceConn` is one exchange, not a reusable address handle. Go: a second `SendRecv` returns `ErrClosed`. Rust: `send_recv` takes `self`, so a second call does not compile. |
| Recovery never used | Recovery adds an attach round trip to every connection and does little for a single exchange. Retry the whole exchange instead. |
| Two timeouts, no defaults | `WithConnectTimeout` limits connecting, including retries; `WithRecvTimeout` limits waiting for the reply. Examples always set `WithRecvTimeout`. |
| Connect timeout implies retry | Until `WithConnectTimeout` expires, failed dials are retried, so a client can start before its server. Without it the client dials once. Handshake errors are not retried, and the request is never sent twice. |
| Uses default stream (stream 0) | Only one message exchange, no need for extra streams. |
| Connection closed after reply | The connection closes at once when `SendRecv` returns, even on error. It skips the graceful close, which waits for the peer to close its side: once the reply arrived nothing is left to deliver, and after a timeout the peer may not be responding. |

## What this does NOT do

- **Connection pooling** — each `OnceConn` opens and closes a fresh TCP
  connection. If you need repeated calls to the same server, use the normal
  `Client.Connect` API.
- **Streaming** — this is strictly one request, one reply.

## Error semantics

Errors returned are the same as the underlying `Client.Connect` and
`SendRecv` / `send_recv` calls. The caller may see:

- **Connect failure** — server unreachable, DNS resolution error, connection
  refused.
- **Handshake failure** — no common codec, version mismatch.
- **Connect timeout** — connecting, including retries, exceeded
  `WithConnectTimeout` (`ErrConnectTimeout` / `Error::ConnectTimeout`).
- **Recv timeout** — the reply did not arrive within `WithRecvTimeout`
  (`stream.recv_timeout`).
- **Decode error** — reply payload cannot be deserialized into the expected type
  (`stream.decode`).
- **Protocol error** — malformed frame or unexpected stream state
  (`stream.protocol`).
- **Reuse** (Go) — a second `SendRecv` on the same `OnceConn` returns
  `ErrClosed` without dialing.

Failure codes follow the shared vocabulary defined in the main
[README](README.md#shared-failure-code-vocabulary).

## Cost model

Each one-shot exchange — `ConnectOnce(addr).SendRecv[T](req)` in Go and
`connect_once(addr).send_recv(req)` in Rust — pays:

1. TCP connect (SYN/SYN-ACK/ACK)
2. Handshake (codec negotiation, 12 + N bytes + 12 bytes)
3. Goroutine/task spawn for reader, writer, decoder (3 lightweight tasks)
4. One framed request + one framed reply
5. TCP close (FIN/ACK)

For infrequent one-shot calls (CLI tools, health probes, config fetch) this
overhead is negligible. For high-throughput use-cases, use a persistent
connection.

### Comparison with RESTful HTTP

| Step | adaptivemsg `ConnectOnce` | HTTP/1.1 REST |
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
