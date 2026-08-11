//! Agent teams and protocols (learn-claude-code s09-s11).
//!
//! Persistent named teammates each run their own agent loop; they
//! communicate through filesystem JSONL mailboxes (drain-on-read). The
//! lead can spawn teammates, send/broadcast messages, and run an
//! autonomous WORK/IDLE cycle (scan → claim → work) with a shutdown
//! handshake keyed by request_id (s10).

use crate::tools::{Tool, ToolResult};
use std::collections::HashMap;
use std::path::PathBuf;

/// Valid team message types.
pub const VALID_MSG_TYPES: &[&str] =
    &["text", "request", "response", "shutdown_request", "shutdown_response", "plan_approval_response"];

/// A team message.
#[derive(Debug, Clone)]
pub struct TeamMessage {
    pub from: String,
    pub to: String,
    pub msg_type: String,
    pub request_id: Option<String>,
    pub content: String,
}

/// Filesystem message bus: `{to}.jsonl`, append on send, drain on read.
#[derive(Debug)]
pub struct MessageBus {
    inbox_dir: PathBuf,
}

impl MessageBus {
    pub fn new(workspace: &PathBuf) -> Self {
        Self { inbox_dir: workspace.join(".team").join("inbox") }
    }

    /// Append a message to the recipient's inbox (type-whitelisted).
    pub fn send(&self, msg: &TeamMessage) -> anyhow::Result<()> {
        // TODO(s09): validate msg_type, append JSONL line, emit TeamMessageSent.
        let _ = msg;
        Ok(())
    }

    /// Read all messages and clear the inbox (drain-on-read).
    pub fn read_inbox(&self, name: &str) -> Vec<TeamMessage> {
        // TODO(s09): read all lines, truncate the file, parse JSON.
        let _ = name;
        Vec::new()
    }

    /// Broadcast to everyone except the sender.
    pub fn broadcast(&self, sender: &str, members: &[String], msg: &TeamMessage) -> anyhow::Result<()> {
        // TODO(s09): send to each member != sender.
        let _ = (sender, members, msg);
        Ok(())
    }
}

/// Lifecycle state of a teammate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TeammateState {
    Spawning,
    Working,
    Idle,
    Shutdown,
}

/// A teammate record (persisted in `.team/config.json`).
#[derive(Debug, Clone)]
pub struct Teammate {
    pub name: String,
    pub role: String,
    pub state: TeammateState,
}

/// Manages teammate lifecycle (spawn / reuse idle / shutdown).
#[derive(Debug, Default)]
pub struct TeammateManager {
    members: HashMap<String, Teammate>,
}

impl TeammateManager {
    /// Spawn (or reuse an idle) teammate with the given role.
    pub fn spawn(&mut self, name: &str, role: &str) -> anyhow::Result<Teammate> {
        // TODO(s09): reuse idle member; else create + persist config.json
        // + start the teammate loop task. Emit TeammateStateChanged.
        let _ = (name, role);
        anyhow::bail!("team.spawn not implemented yet")
    }

    /// Current roster as text for the model.
    pub fn roster(&self) -> String {
        // TODO(s09): "- name (role): state".
        self.members
            .iter()
            .map(|(n, t)| format!("- {} ({}): {:?}", n, t.role, t.state))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// --- Tools -------------------------------------------------------------

/// Tool: `spawn_teammate` (lead only).
pub struct SpawnTeammateTool {
    pub manager: std::sync::Mutex<TeammateManager>,
}

impl Tool for SpawnTeammateTool {
    fn name(&self) -> &str {
        "spawn_teammate"
    }
    fn description(&self) -> &str {
        "Spawn a persistent teammate agent with a role. Teammates share \
         the filesystem, have their own context, and read their inbox \
         before every turn."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s09): { name: string, role: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("spawn_teammate not implemented yet"))
    }
}

/// Tool: `send_message`.
pub struct SendMessageTool {
    pub bus: std::sync::Arc<MessageBus>,
}

impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }
    fn description(&self) -> &str {
        "Send a message to a teammate's inbox. Types: text, request, \
         response, shutdown_request, shutdown_response."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s09): { to: string, msg_type: enum, content: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("send_message not implemented yet"))
    }
}

/// Tool: `read_inbox`.
pub struct ReadInboxTool {
    pub bus: std::sync::Arc<MessageBus>,
}

impl Tool for ReadInboxTool {
    fn name(&self) -> &str {
        "read_inbox"
    }
    fn description(&self) -> &str {
        "Read and drain your inbox."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("read_inbox not implemented yet"))
    }
}

/// Tool: `list_teammates`.
pub struct ListTeammatesTool {
    pub manager: std::sync::Mutex<TeammateManager>,
}

impl Tool for ListTeammatesTool {
    fn name(&self) -> &str {
        "list_teammates"
    }
    fn description(&self) -> &str {
        "List the current team roster."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("list_teammates not implemented yet"))
    }
}

// TODO(s10): shutdown_request / shutdown_response / plan_approval tools —
// request_id keyed FSM reusing AgentCommand-style correlation.
// TODO(s11): autonomous loop — WORK phase bounded 50 turns, IDLE phase
// polls inbox + task board every 5s (max 60s), claims unclaimed tasks,
// auto-shutdown on idle timeout; identity re-injection after compaction.

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let bus = std::sync::Arc::new(MessageBus::new(workspace));
    let manager = std::sync::Mutex::new(TeammateManager::default());
    registry.register(Box::new(SpawnTeammateTool { manager: std::sync::Mutex::new(TeammateManager::default()) }));
    registry.register(Box::new(SendMessageTool { bus: bus.clone() }));
    registry.register(Box::new(ReadInboxTool { bus: bus.clone() }));
    registry.register(Box::new(ListTeammatesTool { manager }));
}
