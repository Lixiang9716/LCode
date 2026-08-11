/**
 * /api/ws — real-time agent event stream.
 *
 * Streams LCode `AgentEvent` JSON envelopes (one per line, NDJSON) to the
 * dashboard. Two modes:
 *
 *  - mock (default): a simulated agent session so the dashboard can be
 *    demoed without a real agent (`?mock=1` to force, `?mock=0` to forbid).
 *  - relay: forwards frames from a real agent WebSocket endpoint, configured
 *    via the `LCODE_AGENT_WS_URL` environment variable. All viewers share a
 *    single upstream connection (fan-out).
 *
 * Transport note: Next.js's built-in web server does not support WebSocket
 * upgrades for Route Handlers (see `handleUpgrade` in next-server.js — it is
 * a no-op outside HMR). This handler therefore serves a streaming HTTP
 * response; the client probes with a WebSocket first and uses the stream as
 * a transparent fallback, so the same endpoint serves both transports when
 * run behind a WS-capable server (e.g. a custom server or `lcode serve`).
 */

import { MockAgentSession } from "@/lib/mock-events";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

const PROTOCOL_VERSION = 1;
const HEARTBEAT_MS = 25_000;

function sendLine(controller: ReadableStreamDefaultController<Uint8Array>, encoder: TextEncoder, obj: unknown): void {
  try {
    controller.enqueue(encoder.encode(JSON.stringify(obj) + "\n"));
  } catch {
    // Controller already closed (client disconnected) — ignore.
  }
}

function controlFrame(type: string, sessionId: string, seq: number, fields: Record<string, unknown> = {}) {
  return { ts: Date.now(), sessionId, seq, type, ...fields };
}

export async function GET(req: Request): Promise<Response> {
  // If a WebSocket upgrade request somehow reaches us (custom proxy), fail
  // fast so the client falls back to the HTTP stream immediately.
  if ((req.headers.get("upgrade") ?? "").toLowerCase() === "websocket") {
    return new Response(
      "WebSocket upgrade is not supported by the Next.js web server; use the HTTP stream fallback.",
      { status: 426 },
    );
  }

  const url = new URL(req.url);
  const mockParam = url.searchParams.get("mock");
  const loop = url.searchParams.get("loop") !== "0";
  const agentWsUrl = (process.env.LCODE_AGENT_WS_URL ?? "").trim();
  const useMock = mockParam === "1" || (mockParam !== "0" && !agentWsUrl);

  const encoder = new TextEncoder();
  const sessionId = useMock ? `mock-${Date.now().toString(36)}` : "relay";
  let seq = 0;

  const cleanup: Array<() => void> = [];

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      sendLine(controller, encoder, controlFrame("Hello", sessionId, ++seq, {
        mode: useMock ? "mock" : "relay",
        version: PROTOCOL_VERSION,
      }));

      // Heartbeat keeps idle streams (relay mode) alive through proxies.
      const heartbeat = setInterval(() => {
        sendLine(controller, encoder, controlFrame("Ping", sessionId, ++seq));
      }, HEARTBEAT_MS);
      cleanup.push(() => clearInterval(heartbeat));

      if (useMock) {
        const mock = new MockAgentSession((fields) => sendLine(controller, encoder, fields), {
          loop,
          sessionId,
        });
        mock.start();
        cleanup.push(() => mock.stop());
        return;
      }

      if (!agentWsUrl) {
        sendLine(controller, encoder, controlFrame("Error", sessionId, ++seq, {
          message:
            "No upstream agent configured. Set LCODE_AGENT_WS_URL to a real agent WebSocket, or load /api/ws?mock=1.",
        }));
        controller.close();
        return;
      }

      const relay = getRelay(agentWsUrl);
      const unsubscribe = relay.subscribe((line) => sendLine(controller, encoder, line));
      cleanup.push(() => unsubscribe());
    },
    cancel() {
      for (const fn of cleanup) fn();
    },
  });

  return new Response(stream, {
    headers: {
      "Content-Type": "application/x-ndjson; charset=utf-8",
      "Cache-Control": "no-cache, no-transform",
      "X-Accel-Buffering": "no",
    },
  });
}

/* ------------------------------------------------------------------ */
/* Upstream relay (shared across all viewers)                          */
/* ------------------------------------------------------------------ */

class UpstreamRelay {
  readonly url: string;
  private ws: WebSocket | null = null;
  private readonly subs = new Set<(line: string) => void>();
  private stopped = false;
  private reconnectDelay = 1000;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private seq = 0;

  constructor(url: string) {
    this.url = url;
  }

  subscribe(fn: (line: string) => void): () => void {
    this.subs.add(fn);
    return () => {
      this.subs.delete(fn);
    };
  }

  connect(): void {
    if (this.stopped || this.ws) return;
    try {
      this.ws = new WebSocket(this.url);
    } catch {
      this.ws = null;
      this.broadcastError(`Invalid LCODE_AGENT_WS_URL: ${this.url}`);
      return;
    }
    this.ws.onopen = () => {
      this.reconnectDelay = 1000;
      this.broadcastFrame("Hello", { mode: "relay", version: PROTOCOL_VERSION });
    };
    this.ws.onmessage = (ev) => {
      const data = String(ev.data);
      if (data.includes("\n")) {
        for (const line of data.split("\n")) {
          if (line.trim()) this.broadcast(line);
        }
      } else if (data.trim()) {
        this.broadcast(data);
      }
    };
    this.ws.onclose = () => {
      this.ws = null;
      if (this.stopped) return;
      this.broadcastError("Upstream agent disconnected — reconnecting…");
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null;
        this.connect();
      }, this.reconnectDelay);
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, 10_000);
    };
  }

  close(): void {
    this.stopped = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.ws?.close();
    this.ws = null;
    this.subs.clear();
  }

  private broadcast(line: string): void {
    for (const fn of [...this.subs]) fn(line);
  }

  private broadcastFrame(type: string, fields: Record<string, unknown>): void {
    const frame = controlFrame(type, "relay", ++this.seq, fields);
    this.broadcast(JSON.stringify(frame));
  }

  private broadcastError(message: string): void {
    const frame = controlFrame("Error", "relay", ++this.seq, { message });
    this.broadcast(JSON.stringify(frame));
  }
}

let sharedRelay: UpstreamRelay | null = null;

function getRelay(agentWsUrl: string): UpstreamRelay {
  if (sharedRelay && sharedRelay.url === agentWsUrl) return sharedRelay;
  sharedRelay?.close();
  sharedRelay = new UpstreamRelay(agentWsUrl);
  sharedRelay.connect();
  return sharedRelay;
}
