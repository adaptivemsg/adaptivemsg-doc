- [adaptivemsg proposal (doc-only)](#adaptivemsg-proposal-doc-only)
  - [Goals](#goals)
  - [Repo split](#repo-split)
  - [Rust implementations](#rust-implementations)
  - [Go implementations](#go-implementations)
  - [Examples](#examples)
  - [Language parity](#language-parity)
  - [Wire protocol](#wire-protocol)
    - [Handshake (per connection, v2)](#handshake-per-connection-v2)
    - [Frame header (all modes)](#frame-header-all-modes)
    - [Optional per-frame meta (internal-only)](#optional-per-frame-meta-internal-only)
  - [Codecs (pluggable)](#codecs-pluggable)
    - [Codec selection \& shared IDs](#codec-selection--shared-ids)
    - [Shared codec candidates](#shared-codec-candidates)
    - [Go-only performance codecs](#go-only-performance-codecs)
    - [Rust-only performance codecs](#rust-only-performance-codecs)
    - [CodecID registry (proposal)](#codecid-registry-proposal)
    - [Compact (array, order-dependent)](#compact-array-order-dependent)
    - [Map (field names, order-independent)](#map-field-names-order-independent)
  - [Message naming rule](#message-naming-rule)
    - [Explicit override](#explicit-override)
  - [Code generation (no IDL)](#code-generation-no-idl)
    - [Example: Go server -\> Rust clients](#example-go-server---rust-clients)
  - [Compatibility rules](#compatibility-rules)
  - [Next steps](#next-steps)

# adaptivemsg proposal (doc-only)

This document describes the clean "from scratch" wire protocol for a shared
Rust/Go runtime message system, plus the repo split. Rust and Go are equal peers
on the wire. No IDL files are required.

## Goals

- One wire protocol for go-go, rust-rust, go-rust, rust-go.
- Client negotiates a codec per connection from a supported list.
- Same message names across languages.
- No external IDL required.

## Repo split

- `adaptivemsg-rust` (crate name stays `adaptivemsg`)
  - Runtime + macros.
- `adaptivemsg-go`
  - Go runtime.
- `amgen-go`
  - Go -> Rust generator (via `go generate`).
- `amgen-rs`
  - Rust -> Go generator (install with `cargo install adaptivemsg-amgen`).
- `adaptivemsg-doc`
  - This proposal.

## Rust implementations

- `adaptivemsg-rust`: runtime + macros (crate name `adaptivemsg`).
- `adaptivemsg-macros`: proc-macro crate for `#[am::message]` and `#[am::message_handler]`.
- `adaptivemsg-amgen` (bin `amgen-rs`): Rust -> Go generator.

## Go implementations

- `adaptivemsg-go`: Go runtime.
- `amgen-go`: Go -> Rust generator (via `go generate`).

## Examples

- `hello-server-go`: Go server using message api in `api/hello` (server build tag).
- `hello-server-rust`: Rust server using message api in `api/hello`.
- `hello-client-go`: Go client using Rust message api.
- `hello-client-rust`: Rust client using Go message api.
- Cross-lang tests pair Go client -> Rust server and Rust client -> Go server.

## Language parity

- Rust and Go are equal peers on the wire.
- The protocol does not assume a "source of truth" language.

## Wire protocol

### Handshake (per connection, v2)

- Client sends: `{protocol_version, codec_list, max_frame, flags}`
- Server replies: `{accept/reject, selected_codec, flags}`
- Codec is fixed per connection for performance.

Handshake is sent before any framed messages. The client hello is a fixed
12-byte header followed by a variable-length codec list.

Proposed layout (all fields big endian):

```
client -> server:
[magic(2) | version(1) | codec_count(1) | flags(1) | reserved(3) | max_frame(u32) | codecs(codec_count)]

server -> client:
[magic(2) | accept(1) | version(1) | codec(1) | flags(1) | reserved(2) | max_frame(u32)]
```

Notes:

- `magic(2)` is a fixed handshake marker (example: `AM`) to reject unknown
  protocols early.
- `version(1)` is the protocol major version (v2 for this layout).
- `codec_count(1)` is the number of codec IDs that follow (max 16).
- `codecs` is an ordered list of `CodecID` values; the server chooses the first
  compatible codec.
- `accept(1)` is 1 for accept, 0 for reject.
- `flags` are capability bits (reserved for future use).
- `max_frame(u32)` in the client hello is the maximum payload size the client
  will accept. (In the current Go runtime, `max_frame == 0` rejects all non-empty frames.)
- `max_frame(u32)` in the server reply is the negotiated maximum payload size
  the server will accept (typically `min(client_max, server_max)`).
- If `accept=0`, the connection should be closed by the server.

### Frame header (all modes)

```
[version(1) | flags(1) | stream_id(u32) | payload_len(u32)]
```

Field details:

- `version(1)`: protocol version. Bump this when the header layout or handshake
  semantics change.
- `flags(1)`: bit flags for per-frame options. Initial bits are reserved; future
  uses could include compression or control frames.
- `stream_id(u32)`: logical stream identifier within a connection. `0` is the
  default stream; other values are additional streams.
- `payload_len(u32)`: payload byte length that follows the header (big endian).
  - If `FLAG_META` is set, this includes `meta_len + meta_bytes + msg_bytes`.
  - Otherwise it is just `msg_bytes`.

Notes:

- All multi-byte fields are big endian.
- `payload_len` must be <= `max_frame` from the handshake.
- The payload bytes are encoded using the selected codec mode for that
  connection (compact or map).
- `flags` are reserved; the current Go runtime ignores them.

### Optional per-frame meta (internal-only)

For tracing, a frame may carry metadata in a sidecar when a flag bit is set.
This does not change the message encoding itself.

Proposed layout when `flags & FLAG_META != 0`:

```
[frame header | meta_len(u16) | meta_bytes | msg_bytes]
```

- `meta_bytes` is MessagePack (a map) for future fields (e.g. trace_id, span_id).
- `msg_bytes` is the normal message payload for the chosen codec.
- This is internal-only; no public API is exposed for meta.
- Meta frames are not implemented in the current Go runtime.

## Codecs (pluggable)

Codecs are identified by a `CodecID` and negotiated during the handshake. The
codec controls the envelope layout and raw body extraction needed for lazy
decode and `PeekWire`.

Initial codecs:
- `1` = Compact (MessagePack array envelope)
- `2` = Map (MessagePack map envelope)

### Codec selection & shared IDs

- The client sends an ordered preference list; the server picks the first codec it also supports (compact-first by default in current runtimes).
- Cross-language interop (go↔rust) requires a shared `CodecID` mapping and matching envelope semantics.
- Each runtime may support extra codecs, but only shared codecs can be negotiated successfully.
- Always include a shared fallback codec to avoid no-common-codec failures.

### Shared codec candidates

Shared codecs must expose a small envelope that includes the wire name so
`PeekWire` and lazy decode remain possible across languages.

- MessagePack (map/compact envelopes) — current baseline.
- CBOR (map/array envelopes) — similar semantics to MessagePack.
- JSON (map envelope) — easy interop, larger/slower.
- Protobuf / FlatBuffers / Cap'n Proto — only if wrapped in an envelope
  carrying the wire name; otherwise you can't do lazy decode.

### Go-only performance codecs

These are suitable for Go↔Go only (not cross-language unless Rust matches them).

- Custom binary + codegen (fastest, no reflection).
- Protobuf with Go codegen + envelope wrapper.
- MessagePack compact with codegen.
- `encoding/gob` (Go-only; usually not fastest).

### Rust-only performance codecs

These are suitable for Rust↔Rust only (not cross-language unless Go matches them).

- Custom binary + codegen (fastest, no reflection).
- Protobuf via `prost` + envelope wrapper.
- `bincode` / `postcard` / `rkyv` (fast Rust-native codecs; still need an envelope for wire name).

### CodecID registry (proposal)

- `0`: reserved (invalid).
- `1–15`: core shared codecs (stable, cross-language).
- `16–31`: shared experimental codecs (cross-language, not stable).
- `32–63`: shared custom codecs (must be documented in adaptivemsg-doc).
- `64–127`: implementation-specific (Go-only/Rust-only).
- `128–255`: reserved for future expansion.

Assigned implementation-specific IDs:
- `64` = Postcard (Rust-only, envelope `{type, data}`).

### Compact (array, order-dependent)

```
[ "msg.name", field1, field2, ... ]
```

### Map (field names, order-independent)

```
{ "type": "msg.name", "data": { "field1": ..., "field2": ... } }
```

Notes:
- Compact is smaller/faster but field order must match across languages.
- Map is larger but more flexible; field order does not matter.
- The client advertises a preferred codec list; the server picks the first common.

## Message naming rule

Default name:

```
<ns>.<module_leaf>.<TypeName>
```

Where:
- `ns` is defined with the source-of-truth message definition (default: `am`).
  The generator does not override it.
- `module_leaf` is the last segment of the module/package path.
  - Rust: last segment of `module_path!()`.
  - Go: last directory of the package path.

Example:

```
am.echo.MessageRequest
```

### Explicit override

Users can provide a stable name override when needed:

- Rust: `#[message(ns = "am", name = "echo.MessageRequest")]`
- Go: `// am:message ns="am" name="echo.MessageRequest"`

If an override is set, it replaces the default.

## Code generation (no IDL)

`amgen-go` and `amgen-rs` generate bindings directly from source (no schema files). They use the
message name rule (or override) and preserve field order for compact mode.

Recommended layout: keep shared message api under `api/<service>/` so the package/module leaf
is stable across languages (example: `api/hello`).

- Go -> Rust: use `amgen-go` to parse `api/<service>/message.go`, emit a `message.rs` file
  alongside it and a repo-root `Cargo.toml` that points its library to that file.
- Rust -> Go: use the Rust `amgen-rs` binary (install with `cargo install adaptivemsg-amgen`).

### Example: Go server -> Rust clients

Repo layout:

```
go-server/
  go.mod
  Cargo.toml        # generated Rust crate (repo root)
  api/
    hello/
      message.go      # Go message structs (source of truth)
      message.rs      # generated Rust output
```

Add a generator hook in `api/hello/message.go`:

```
//go:generate go run <module>/cmd/amgen-go
```

Command:

```
go generate ./...
```

Note: install `amgen-go` or invoke it via `go run <module>/cmd/amgen-go` before running `go generate`.
`amgen-go` refuses to overwrite an existing repo-root `Cargo.toml`.
`amgen-go` reads the `GOFILE` value provided by `go generate`, and writes a sibling `.rs`
file using the same base name.
The generated `Cargo.toml` sets `[lib] path` to the `message.rs` location and includes a placeholder
`adaptivemsg` dependency; fill it with a path or git source.

Rust client usage (recommended):

```
// Cargo.toml
[dependencies]
hello = { path = "../go-server" }
# hello = { git = "https://github.com/you/go-server", package = "hello" }
```

For Rust -> Go generation, invoke `amgen-rs` from your Rust toolchain (or run it directly)
to regenerate Go structs from `api/<service>/message.rs` before consuming them in Go.

## Compatibility rules

- Compact mode is order-sensitive: do not reorder fields across languages.
- Map mode is order-agnostic: safe for reordering, but renames break.
- Changing the message name breaks compatibility in both modes.

## Next steps

1) Add CI for cross-lang tests (Go client -> Rust server, Rust client -> Go server).
2) Expand generator parity and docs for `amgen-go` and `amgen-rs`.
3) Maintain Rust/Go runtime parity (codecs + handshake + error semantics).
