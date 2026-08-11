//! Agent runtime: event bus and command channel.
//!
//! The runtime decouples the agent loop from its observers:
//! - [`AgentEvent`]s are broadcast to any number of subscribers
//!   (REPL renderer, logger, tests, future UIs)
//! - [`AgentCommand`]s flow back from a single controller (approvals, abort)

use crate::agent::event::{AgentCommand, AgentEvent};
use tokio::sync::{broadcast, mpsc};

/// Outcome of an approval request.
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    Aborted,
}

/// The agent runtime owns the event bus and the command channel.
#[derive(Debug)]
pub struct AgentRuntime {
    events_tx: broadcast::Sender<AgentEvent>,
    commands_rx: mpsc::Receiver<AgentCommand>,
}

impl AgentRuntime {
    /// Create a new runtime along with an event subscription and the
    /// command sender for the controller.
    pub fn new() -> (Self, broadcast::Receiver<AgentEvent>, mpsc::Sender<AgentCommand>) {
        let (events_tx, events_rx) = broadcast::channel(256);
        let (commands_tx, commands_rx) = mpsc::channel(64);
        (Self { events_tx, commands_rx }, events_rx, commands_tx)
    }

    /// Publish an event to all subscribers. Subscribers that are too slow
    /// to keep up are dropped (lagged) — the agent loop never blocks on
    /// observation.
    pub fn publish(&self, event: AgentEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Wait for the controller's decision on a pending tool call.
    ///
    /// Messages for other tool calls are ignored; [`AgentCommand::Abort`]
    /// ends the wait with [`ApprovalDecision::Aborted`].
    pub async fn await_approval(&mut self, tool_call_id: &str) -> ApprovalDecision {
        loop {
            match self.commands_rx.recv().await {
                Some(AgentCommand::ApproveToolCall { id }) if id == tool_call_id => {
                    return ApprovalDecision::Approved;
                }
                Some(AgentCommand::RejectToolCall { id }) if id == tool_call_id => {
                    return ApprovalDecision::Rejected;
                }
                Some(AgentCommand::Abort) => return ApprovalDecision::Aborted,
                // Ignore commands addressed to other tool calls / sessions.
                _ => continue,
            }
        }
    }
}
