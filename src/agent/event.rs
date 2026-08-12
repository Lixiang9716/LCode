//! Agent event model.
//!
//! The agent runtime is event-driven: every observable step of an agent
//! session is published as an [`AgentEvent`] on the event bus, and control
//! flows back through [`AgentCommand`] messages (approvals, aborts).
//!
//! Subscribers (REPL, logging, tests, future UIs) consume the event stream
//! without coupling to the agent loop's internals.

use serde::Serialize;
use serde_json::Value;

/// Events published by the agent runtime during a session.
#[derive(Debug, Clone, Serialize)]
pub enum AgentEvent {
    /// A session starts with the given task description.
    SessionStarted { task: String },
    /// A new loop turn begins.
    TurnStarted { turn: u32 },
    /// The model produced assistant text.
    TextGenerated { content: String },
    /// A streaming token delta (typewriter feed). Streamed responses
    /// publish one event per delta; the full text is the concatenation
    /// of the deltas in arrival order.
    TextDelta { content: String },
    /// The model requested a tool call.
    ///
    /// When `requires_approval` is true, the runtime waits for an
    /// [`AgentCommand::ApproveToolCall`] / [`AgentCommand::RejectToolCall`]
    /// before executing.
    ToolCallRequested { id: String, name: String, arguments: Value, requires_approval: bool },
    /// A tool call executed successfully.
    ToolCallExecuted { id: String, output: String },
    /// A tool call failed to execute.
    ToolCallFailed { id: String, error: String },
    /// A tool call was rejected by the user.
    ToolCallDeclined { id: String },
    /// A loop turn finished.
    TurnFinished { turn: u32 },
    /// The task completed with the given turn count.
    TaskFinished { turns: u32, prompt_tokens: u32, completion_tokens: u32 },
    /// The task was aborted (user interrupt or max turns).
    TaskAborted { reason: String },
    /// An unrecoverable error occurred.
    Error { message: String },

    // --- Plan / todo tracking (s03) ---
    /// The model updated the todo list.
    TodoUpdated { items: Vec<crate::agent::todo::TodoItem> },
    /// The model has not updated todos for several turns; a reminder was
    /// injected into the tool result stream.
    TodoNag { turns_since_update: u32 },

    // --- Skill loading (s05) ---
    /// A skill was loaded into the context.
    SkillLoaded { name: String },

    // --- Context compaction (s06) ---
    /// The conversation was compacted; a transcript was written to disk.
    ContextCompacted { summary: String, transcript_path: String },

    // --- Subagents (s04) ---
    /// A subagent was spawned with the given prompt.
    SubagentSpawned { prompt: String },
    /// A subagent finished and returned its summary.
    SubagentCompleted { summary: String },

    // --- Background tasks (s08) ---
    /// A background command started.
    BackgroundTaskStarted { id: String, command: String },
    /// A background command finished.
    BackgroundTaskCompleted { id: String, status: String, output: String },

    // --- Task board (s07) ---
    /// A persistent task was created.
    TaskCreated { id: u32, title: String },
    /// A persistent task changed status.
    TaskUpdated { id: u32, status: String },

    // --- Team messaging (s09/s10) ---
    /// A message was sent between agents.
    TeamMessageSent { from: String, to: String, msg_type: String },
    /// A teammate changed lifecycle state.
    TeammateStateChanged { name: String, state: String },

    // --- Worktree isolation (s12) ---
    /// A worktree was created for a task.
    WorktreeCreated { name: String, task_id: u32 },
    /// A worktree was removed.
    WorktreeRemoved { name: String },
}

/// Control messages sent to the agent runtime.
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// Start running a task.
    RunTask { task: String, max_turns: u32 },
    /// Approve a pending tool call.
    ApproveToolCall { id: String },
    /// Reject a pending tool call.
    RejectToolCall { id: String },
    /// Abort the current session.
    Abort,
    /// Manually trigger context compaction with an optional focus.
    Compact { focus: Option<String> },
}
