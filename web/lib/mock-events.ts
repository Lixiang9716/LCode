/**
 * Server-side mock agent session.
 *
 * When no real agent is available, /api/ws serves a simulated session so
 * the dashboard can be demoed and tested end-to-end. The mock replays the
 * same envelope protocol a real agent would emit (see lib/types.ts), with
 * realistic pacing, tool calls, outputs and the occasional failure.
 *
 * Server-only: imported exclusively by app/api/ws/route.ts.
 */

export interface MockSessionOptions {
  /** Restart with a new session after the previous one finishes. */
  loop?: boolean;
  /** Session id used by the first session (so it matches the Hello frame). */
  sessionId?: string;
}

export type EmitFn = (frame: Record<string, unknown>) => void;

const TASKS = [
  "Fix the flaky test in tests/events.rs and update the CHANGELOG",
  "Add a --json output flag to the lcode CLI and document it in the README",
  "Investigate the O(n^2) hot path in the event bus and optimize it",
];

const TOOL_IDS = ["call_01", "call_02", "call_03", "call_04", "call_05"];

export class MockAgentSession {
  private timers: ReturnType<typeof setTimeout>[] = [];
  private sessionNo = 0;
  private seq = 0;
  private sessionId = "";
  private stopped = false;

  constructor(
    private readonly emit: EmitFn,
    private readonly opts: MockSessionOptions = {},
  ) {}

  start(): void {
    this.sessionNo = 0;
    this.schedule(() => this.runSession(), 250);
  }

  stop(): void {
    this.stopped = true;
    for (const t of this.timers) clearTimeout(t);
    this.timers = [];
  }

  private schedule(fn: () => void, delay: number): void {
    if (this.stopped) return;
    const t = setTimeout(() => {
      const i = this.timers.indexOf(t);
      if (i >= 0) this.timers.splice(i, 1);
      fn();
    }, delay);
    this.timers.push(t);
  }

  /** Build the shared envelope fields for the current session. */
  private frame(type: string, fields: Record<string, unknown>): Record<string, unknown> {
    return {
      ts: Date.now(),
      sessionId: this.sessionId,
      seq: this.seq,
      type,
      ...fields,
    };
  }

  private runSession(): void {
    if (this.stopped) return;
    this.sessionNo += 1;
    this.seq = 0;
    this.sessionId =
      this.sessionNo === 1 && this.opts.sessionId
        ? this.opts.sessionId
        : `mock-${Date.now().toString(36)}-${this.sessionNo}`;
    const task = TASKS[this.sessionNo % TASKS.length];

    this.schedule(() => this.emitFrame("SessionStarted", { task }), 150);

    // Turn 1 — inspect the code
    this.schedule(() => this.emitFrame("TurnStarted", { turn: 1 }), 500);
    this.schedule(
      () => this.emitFrame("TextGenerated", { content: "I'll start by inspecting the current state of the code." }),
      900,
    );
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content: "Let me read tests/events.rs and check how the event bus is exercised.",
        }),
      1350,
    );
    this.schedule(
      () =>
        this.emitFrame("ToolCallRequested", {
          id: TOOL_IDS[0],
          name: "ReadFile",
          arguments: { path: "tests/events.rs", start_line: 1, end_line: 120 },
          requires_approval: false,
        }),
      1800,
    );
    this.schedule(
      () =>
        this.emitFrame("ToolCallExecuted", {
          id: TOOL_IDS[0],
          output: [
            "use lcode::event::{AgentEvent, EventBus};",
            "",
            "#[test]",
            "fn event_order_is_consistent() {",
            "    let bus = EventBus::new();",
            "    let mut rx = bus.subscribe();",
            "    bus.broadcast(AgentEvent::SessionStarted { task: \"t\".into() });",
            "    bus.broadcast(AgentEvent::TurnStarted { turn: 1 });",
            "    // ...",
            "}",
            "",
            "#[test]",
            "fn burst_emission_keeps_order() {",
            "    // flaky under load: panics ~1 in 20 runs",
            "}",
          ].join("\n"),
        }),
      2600,
    );
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content:
            "The `burst_emission_keeps_order` test subscribes to the bus and asserts order. The race is in `broadcast`:",
        }),
      3300,
    );
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content:
            "it dispatches through a channel without a sequencing lock, so bursts of events can be observed in a different order under load.",
        }),
      3700,
    );
    this.schedule(
      () =>
        this.emitFrame("ToolCallRequested", {
          id: TOOL_IDS[1],
          name: "Bash",
          arguments: { command: "cargo test --test events burst_emission_keeps_order 2>&1 | tail -25", cwd: "." },
          requires_approval: false,
        }),
      4200,
    );
    this.schedule(
      () =>
        this.emitFrame("ToolCallExecuted", {
          id: TOOL_IDS[1],
          output: [
            "running 1 test",
            "thread 'burst_emission_keeps_order' panicked at tests/events.rs:34:5:",
            "  assertion failed: got TurnStarted before SessionStarted",
            "test result: FAILED. 1 failed; 0 passed",
            "",
            "note: reproduced 5/5 under `--release -- --test-threads=8`",
          ].join("\n"),
        }),
      5600,
    );
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content: "Reproduced consistently. I'll add a sequencing lock around `broadcast` and re-run.",
        }),
      6300,
    );
    this.schedule(() => this.emitFrame("TurnFinished", { turn: 1 }), 6800);

    // Turn 2 — apply the fix
    this.schedule(() => this.emitFrame("TurnStarted", { turn: 2 }), 7300);
    this.schedule(() => this.emitFrame("TextGenerated", { content: "Applying the fix now." }), 7800);
    this.schedule(
      () =>
        this.emitFrame("ToolCallRequested", {
          id: TOOL_IDS[2],
          name: "Edit",
          arguments: {
            path: "src/event.rs",
            old_string: "pub fn broadcast(&self, ev: AgentEvent) {",
            new_string: "pub fn broadcast(&self, ev: AgentEvent) {\n    let _guard = self.ordering.lock().unwrap();",
          },
          requires_approval: false,
        }),
      8200,
    );
    if (Math.random() < 0.25) {
      // Show a failure + retry every ~1 in 4 sessions.
      this.schedule(
        () =>
          this.emitFrame("ToolCallFailed", {
            id: TOOL_IDS[2],
            error: "edit conflict: old_string not found near line 41 (file was reformatted)",
          }),
        9200,
      );
      this.schedule(
        () =>
          this.emitFrame("TextGenerated", {
            content: "The edit conflicted — the file was reformatted. Retrying with the exact snippet.",
          }),
        9700,
      );
      this.schedule(
        () =>
          this.emitFrame("ToolCallRequested", {
            id: TOOL_IDS[3],
            name: "Edit",
            arguments: {
              path: "src/event.rs",
              old_string: "fn broadcast(&self, ev: AgentEvent) {",
              new_string: "fn broadcast(&self, ev: AgentEvent) {\n    let _guard = self.ordering.lock().unwrap();",
            },
            requires_approval: false,
          }),
        10200,
      );
      this.schedule(
        () => this.emitFrame("ToolCallExecuted", { id: TOOL_IDS[3], output: "patched src/event.rs (+2 lines)" }),
        11200,
      );
    } else {
      this.schedule(
        () => this.emitFrame("ToolCallExecuted", { id: TOOL_IDS[2], output: "patched src/event.rs (+2 lines)" }),
        9200,
      );
    }
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content: "Now re-running the full test suite to confirm nothing else broke.",
        }),
      12000,
    );
    this.schedule(
      () =>
        this.emitFrame("ToolCallRequested", {
          id: TOOL_IDS[4],
          name: "Bash",
          arguments: { command: "cargo test --test events 2>&1 | tail -15", cwd: "." },
          requires_approval: false,
        }),
      12500,
    );
    this.schedule(
      () =>
        this.emitFrame("ToolCallExecuted", {
          id: TOOL_IDS[4],
          output: [
            "running 12 tests",
            "test burst_emission_keeps_order ... ok",
            "test event_order_is_consistent ... ok",
            "test session_lifecycle ... ok",
            "test result: ok. 12 passed; 0 failed",
            "   Finished in 1.42s",
          ].join("\n"),
        }),
      13900,
    );
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content: "All tests pass. The ordering lock fixes the race without changing the public API.",
        }),
      14600,
    );
    this.schedule(
      () =>
        this.emitFrame("TextGenerated", {
          content: "Updating the CHANGELOG to note the fix.",
        }),
      15000,
    );
    this.schedule(() => this.emitFrame("TurnFinished", { turn: 2 }), 15500);
    this.schedule(
      () =>
        this.emitFrame("TaskFinished", {
          turns: 2,
          prompt_tokens: 15_000 + Math.floor(Math.random() * 6000),
          completion_tokens: 2_400 + Math.floor(Math.random() * 900),
        }),
      16000,
    );

    if (this.opts.loop !== false) {
      this.schedule(() => this.runSession(), 17200);
    }
  }

  private emitFrame(type: string, fields: Record<string, unknown>): void {
    this.seq += 1;
    this.emit(this.frame(type, fields));
  }
}
