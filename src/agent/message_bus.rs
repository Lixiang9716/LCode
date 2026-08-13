//! Team messaging bus: filesystem JSONL mailboxes between agents.
//!
//! `{to}.jsonl` files under `.team/inbox/`, append on send, drain on
//! read. Carries the session-end shutdown signal that teammate loops
//! poll so the process exits promptly after the lead session ends.
#![allow(clippy::ptr_arg)]

use crate::agent::event::AgentEvent;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use tokio::sync::broadcast;

/// Valid team message types.
pub const VALID_MSG_TYPES: &[&str] = &[
    "text",
    "request",
    "response",
    "shutdown_request",
    "shutdown_response",
    "plan_approval_request",
    "plan_approval_response",
];
/// Loop constants: bounded WORK turns; IDLE polls every 5s up to 12 (60s).
pub const TEAMMATE_WORK_TURNS: u32 = 50;
pub const TEAMMATE_IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
pub const TEAMMATE_IDLE_POLLS: u32 = 12;
/// A team message (one JSONL line in a `{to}.jsonl` inbox).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Runtime event bus publisher (skipped while `None`).
    events: Option<broadcast::Sender<AgentEvent>>,
    /// Session-end signal: teammate loops poll it and exit early so the
    /// event bus closes and the process does not hang after the lead
    /// session ends.
    shutdown: tokio::sync::watch::Sender<bool>,
}
impl MessageBus {
    pub fn new(workspace: &PathBuf) -> Self {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Self { inbox_dir: workspace.join(".team").join("inbox"), events: None, shutdown }
    }
    pub(crate) fn from_inbox(inbox_dir: PathBuf) -> Self {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Self { inbox_dir, events: None, shutdown }
    }
    /// Signal all teammate loops to wind down (session end).
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
    /// Has the session asked teammate loops to stop?
    pub fn is_shutdown(&self) -> bool {
        *self.shutdown.borrow()
    }
    /// Attach the runtime event bus so sends publish `TeamMessageSent`.
    pub fn set_events(&mut self, events: broadcast::Sender<AgentEvent>) {
        self.events = Some(events);
    }
    fn inbox_path(&self, name: &str) -> PathBuf {
        self.inbox_dir.join(format!("{}.jsonl", name))
    }
    /// Append a message to the recipient's inbox (type-whitelisted).
    pub fn send(&self, msg: &TeamMessage) -> anyhow::Result<()> {
        if !VALID_MSG_TYPES.contains(&msg.msg_type.as_str()) {
            anyhow::bail!("Invalid message type '{}'", msg.msg_type);
        }
        std::fs::create_dir_all(&self.inbox_dir)?;
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(self.inbox_path(&msg.to))?;
        writeln!(file, "{}", serde_json::to_string(msg)?)?;
        if let Some(tx) = &self.events {
            let _ = tx.send(AgentEvent::TeamMessageSent {
                from: msg.from.clone(),
                to: msg.to.clone(),
                msg_type: msg.msg_type.clone(),
            });
        }
        Ok(())
    }
    /// Read all messages and clear the inbox (drain-on-read).
    pub fn read_inbox(&self, name: &str) -> Vec<TeamMessage> {
        let Ok(text) = std::fs::read_to_string(self.inbox_path(name)) else { return Vec::new() };
        let messages: Vec<TeamMessage> =
            text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
        let _ = std::fs::write(self.inbox_path(name), "");
        messages
    }
    /// Drain the lead's inbox and render it for injection into the main
    /// agent conversation (executor integration point — module docs).
    pub fn drain_lead_inbox(&self) -> (Vec<TeamMessage>, String) {
        let messages = self.read_inbox("lead");
        let text = if messages.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = messages
                .iter()
                .map(|m| format!("From {} [{}]: {}", m.from, m.msg_type, m.content))
                .collect();
            format!("[Inbox]\n{}", lines.join("\n"))
        };
        (messages, text)
    }
    /// Broadcast to everyone except the sender.
    pub fn broadcast(
        &self,
        sender: &str,
        members: &[String],
        msg: &TeamMessage,
    ) -> anyhow::Result<()> {
        for member in members {
            if member != sender {
                let mut m = msg.clone();
                m.to = member.clone();
                self.send(&m)?;
            }
        }
        Ok(())
    }
}
