"use client";

import { useEffect, useRef, useState } from "react";
import { activeAssistant, applyEvent, initialFeed, type FeedState } from "@/lib/feed";
import { connectEventStream, type ConnStatus, type TransportKind } from "@/lib/transport";
import { EventFeed } from "./EventFeed";
import { SessionPanel } from "./SessionPanel";

type ConnMode = "auto" | "mock" | "real";

const REAL_WS_URL: string | null = process.env.NEXT_PUBLIC_AGENT_WS_URL ?? null;

function endpointFor(mode: ConnMode): string {
  if (mode === "mock") return "/api/ws?mock=1";
  if (mode === "real") return REAL_WS_URL ?? "/api/ws?mock=0";
  return "/api/ws";
}

const MODE_LABEL: Record<ConnMode, string> = { auto: "auto", mock: "mock", real: "real agent" };

export function Dashboard() {
  const [feed, setFeed] = useState<FeedState>(initialFeed);
  const [mode, setMode] = useState<ConnMode>("auto");
  const [conn, setConn] = useState<{ status: ConnStatus; kind: TransportKind | null; detail?: string }>({
    status: "connecting",
    kind: null,
  });
  const [sessionNonce, setSessionNonce] = useState(0);
  const [stickToBottom, setStickToBottom] = useState(true);
  const feedRef = useRef<HTMLDivElement>(null);

  // Live connection — reconnects on mode change or manual session reset.
  useEffect(() => {
    const connection = connectEventStream({
      url: endpointFor(mode),
      onEvent: (ev) => setFeed((prev) => applyEvent(prev, ev)),
      onStatus: (status, kind, detail) => setConn({ status, kind, detail }),
    });
    return () => connection.close();
  }, [mode, sessionNonce]);

  // Typewriter: reveal the trailing assistant block gradually.
  useEffect(() => {
    const t = setInterval(() => {
      setFeed((prev) => {
        const last = activeAssistant(prev);
        if (!last || last.revealed >= last.content.length) return prev;
        const blocks = [...prev.blocks];
        blocks[blocks.length - 1] = {
          ...last,
          revealed: Math.min(last.content.length, last.revealed + 3),
        };
        return { ...prev, blocks };
      });
    }, 24);
    return () => clearInterval(t);
  }, []);

  // Stick to the bottom of the feed unless the user scrolls up.
  useEffect(() => {
    if (!stickToBottom) return;
    const el = feedRef.current;
    if (!el) return;
    const raf = requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
    return () => cancelAnimationFrame(raf);
  }, [feed.blocks, stickToBottom]);

  const handleScroll = () => {
    const el = feedRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setStickToBottom(nearBottom);
  };

  const newSession = () => {
    setFeed(initialFeed);
    setConn({ status: "connecting", kind: null });
    setSessionNonce((n) => n + 1);
  };

  const clearFeed = () => {
    setFeed(initialFeed);
  };

  const toggleTool = (id: number) => {
    setFeed((prev) => ({
      ...prev,
      blocks: prev.blocks.map((b) =>
        b.kind === "tool" && b.id === id ? { ...b, expanded: !b.expanded } : b,
      ),
    }));
  };

  const connOk = conn.status === "connected";
  const dotClass = connOk ? "bg-emerald-400" : conn.status === "connecting" ? "animate-pulse bg-yellow-400" : "bg-red-500";

  return (
    <div className="flex h-dvh flex-col">
      {/* Header */}
      <header className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-zinc-800 bg-zinc-900/60 px-4 py-2.5">
        <h1 className="font-mono text-sm font-bold tracking-tight text-zinc-100">
          lcode <span className="text-zinc-500">·</span> <span className="text-emerald-400">session monitor</span>
        </h1>
        <div className="flex items-center gap-2" title={conn.detail}>
          <span className={`h-2 w-2 rounded-full ${dotClass}`} />
          <span className="font-mono text-xs text-zinc-400">
            {conn.status}
            {conn.kind === "websocket" ? " · ws" : conn.kind === "stream" ? " · http-stream" : ""}
          </span>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <label className="flex items-center gap-1.5 font-mono text-xs text-zinc-500">
            mode
            <select
              value={mode}
              onChange={(e) => setMode(e.target.value as ConnMode)}
              className="cursor-pointer rounded border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono text-xs text-zinc-200 outline-none focus:border-emerald-500"
            >
              <option value="auto">auto</option>
              <option value="mock">mock</option>
              <option value="real">real agent</option>
            </select>
          </label>
          <button
            type="button"
            onClick={newSession}
            className="cursor-pointer rounded border border-zinc-700 bg-zinc-900 px-2.5 py-1 font-mono text-xs text-zinc-300 hover:border-emerald-500 hover:text-emerald-300"
          >
            new session
          </button>
          <button
            type="button"
            onClick={clearFeed}
            className="cursor-pointer rounded border border-zinc-700 bg-zinc-900 px-2.5 py-1 font-mono text-xs text-zinc-300 hover:border-zinc-500"
          >
            clear
          </button>
        </div>
      </header>

      {/* Body */}
      <div className="flex min-h-0 flex-1">
        <main
          ref={feedRef}
          onScroll={handleScroll}
          className="relative min-w-0 flex-1 overflow-y-auto bg-[#0a0e13] px-4 py-3"
        >
          <EventFeed blocks={feed.blocks} onToggleTool={toggleTool} />
          {!stickToBottom && (
            <button
              type="button"
              onClick={() => setStickToBottom(true)}
              className="fixed bottom-4 left-1/2 -translate-x-1/2 cursor-pointer rounded-full border border-zinc-700 bg-zinc-900/95 px-3 py-1 font-mono text-xs text-zinc-300 shadow-lg hover:border-emerald-500 hover:text-emerald-300"
            >
              ▼ jump to latest
            </button>
          )}
        </main>
        <aside className="w-80 shrink-0 overflow-y-auto border-l border-zinc-800 bg-zinc-950/60">
          <SessionPanel
            stats={feed.stats}
            sessionId={feed.sessionId}
            sessionMode={feed.sessionMode}
            connStatus={conn.status}
            connKind={conn.kind}
            connDetail={conn.detail}
          />
        </aside>
      </div>

      {/* Footer status strip */}
      <footer className="flex items-center gap-3 border-t border-zinc-800 bg-zinc-900/60 px-4 py-1.5 font-mono text-[11px] text-zinc-500">
        <span>
          mode: <span className="text-zinc-300">{MODE_LABEL[mode]}</span>
        </span>
        <span className="text-zinc-700">·</span>
        <span>
          endpoint: <span className="text-zinc-400">{endpointFor(mode)}</span>
        </span>
        <span className="text-zinc-700">·</span>
        <span>
          protocol: JSON envelopes mirroring <span className="text-zinc-400">AgentEvent</span> (src/agent/event.rs)
        </span>
      </footer>
    </div>
  );
}
