//! Agent event model.
//!
//! The agent runtime is event-driven: every observable step of an agent
//! session is published as an [`AgentEvent`] on the event bus, and control
//! flows back through [`AgentCommand`] messages (approvals, aborts).
//!
//! Subscribers (REPL, logging, tests, future UIs) consume the event stream
//! without coupling to the agent loop's internals.

use serde_json::Value;

/// Events published by the agent runtime during a session.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A session starts with the given task description.
    SessionStarted { task: String },
    /// A new loop turn begins.
    TurnStarted { turn: u32 },
    /// The model produced assistant text.
    TextGenerated { content: String },
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
}
