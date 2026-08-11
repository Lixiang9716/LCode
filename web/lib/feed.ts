/**
 * Pure feed state reducer: turns the incoming event envelope stream into
 * the rendered block list + session statistics. Kept framework-free so the
 * dashboard logic is easy to test and reason about.
 */

import type { AgentEvent } from "./types";
import { isKnownEvent } from "./types";
import type { SessionStatus } from "./types";

export type BlockTone = "dim" | "info" | "warn" | "ok" | "error";

export interface MetaBlock {
  id: number;
  kind: "meta";
  ts: number;
  text: string;
  tone: BlockTone;
}

export interface AssistantBlock {
  id: number;
  kind: "assistant";
  ts: number;
  content: string;
  /** Number of characters already revealed by the typewriter effect. */
  revealed: number;
}

export type ToolState = "pending" | "done" | "failed" | "declined";

export interface ToolBlock {
  id: number;
  kind: "tool";
  ts: number;
  callId: string;
  name: string;
  args: unknown;
  requiresApproval: boolean;
  state: ToolState;
  output?: string;
  error?: string;
  expanded: boolean;
}

export type Block = MetaBlock | AssistantBlock | ToolBlock;

export interface SessionStats {
  status: SessionStatus;
  task: string | null;
  turn: number;
  promptTokens: number | null;
  completionTokens: number | null;
  events: number;
  toolCalls: number;
  startedAt: number | null;
  lastEventAt: number | null;
  finishedAt: number | null;
}

export const initialStats: SessionStats = {
  status: "idle",
  task: null,
  turn: 0,
  promptTokens: null,
  completionTokens: null,
  events: 0,
  toolCalls: 0,
  startedAt: null,
  lastEventAt: null,
  finishedAt: null,
};

export interface FeedState {
  blocks: Block[];
  stats: SessionStats;
  sessionId: string | null;
  sessionMode: "mock" | "relay" | null;
  nextBlockId: number;
  toolByCallId: Map<string, number>;
}

export const initialFeed: FeedState = {
  blocks: [],
  stats: initialStats,
  sessionId: null,
  sessionMode: null,
  nextBlockId: 1,
  toolByCallId: new Map(),
};

/** How far the typewriter has revealed the most recent assistant block. */
export function activeAssistant(feed: FeedState): AssistantBlock | null {
  const last = feed.blocks[feed.blocks.length - 1];
  return last && last.kind === "assistant" ? last : null;
}

function meta(
  state: FeedState,
  ts: number,
  text: string,
  tone: BlockTone,
): FeedState {
  const block: MetaBlock = { id: state.nextBlockId, kind: "meta", ts, text, tone };
  return { ...state, nextBlockId: state.nextBlockId + 1, blocks: [...state.blocks, block] };
}

function tool(
  state: FeedState,
  ts: number,
  callId: string,
  name: string,
  args: unknown,
  requiresApproval: boolean,
): FeedState {
  const block: ToolBlock = {
    id: state.nextBlockId,
    kind: "tool",
    ts,
    callId,
    name,
    args,
    requiresApproval,
    state: "pending",
    expanded: false,
  };
  const toolByCallId = new Map(state.toolByCallId);
  toolByCallId.set(callId, block.id);
  return {
    ...state,
    nextBlockId: state.nextBlockId + 1,
    blocks: [...state.blocks, block],
    toolByCallId,
  };
}

function patchTool(state: FeedState, callId: string, patch: Partial<ToolBlock>): FeedState {
  const blockId = state.toolByCallId.get(callId);
  if (blockId === undefined) return state;
  const blocks = state.blocks.map((b) =>
    b.kind === "tool" && b.id === blockId ? ({ ...b, ...patch } as ToolBlock) : b,
  );
  return { ...state, blocks };
}

export function applyEvent(state: FeedState, ev: AgentEvent): FeedState {
  const stats = { ...state.stats, events: state.stats.events + 1, lastEventAt: ev.ts };

  if (!isKnownEvent(ev)) {
    const extra = JSON.stringify(stripEnvelope(ev));
    return meta(state, ev.ts, `${ev.type}${extra && extra !== "{}" ? ` ${extra}` : ""}`, "dim");
  }

  switch (ev.type) {
    case "Hello":
      return {
        ...state,
        stats,
        sessionId: ev.sessionId,
        sessionMode: ev.mode,
      };

    case "SessionStarted":
      return {
        ...meta(state, ev.ts, `▸ session started — ${ev.task}`, "info"),
        stats: {
          ...stats,
          status: "running",
          task: ev.task,
          turn: 0,
          promptTokens: null,
          completionTokens: null,
          startedAt: ev.ts,
          finishedAt: null,
        },
        sessionId: state.sessionId ?? ev.sessionId,
      };

    case "TurnStarted":
      return {
        ...meta(state, ev.ts, `── turn ${ev.turn} ──`, "dim"),
        stats: { ...stats, turn: Math.max(stats.turn, ev.turn) },
      };

    case "TextGenerated": {
      const blocks = [...state.blocks];
      const last = blocks[blocks.length - 1];
      if (last && last.kind === "assistant") {
        blocks[blocks.length - 1] = { ...last, content: last.content + ev.content };
      } else {
        const block: AssistantBlock = {
          id: state.nextBlockId,
          kind: "assistant",
          ts: ev.ts,
          content: ev.content,
          revealed: 0,
        };
        blocks.push(block);
        return { ...state, nextBlockId: state.nextBlockId + 1, stats, blocks };
      }
      return { ...state, stats, blocks };
    }

    case "ToolCallRequested": {
      const next = tool(state, ev.ts, ev.id, ev.name, ev.arguments, ev.requires_approval);
      return { ...next, stats: { ...next.stats, toolCalls: next.stats.toolCalls + 1 } };
    }

    case "ToolCallExecuted":
      return {
        ...patchTool(state, ev.id, { state: "done", output: ev.output }),
        stats,
      };

    case "ToolCallFailed":
      return patchTool(state, ev.id, { state: "failed", error: ev.error });

    case "ToolCallDeclined":
      return patchTool(state, ev.id, { state: "declined" });

    case "TurnFinished":
      return { ...state, stats };

    case "TaskFinished":
      return {
        ...meta(
          state,
          ev.ts,
          `✔ task finished · ${ev.turns} turns · ${ev.prompt_tokens.toLocaleString()} prompt + ${ev.completion_tokens.toLocaleString()} completion tokens`,
          "ok",
        ),
        stats: {
          ...stats,
          status: "completed",
          turn: Math.max(stats.turn, ev.turns),
          promptTokens: ev.prompt_tokens,
          completionTokens: ev.completion_tokens,
          finishedAt: ev.ts,
        },
      };

    case "TaskAborted":
      return {
        ...meta(state, ev.ts, `⏹ task aborted — ${ev.reason}`, "warn"),
        stats: { ...stats, status: "aborted", finishedAt: ev.ts },
      };

    case "Error":
      return {
        ...meta(state, ev.ts, `✖ ${ev.message}`, "error"),
        stats: { ...stats, status: "error", finishedAt: ev.ts },
      };
  }
}

function stripEnvelope(ev: AgentEvent): Record<string, unknown> {
  const { ts: _ts, sessionId: _sid, seq: _seq, type: _type, ...rest } = ev as Record<string, unknown>;
  return rest;
}
