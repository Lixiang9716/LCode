/**
 * Client-side event transport.
 *
 * The dashboard consumes the same JSON envelope protocol (lib/types.ts)
 * over two interchangeable transports:
 *
 *  - WebSocket  – used when the endpoint genuinely speaks WebSocket
 *                 (a custom server or the future `lcode serve`).
 *  - HTTP stream – fallback for Next.js's built-in web server, which does
 *                 not support WebSocket upgrades for Route Handlers.
 *
 * For http(s) URLs we *probe* with a short-lived WebSocket first: if the
 * server answers the upgrade we stay on WebSocket, otherwise we
 * transparently reconnect over a fetch() NDJSON stream.
 */

import { isControlEvent, parseEvent, type AgentEvent } from "./types";

export type TransportKind = "websocket" | "stream";
export type ConnStatus = "connecting" | "connected" | "closed" | "error";

export interface EventConnection {
  /** Transport used once the connection settles (ws:// → websocket, http:// → stream). */
  kind: TransportKind;
  /** Tear down the connection (idempotent). */
  close(): void;
}

export interface ConnectOptions {
  url: string;
  onEvent: (ev: AgentEvent) => void;
  onStatus: (status: ConnStatus, kind: TransportKind | null, detail?: string) => void;
  /** How long to wait for the WebSocket probe to open before falling back. */
  probeTimeoutMs?: number;
}

const DEFAULT_PROBE_TIMEOUT_MS = 1500;

function toWsUrl(url: string): string {
  return url.replace(/^http/, "ws");
}

function handleWsMessage(msg: MessageEvent, onEvent: (ev: AgentEvent) => void): void {
  let raw: unknown;
  try {
    raw = JSON.parse(String(msg.data));
  } catch {
    return;
  }
  const ev = parseEvent(raw);
  if (ev && !isControlEvent(ev)) onEvent(ev);
}

export function connectEventStream(opts: ConnectOptions): EventConnection {
  const url = opts.url;
  if (/^wss?:/i.test(url)) {
    return openWebSocket(url, opts);
  }
  return probeThenStream(url, opts);
}

/** Direct WebSocket connection to a real WS endpoint (no fallback). */
function openWebSocket(url: string, opts: ConnectOptions): EventConnection {
  let closed = false;
  let ws: WebSocket | null = null;

  const connect = () => {
    opts.onStatus("connecting", "websocket", url);
    ws = new WebSocket(url);
    ws.onopen = () => {
      if (!closed) opts.onStatus("connected", "websocket", url);
    };
    ws.onmessage = (msg) => handleWsMessage(msg, opts.onEvent);
    ws.onerror = () => {
      if (!closed) opts.onStatus("error", "websocket", "connection error");
    };
    ws.onclose = () => {
      if (!closed) opts.onStatus("closed", "websocket", "connection closed");
    };
  };

  connect();

  return {
    kind: "websocket",
    close() {
      closed = true;
      ws?.close();
    },
  };
}

/**
 * Same-origin / HTTP endpoint: probe with a WebSocket first (fast upgrade
 * if the server supports it, e.g. a custom server wrapping Next), otherwise
 * fall back to a streaming fetch of the same URL.
 */
function probeThenStream(url: string, opts: ConnectOptions): EventConnection {
  let closed = false;
  let settled = false;
  let stream: { close(): void } | null = null;
  let probe: WebSocket | null = null;
  const timeoutMs = opts.probeTimeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS;

  const fallbackToStream = (detail?: string) => {
    if (closed || settled) return;
    settled = true;
    if (timer) clearTimeout(timer);
    probe?.close();
    stream = openHttpStream(url, opts, detail);
  };

  const onProbeMessage = (msg: MessageEvent) => {
    if (!settled || closed) return;
    handleWsMessage(msg, opts.onEvent);
  };

  probe = new WebSocket(toWsUrl(url));
  probe.onopen = () => {
    if (closed || settled) return;
    settled = true;
    if (timer) clearTimeout(timer);
    opts.onStatus("connected", "websocket", url);
    // Probe *is* the live connection now.
    probe.onmessage = onProbeMessage;
    probe.onerror = () => {
      if (!closed) opts.onStatus("error", "websocket", "connection error");
    };
    probe.onclose = () => {
      if (!closed) opts.onStatus("closed", "websocket", "connection closed");
    };
  };
  probe.onerror = () => fallbackToStream("websocket upgrade failed");
  probe.onclose = () => fallbackToStream("websocket upgrade closed");
  const timer = setTimeout(() => fallbackToStream("websocket upgrade timed out"), timeoutMs);

  return {
    kind: "stream",
    close() {
      closed = true;
      if (timer) clearTimeout(timer);
      probe?.close();
      stream?.close();
    },
  };
}

/** NDJSON stream over fetch(). */
function openHttpStream(
  url: string,
  opts: ConnectOptions,
  detail?: string,
): { close(): void } {
  let closed = false;
  const ctrl = new AbortController();

  opts.onStatus("connecting", "stream", detail ?? "websocket unavailable — using HTTP stream");

  void (async () => {
    try {
      const res = await fetch(url, {
        signal: ctrl.signal,
        headers: { Accept: "application/x-ndjson" },
      });
      if (closed) return;
      if (!res.ok || !res.body) {
        opts.onStatus("error", "stream", `HTTP ${res.status}`);
        return;
      }
      opts.onStatus("connected", "stream", url);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (closed) return;
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let nl: number;
        while ((nl = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, nl).trim();
          buffer = buffer.slice(nl + 1);
          if (!line) continue;
          let raw: unknown;
          try {
            raw = JSON.parse(line);
          } catch {
            continue;
          }
          const ev = parseEvent(raw);
          if (ev && !isControlEvent(ev)) opts.onEvent(ev);
        }
      }
      if (!closed) opts.onStatus("closed", "stream", "stream ended");
    } catch (err) {
      if (closed) return;
      opts.onStatus("error", "stream", err instanceof Error ? err.message : "stream failed");
    }
  })();

  return {
    close() {
      closed = true;
      ctrl.abort();
    },
  };
}
