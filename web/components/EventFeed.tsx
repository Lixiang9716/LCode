"use client";

import type { Block, ToolBlock } from "@/lib/feed";

function clock(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour12: false });
}

const META_TONES: Record<string, string> = {
  dim: "text-zinc-500",
  info: "text-cyan-400",
  warn: "text-amber-400",
  ok: "text-sky-400",
  error: "text-red-400",
};

function ToolStateChip({ block }: { block: ToolBlock }) {
  if (block.requiresApproval && block.state === "pending") {
    return <span className="text-[11px] rounded border border-orange-500/50 px-1.5 py-px text-orange-300">approval</span>;
  }
  const styles: Record<string, string> = {
    pending: "border-zinc-700 text-zinc-500",
    done: "border-emerald-500/50 text-emerald-300",
    failed: "border-red-500/60 text-red-400",
    declined: "border-orange-500/60 text-orange-300",
  };
  const label: Record<string, string> = { pending: "…", done: "done", failed: "failed", declined: "declined" };
  return (
    <span className={`text-[11px] rounded border px-1.5 py-px ${styles[block.state]}`}>{label[block.state]}</span>
  );
}

function ToolBlockView({ block, onToggle }: { block: ToolBlock; onToggle: (id: number) => void }) {
  let inlineArgs = "";
  try {
    inlineArgs = JSON.stringify(block.args);
  } catch {
    inlineArgs = String(block.args);
  }
  const truncated = inlineArgs.length > 110 ? inlineArgs.slice(0, 107) + "…" : inlineArgs;

  return (
    <div className="rounded-md border border-zinc-800 bg-zinc-900/50">
      <button
        type="button"
        onClick={() => onToggle(block.id)}
        className="flex w-full cursor-pointer items-center gap-2 px-3 py-1.5 text-left hover:bg-zinc-800/40"
      >
        <span className="text-amber-300">◈</span>
        <span className="font-medium text-amber-200">{block.name}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-zinc-500">{truncated}</span>
        <ToolStateChip block={block} />
        <span className="font-mono text-[11px] text-zinc-600">{clock(block.ts)}</span>
        <span className="text-[10px] text-zinc-600">{block.expanded ? "▾" : "▸"}</span>
      </button>

      <div className="border-t border-zinc-800/70 px-3 py-2">
        {block.expanded && (
          <pre className="mb-2 whitespace-pre-wrap font-mono text-xs text-amber-200/60">{pretty(block.args)}</pre>
        )}
        {block.error && (
          <pre className="whitespace-pre-wrap font-mono text-xs text-red-400">✖ {block.error}</pre>
        )}
        {block.output && (
          <pre
            className={`whitespace-pre-wrap font-mono text-xs leading-relaxed text-emerald-200/70 ${
              block.expanded ? "" : "line-clamp-6"
            }`}
          >
            {block.output}
          </pre>
        )}
        {!block.error && !block.output && (
          <span className="font-mono text-xs text-zinc-600">
            {block.state === "pending" ? "awaiting result…" : "no output"}
          </span>
        )}
      </div>
    </div>
  );
}

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function EventFeed({
  blocks,
  onToggleTool,
}: {
  blocks: Block[];
  onToggleTool: (id: number) => void;
}) {
  if (blocks.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-zinc-600">
        <span>
          waiting for agent events…{" "}
          <span className="animate-pulse">▮</span>
        </span>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {blocks.map((block) => {
        if (block.kind === "meta") {
          return (
            <div key={block.id} className={`flex gap-2 px-1 font-mono text-xs ${META_TONES[block.tone]}`}>
              <span className="shrink-0 text-zinc-700">{clock(block.ts)}</span>
              <span className="whitespace-pre-wrap">{block.text}</span>
            </div>
          );
        }
        if (block.kind === "assistant") {
          const done = block.revealed >= block.content.length;
          return (
            <div key={block.id} className="px-1 font-mono text-[13px] leading-relaxed text-emerald-300/90">
              <span className="whitespace-pre-wrap">{block.content.slice(0, block.revealed)}</span>
              {!done && <span className="animate-blink text-emerald-300">▌</span>}
            </div>
          );
        }
        return <ToolBlockView key={block.id} block={block} onToggle={onToggleTool} />;
      })}
    </div>
  );
}
