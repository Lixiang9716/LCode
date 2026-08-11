"use client";

import { useEffect, useState } from "react";
import type { SessionStats } from "@/lib/feed";
import type { ConnStatus, TransportKind } from "@/lib/transport";
import type { SessionStatus } from "@/lib/types";

const STATUS_STYLES: Record<SessionStatus, { dot: string; label: string; text: string }> = {
  idle: { dot: "bg-zinc-500", label: "idle", text: "text-zinc-400" },
  running: { dot: "bg-emerald-400 animate-pulse", label: "running", text: "text-emerald-300" },
  completed: { dot: "bg-sky-400", label: "completed", text: "text-sky-300" },
  aborted: { dot: "bg-amber-400", label: "aborted", text: "text-amber-300" },
  error: { dot: "bg-red-500", label: "error", text: "text-red-400" },
};

const CONN_STYLES: Record<ConnStatus, { dot: string; label: string }> = {
  connecting: { dot: "bg-yellow-400 animate-pulse", label: "connecting" },
  connected: { dot: "bg-emerald-400", label: "connected" },
  closed: { dot: "bg-zinc-600", label: "closed" },
  error: { dot: "bg-red-500", label: "error" },
};

function fmtDuration(ms: number | null): string {
  if (ms === null) return "—";
  const s = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(s / 60);
  return m > 0 ? `${m}m ${s % 60}s` : `${s}s`;
}

function fmtNum(n: number | null): string {
  return n === null ? "—" : n.toLocaleString();
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-3 py-1">
      <span className="shrink-0 font-mono text-[11px] uppercase tracking-wider text-zinc-600">{label}</span>
      <span className="min-w-0 truncate text-right font-mono text-xs text-zinc-300">{children}</span>
    </div>
  );
}

export function SessionPanel({
  stats,
  sessionId,
  sessionMode,
  connStatus,
  connKind,
  connDetail,
}: {
  stats: SessionStats;
  sessionId: string | null;
  sessionMode: "mock" | "relay" | null;
  connStatus: ConnStatus;
  connKind: TransportKind | null;
  connDetail?: string;
}) {
  // Tick once per second while running to keep the elapsed clock fresh.
  const [, setTick] = useState(0);
  useEffect(() => {
    if (stats.status !== "running") return;
    const t = setInterval(() => setTick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [stats.status]);

  const st = STATUS_STYLES[stats.status];
  const cn = CONN_STYLES[connStatus];

  return (
    <div className="flex h-full flex-col gap-4 p-4">
      <section className="rounded-md border border-zinc-800 bg-zinc-900/50 p-3">
        <div className="mb-2 flex items-center gap-2">
          <span className={`h-2 w-2 rounded-full ${st.dot}`} />
          <span className={`font-mono text-sm font-semibold uppercase tracking-wider ${st.text}`}>{st.label}</span>
        </div>
        <Row label="session">{sessionId ?? "—"}</Row>
        <Row label="mode">{sessionMode ?? "—"}</Row>
        <Row label="turn">{stats.turn === 0 ? "—" : stats.turn}</Row>
        <Row label="tool calls">{fmtNum(stats.toolCalls)}</Row>
        <Row label="events">{fmtNum(stats.events)}</Row>
        <Row label="prompt tok">{fmtNum(stats.promptTokens)}</Row>
        <Row label="completion tok">{fmtNum(stats.completionTokens)}</Row>
        <Row label="elapsed">{fmtDuration(stats.startedAt ? Date.now() - stats.startedAt : null)}</Row>
      </section>

      {stats.task && (
        <section className="rounded-md border border-zinc-800 bg-zinc-900/50 p-3">
          <div className="mb-1.5 font-mono text-[11px] uppercase tracking-wider text-zinc-600">task</div>
          <p className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-cyan-300/90">{stats.task}</p>
        </section>
      )}

      <section className="rounded-md border border-zinc-800 bg-zinc-900/50 p-3">
        <div className="mb-1.5 font-mono text-[11px] uppercase tracking-wider text-zinc-600">connection</div>
        <div className="mb-2 flex items-center gap-2">
          <span className={`h-2 w-2 rounded-full ${cn.dot}`} />
          <span className="font-mono text-xs text-zinc-300">{cn.label}</span>
        </div>
        <Row label="transport">
          {connKind === "websocket" ? "WebSocket" : connKind === "stream" ? "HTTP stream" : "—"}
        </Row>
        <Row label="endpoint">
          <span title={connDetail}>{connDetail ?? "—"}</span>
        </Row>
      </section>

      <p className="mt-auto px-1 font-mono text-[11px] leading-relaxed text-zinc-600">
        mock mode runs a simulated agent session. Connect a real agent by setting{" "}
        <span className="text-zinc-400">LCODE_AGENT_WS_URL</span> on the server, or{" "}
        <span className="text-zinc-400">NEXT_PUBLIC_AGENT_WS_URL</span> for a direct browser WebSocket.
      </p>
    </div>
  );
}
