# Reaven protocol (v2 over WSS)

Each peer opens **one TLS-protected WebSocket** to the relay, conventionally
`wss://relay.example/ws`. Every protocol frame is exactly one JSON object carried
in one WebSocket **text message**. There is no newline framing: the old
newline-delimited TCP protocol is gone, and a v2 peer that does not perform the
WebSocket upgrade cannot connect. Incoming binary messages are also accepted and
parsed as UTF-8 JSON (the Rust and Android clients send text).

The WebSocket layer handles ping/pong and close frames; `FrameReader` silently
skips ping/pong and treats close as end of session. A single message is limited
to `MAX_FRAME_BYTES` (32 MB); larger messages are rejected and the connection is
dropped.

The relay rewrites `from` on every frame it forwards, so a peer cannot impersonate
another.

Implementation: `src/protocol.rs` (frame types) and `src/transport.rs` (WSS
handshake and message framing). Android port:
`android/app/src/main/java/dev/reaven/control/data/Protocol.kt`.

## Transport and addresses

The relay always terminates TLS; there is no plaintext listener. Certificates can
come from ACME/Let's Encrypt (`--acme-domain`), an existing PEM pair
(`--tls-cert`/`--tls-key`), or `--self-signed` for local testing. The Rust clients
verify the relay certificate and hostname against the built-in WebPKI roots (the
Android app uses the system CA store) unless `--insecure` / **Allow self-signed
certificate** is used.

`--server` accepts either a full URL or the shorthand form:

| Input | Connects to |
|---|---|
| `relay.example.com` | `wss://relay.example.com/ws` |
| `relay.example.com:443` | `wss://relay.example.com:443/ws` |
| `https://relay.example.com` | `wss://relay.example.com/ws` |
| `wss://relay.example.com/custom` | `wss://relay.example.com/custom` |

The relay does not route on the HTTP path, so `/ws` is a convention rather than a
requirement. WebSocket authentication is in-band: the first frame must be
`hello`; HTTP Basic auth is not used.

## Envelope

Every frame carries a `type` discriminator (snake_case):

```json
{"type": "welcome"}
```

## Session

| type | direction | fields |
|---|---|---|
| `hello` | peer → relay | `name`, `password` (nullable), `kind` (`agent`\|`operator`, default `agent`), `info` (optional `PeerInfo`) |
| `welcome` | relay → peer | — |
| `error` | either | `message` |

`PeerInfo` is `{name, kind, os, hostname, version}`. Older clients that omit
`kind`/`info` are still accepted and treated as agents.

The relay replies `error` with `authentication failed` and closes if the password
does not match. If the password is unset on the relay, `null` is accepted.

## Discovery

```json
{"type":"list_peers","id":"<uuid>","from":""}
{"type":"peer_list","id":"<uuid>","to":"operator-…","peers":[{...PeerInfo}]}
```

Only peers of kind `agent` are returned, so operators never appear as machines.

## Request / response

Used for one-shot operations. `op` is tagged by `op`, results by `kind`.

```json
{"type":"request","id":"<uuid>","from":"","target":"desktop",
 "op":{"op":"screenshot","monitor":0,"quality":85,"max_width":1920}}
```

```json
{"type":"response","id":"<uuid>","from":"desktop","to":"operator-…",
 "result":{"kind":"image","data":"<base64 JPEG>","width":1920,"height":1080,"format":"jpeg"}}
```

**`result` is untagged**: on success it is the payload object, on failure a bare
string.

```json
{"type":"response","id":"…","from":"reaven-server","to":"…","result":"'pc' is offline"}
```

Serde's default `Result` encoding (`{"Ok": {…}}`) is deliberately not used — it
forced every non-Rust client to unwrap a wrapper for no benefit.

### Operations

| op | fields | payload kind |
|---|---|---|
| `exec` | `command` | `text` (`stdout`, `stderr`, `code`) |
| `upload` | `path`, `data` (base64) | `text` |
| `download` | `path` | `file` (`data` base64) |
| `screenshot` | `monitor`, `quality` 1–100, `max_width` (0 = native) | `image` |
| `camera_snapshot` | `index`, `width`, `quality` | `image` |
| `list_screens` | — | `json` (`data` = stringified array of monitor objects) |
| `list_cameras` | — | `json` |
| `ping` | — | `text` |

Payload shapes:

```json
{"kind":"text","stdout":"…","stderr":"…","code":0}
{"kind":"file","data":"<base64>"}
{"kind":"image","data":"<base64 JPEG>","width":1920,"height":1080,"format":"jpeg"}
{"kind":"json","data":"[{\"index\":0,\"name\":\"screen\",\"width\":1440,…}]"}
```

## Interactive shell (PTY)

| type | fields |
|---|---|
| `shell_start` | `id`, `target`, `rows`, `cols`, `term` |
| `shell_output` | `id`, `to`, `data` (base64 raw bytes) |
| `shell_input` | `id`, `to`, `data` (base64) |
| `shell_resize` | `id`, `to`, `rows`, `cols` |
| `shell_close` | `id`, `to` |
| `shell_exit` | `id`, `to`, `code` |

Output is raw PTY bytes: ANSI escape sequences pass through untouched.

## Video streaming

```json
{"type":"video_start","id":"<uuid>","from":"","target":"desktop",
 "source":{"kind":"screen","monitor":0},
 "max_width":1600,"quality":70,"fps":12}
```

`source` is tagged: `{"kind":"screen","monitor":N}` or `{"kind":"camera","index":N}`.
`fps` is 1–60, `quality` 1–100, `max_width` 0 = native (down-scaling keeps aspect).

```json
{"type":"video_frame","id":"<uuid>","from":"desktop","to":"operator-…",
 "seq":42,"width":1600,"height":900,
 "source":{"kind":"screen","monitor":0},
 "data":"<base64 JPEG>"}
```

`seq` increases monotonically per stream and lets a client drop stale frames.

| type | fields |
|---|---|
| `video_stop` | `id`, `to` — sent by either side to end a stream |
| `video_error` | `id`, `to`, `message` — e.g. no such monitor, camera busy |

The agent also emits a final `video_stop` when its capture thread finishes.

## Input injection

```json
{"type":"control","id":"<uuid>","from":"","target":"desktop",
 "stream":"<uuid of a video stream, or null>",
 "events":[{"event":"click","button":"left","x":0.5,"y":0.25}]}
```

The agent answers with a `response` carrying the same `id` (payload `text` such as
`"1 event(s) applied"`), or an error string.

`x`/`y` are **normalised to `0.0..=1.0`** across the streamed frame. The agent maps
them onto absolute desktop pixels using the geometry of the stream identified by
`stream` (or the most recent screen stream), clamping out-of-range and `NaN`
values.

| event | fields |
|---|---|
| `move` | `x`, `y` |
| `down` | `button` (`left`\|`middle`\|`right`), `x`, `y` |
| `up` | `button`, `x`, `y` |
| `click` | `button`, `x`, `y` |
| `double_click` | `button`, `x`, `y` |
| `scroll` | `x`, `y`, `dx`, `dy` (positive `dy` scrolls down) |
| `key` | `key` (name, e.g. `Return`, `escape`, `F5`, `a`), `pressed` |
| `text` | `text` (layout-aware string) |
| `combo` | `keys` (array, pressed in order, released in reverse — e.g. `["ctrl","alt","t"]`) |

Unknown key names are rejected with an error rather than silently ignored.

An agent started with `--no-input` refuses every `control` frame with
`this agent is running with --no-input (view-only)`.

## Throughput notes

- Frames are base64 inside JSON text messages, so a frame costs ~4/3 its JPEG
  size plus JSON and WebSocket/TLS overhead. A 1600px-wide screen at quality 70
  lands around 60–200 KB per frame.
- TLS adds a small per-message overhead but no extra round trip after the
  handshake. WebSocket permessage-deflate is not enabled.
- The agent caps `fps`; a slow consumer does not slow capture but does grow the
  relay's per-peer queue. Prefer 8–15 fps over WAN links.
- `MAX_FRAME_BYTES` is 32 MB at both the protocol and WebSocket layers; anything
  larger is refused and the connection dropped.
- Error strings are sent as `error` frames before the socket closes where
  possible (authentication failure, duplicate peer name).
