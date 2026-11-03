# lumen-ipc

Protocol between the Lumen daemon and its clients (window, tray, command line).

- **Transport:** Unix domain socket `$XDG_RUNTIME_DIR/lumen/ipc.sock` (private 0700 directory) on Linux, named pipe `\\.\pipe\lumen-ipc-<user>` on Windows, through `interprocess` (0BSD/Apache-2.0). `LUMEN_IPC_ENDPOINT` overrides it.
- **Format:** one JSON object per line, at most 1 MiB. Messages are rare and small, so JSON costs microseconds and keeps the protocol scriptable (`socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/lumen/ipc.sock`). Versioned by `PROTOCOL_VERSION` in an optional `hello`; compatibility rules in `src/proto.rs` (unknown fields ignored, `#[serde(default)]` on new fields, `Unknown` catch-alls on daemon-to-client enums).
- **Requests:** `hello`, `bye`, `list_outputs`, `get_state`, `apply` (output or all, path, scaling), `clear`, `get_properties` / `set_properties` (live preview or persist) / `reset_properties`, `set_paused`, `set_muted`, `set_volume`, `get_settings` / `set_settings`, `get_stats`, `subscribe` / `unsubscribe`, `shutdown`.
- **Events** (subscribed topics only): `state`, `outputs`, `settings`, `properties`, `stats`, `shutting_down`.

```text
-> {"id":3,"method":"set_paused","params":{"paused":true}}
<- {"reply":{"id":3,"ok":{"type":"done"}}}
<- {"event":{"type":"state","paused":true,"pause_reason":"user","muted":false,"volume":0.5,"outputs":[]}}
```

## Daemon side

Implement `Handler` (every method defaults to `unsupported`), then `Server::bind(&Endpoint::for_current_user(), handler, "lumen", version)`. Keep the `Server` alive; push changes with `server.publisher().publish(&Event::State(..))`. `Publisher::has_subscribers(Topic::Stats)` and `Handler::subscribers_changed` let the daemon sample stats only while someone looks.

**Cost:** nothing polls. One accept thread and, per connected client, a reader and a writer thread, all blocked in system calls while idle. Events are serialized once, only when a client subscribed to their topic; `publish` never blocks the caller (a lagging client misses events instead).

## Client side

`Client::connect_with_events(&endpoint, "name", |event| ...)` performs the handshake and calls the closure from its reader thread; typed helpers (`apply`, `state`, `set_properties`, …) block with a 5 s timeout. `Error::NotRunning` means no daemon listens.

## Tests

`cargo test -p lumen-ipc`: wire format (round trips, documented shapes, tolerance of newer peers) and end-to-end tests over a real socket (every call, events per subscription, subscriber counts, malformed and oversized frames, version mismatch, concurrent calls, stale socket file, shutdown and disconnection).

## Specs

None: this protocol is Lumen's own (no upstream equivalent). Windows named pipes keep their default security descriptor (owner and SYSTEM full access; other users cannot write requests).
