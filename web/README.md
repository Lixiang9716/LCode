# LCode Web Dashboard

Real-time session monitor for the LCode agent (Rust CLI). It renders the
`AgentEvent` stream of a running agent session — assistant text, tool calls,
tool results and task status — with a terminal-style dark theme.

## Quick start

```bash
cd web
npm install
npm run dev
```

Open http://localhost:3000. By default the dashboard runs in **mock mode**:
`/api/ws` simulates a complete agent session (reads, builds, edits, test
runs, task completion) so the UI can be demoed and developed without a real
agent. Use the `mode` selector in the header to switch between
`auto` / `mock` / `real agent`.

## Architecture

```
Browser (dashboard page)
  │  JSON event envelopes — one object per message
  ▼
/api/ws  (Next.js Route Handler, app/api/ws/route.ts)
  ├─ mock mode: simulated session (lib/mock-events.ts)
  └─ relay mode: forwards frames from a real agent WebSocket
                 (LCODE_AGENT_WS_URL), fan-out to all viewers
```

- The dashboard page (`app/page.tsx` + `components/Dashboard.tsx`) renders
  the event feed with a typewriter effect, a session status panel (status,
  turns, token stats, elapsed time) and color-coded event types:
  - text / assistant output — green
  - tool calls & results — yellow/amber
  - task completion — blue
  - errors — red
- `lib/feed.ts` is a pure state reducer that turns the event stream into
  rendered blocks and session statistics (framework-free, unit-testable).

## Event protocol

Every message is a JSON object mirroring the `AgentEvent` enum in
`src/agent/event.rs` (same variant names and snake_case fields as serde):

```json
{ "ts": 1723370000000, "sessionId": "mock-xyz", "seq": 12,
  "type": "ToolCallRequested", "id": "call_01", "name": "ReadFile",
  "arguments": { "path": "src/main.rs" }, "requires_approval": false }
```

Variants: `Hello`, `SessionStarted`, `TurnStarted`, `TextGenerated`,
`ToolCallRequested`, `ToolCallExecuted`, `ToolCallFailed`,
`ToolCallDeclined`, `TurnFinished`, `TaskFinished`, `TaskAborted`, `Error`.
`Ping` frames are transport keep-alives and are ignored by the UI. Unknown
variants render as a generic line, so the dashboard stays forward-compatible
with future `AgentEvent` additions.

## Connecting a real agent (future `lcode serve`)

The dashboard is designed to be pointed at the WebSocket endpoint of a real
agent server (upcoming `lcode serve` command). Two knobs:

| Env var                      | Where      | Effect                                            |
| ---------------------------- | ---------- | ------------------------------------------------- |
| `LCODE_AGENT_WS_URL`         | server     | `/api/ws` relays frames from this upstream agent  |
| `NEXT_PUBLIC_AGENT_WS_URL`   | client     | browser connects **directly** to this WebSocket   |

```bash
# relay through the Next.js server
LCODE_AGENT_WS_URL=ws://127.0.0.1:5000/ws npm run dev
```

With no env vars and no `?mock=` param, the server defaults to mock mode.
`/api/ws?mock=1` forces mock; `/api/ws?mock=0` forbids it and reports an
error if no upstream is configured.

## Transport: WebSocket vs HTTP stream

Next.js's built-in web server does **not** support WebSocket upgrades for
Route Handlers (`handleUpgrade` is a no-op outside HMR), so `/api/ws` serves
the event stream over **NDJSON** (one JSON object per line). The client
(`lib/transport.ts`) *probes* with a real WebSocket first and falls back to
the HTTP stream if the server cannot upgrade — so the exact same code uses a
true WebSocket when the endpoint supports it (custom server, `lcode serve`,
or a `NEXT_PUBLIC_AGENT_WS_URL` target), with no change in behavior. The
connection indicator in the header shows which transport is in use
(`ws` vs `http-stream`).

## Production

```bash
npm run build
npm run start
```

## Mock session

`lib/mock-events.ts` replays a realistic session on loop: it "fixes a flaky
test" by reading code, running `cargo test`, editing `src/event.rs`, and
re-running the suite — with occasional tool failures, retries and token
stats, paced like a real agent.
