/**
 * Wire protocol shared by the dashboard client and server.
 *
 * The envelope deliberately mirrors the `AgentEvent` enum from
 * src/agent/event.rs in the LCode Rust crate — including the snake_case
 * field names produced by serde serialization. Every message on the wire
 * is one JSON object: a single text frame over WebSocket, or one line of
 * NDJSON over the HTTP stream fallback.
 */

export interface Envelope {
  /** Epoch milliseconds at emit time. */
  ts: number;
  /** Opaque session identifier (changes per session). */
  sessionId: string;
  /** Per-session monotonic sequence number. */
  seq: number;
}

export interface HelloEvent extends Envelope {
  type: "Hello";
  /** "mock" – simulated stream; "relay" – forwarded from a real agent. */
  mode: "mock" | "relay";
  version: number;
}

export interface SessionStartedEvent extends Envelope {
  type: "SessionStarted";
  task: string;
}

export interface TurnStartedEvent extends Envelope {
  type: "TurnStarted";
  turn: number;
}

export interface TextGeneratedEvent extends Envelope {
  type: "TextGenerated";
  content: string;
}

export interface ToolCallRequestedEvent extends Envelope {
  type: "ToolCallRequested";
  id: string;
  name: string;
  arguments: unknown;
  requires_approval: boolean;
}

export interface ToolCallExecutedEvent extends Envelope {
  type: "ToolCallExecuted";
  id: string;
  output: string;
}

export interface ToolCallFailedEvent extends Envelope {
  type: "ToolCallFailed";
  id: string;
  error: string;
}

export interface ToolCallDeclinedEvent extends Envelope {
  type: "ToolCallDeclined";
  id: string;
}

export interface TurnFinishedEvent extends Envelope {
  type: "TurnFinished";
  turn: number;
}

export interface TaskFinishedEvent extends Envelope {
  type: "TaskFinished";
  turns: number;
  prompt_tokens: number;
  completion_tokens: number;
}

export interface TaskAbortedEvent extends Envelope {
  type: "TaskAborted";
  reason: string;
}

export interface ErrorEvent extends Envelope {
  type: "Error";
  message: string;
}

/** Any envelope we do not model yet (future AgentEvent variants). */
export interface UnknownEvent extends Envelope {
  type: string;
  [key: string]: unknown;
}

export type AgentEvent =
  | HelloEvent
  | SessionStartedEvent
  | TurnStartedEvent
  | TextGeneratedEvent
  | ToolCallRequestedEvent
  | ToolCallExecutedEvent
  | ToolCallFailedEvent
  | ToolCallDeclinedEvent
  | TurnFinishedEvent
  | TaskFinishedEvent
  | TaskAbortedEvent
  | ErrorEvent
  | UnknownEvent;

/** The modeled variants (everything except UnknownEvent). */
export type KnownAgentEvent = Exclude<AgentEvent, UnknownEvent>;

/** Narrow an envelope to a known variant so fields type-check. */
export function isKnownEvent(ev: AgentEvent): ev is KnownAgentEvent {
  switch (ev.type) {
    case "Hello":
    case "SessionStarted":
    case "TurnStarted":
    case "TextGenerated":
    case "ToolCallRequested":
    case "ToolCallExecuted":
    case "ToolCallFailed":
    case "ToolCallDeclined":
    case "TurnFinished":
    case "TaskFinished":
    case "TaskAborted":
    case "Error":
      return true;
    default:
      return false;
  }
}

export function parseEvent(raw: unknown): AgentEvent | null {
  if (typeof raw !== "object" || raw === null) return null;
  const ev = raw as Record<string, unknown>;
  if (typeof ev.type !== "string") return null;
  return ev as unknown as AgentEvent;
}

/** Transport-level control frames (keep-alives) that are never rendered. */
export function isControlEvent(ev: AgentEvent): boolean {
  return ev.type === "Ping" || ev.type.startsWith("_");
}

export type SessionStatus = "idle" | "running" | "completed" | "aborted" | "error";
