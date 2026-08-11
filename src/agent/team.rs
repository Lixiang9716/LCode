//! Agent teams and protocols (learn-claude-code s09-s11).
//!
//! Teammates each run their own loop, communicating through filesystem
//! JSONL mailboxes (append on send, drain on read), with an autonomous
//! WORK/IDLE cycle and a shutdown handshake keyed by request_id (s10).
//! Basic-version loop: no LLM — message read/write plus tool echo
//! (send_message, read_inbox); production injects a provider.

// The scaffold API takes `&PathBuf` (matching `register`'s skeleton
// signature); keep it, so silence the ptr_arg lint.
#![allow(clippy::ptr_arg)]
use crate::agent::event::AgentEvent;
use crate::tools::{Tool, ToolResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
/// Valid team message types.
pub const VALID_MSG_TYPES: &[&str] = &[
    "text",
    "request",
    "response",
    "shutdown_request",
    "shutdown_response",
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
}
impl MessageBus {
    pub fn new(workspace: &PathBuf) -> Self {
        Self { inbox_dir: workspace.join(".team").join("inbox"), events: None }
    }
    fn from_inbox(inbox_dir: PathBuf) -> Self {
        Self { inbox_dir, events: None }
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
/// Lifecycle state of a teammate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TeammateState {
    Spawning,
    Working,
    Idle,
    Shutdown,
}
impl TeammateState {
    /// Lowercase form used in config.json and the roster.
    pub fn as_str(self) -> &'static str {
        match self {
            TeammateState::Spawning => "spawning",
            TeammateState::Working => "working",
            TeammateState::Idle => "idle",
            TeammateState::Shutdown => "shutdown",
        }
    }
    fn parse(s: &str) -> TeammateState {
        match s {
            "spawning" => TeammateState::Spawning,
            "working" => TeammateState::Working,
            "idle" => TeammateState::Idle,
            _ => TeammateState::Shutdown,
        }
    }
}
impl Serialize for TeammateState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for TeammateState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(TeammateState::parse(&String::deserialize(deserializer)?))
    }
}
/// A teammate record (persisted in `.team/config.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Teammate {
    pub name: String,
    pub role: String,
    pub state: TeammateState,
}
/// Persisted roster file shape (`.team/config.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct TeamConfig {
    #[serde(default)]
    team_name: String,
    #[serde(default)]
    members: Vec<Teammate>,
}
/// Manages teammate lifecycle (spawn / reuse idle / shutdown).
#[derive(Debug, Default)]
pub struct TeammateManager {
    members: HashMap<String, Teammate>,
    team_dir: PathBuf,
    /// Runtime event bus publisher (skipped while `None`).
    events: Option<broadcast::Sender<AgentEvent>>,
}
impl TeammateManager {
    /// Create a manager rooted at `workspace/.team`, loading the roster
    /// from `.team/config.json` if it exists (disk is the source of truth).
    pub fn new(workspace: &PathBuf) -> Self {
        let mut manager = Self { team_dir: workspace.join(".team"), ..Self::default() };
        manager.reload();
        manager
    }
    /// Attach the runtime event bus (skipped while `None`).
    pub fn set_events(&mut self, events: broadcast::Sender<AgentEvent>) {
        self.events = Some(events);
    }
    fn config_path(&self) -> Option<PathBuf> {
        (!self.team_dir.as_os_str().is_empty()).then(|| self.team_dir.join("config.json"))
    }
    fn read_config(&self) -> Option<TeamConfig> {
        self.config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
    }
    /// Reload `members` from config.json (the loop persists changes there).
    fn reload(&mut self) {
        if let Some(config) = self.read_config() {
            self.members = config.members.into_iter().map(|m| (m.name.clone(), m)).collect();
        }
    }
    fn persist(&self) -> anyhow::Result<()> {
        let Some(path) = self.config_path() else { return Ok(()) };
        std::fs::create_dir_all(&self.team_dir)?;
        let config = TeamConfig {
            team_name: "default".to_string(),
            members: self.members.values().cloned().collect(),
        };
        std::fs::write(path, serde_json::to_string_pretty(&config)?)?;
        Ok(())
    }
    fn publish(&self, event: AgentEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event);
        }
    }
    /// Spawn (or reuse an idle) teammate with the given role. Registers
    /// the member in `.team/config.json` and starts the teammate loop
    /// (`run_teammate_loop`) as a daemon task when a tokio runtime
    /// exists; basic-version loop: no LLM.
    pub fn spawn(&mut self, name: &str, role: &str) -> anyhow::Result<Teammate> {
        if name.is_empty() {
            anyhow::bail!("Teammate name must not be empty");
        }
        self.reload();
        if let Some(state) = self.members.get(name).map(|m| m.state.as_str()) {
            if state == "working" || state == "spawning" {
                anyhow::bail!("'{}' is currently {}", name, state);
            }
        }
        let member =
            Teammate { name: name.into(), role: role.into(), state: TeammateState::Working };
        self.members.insert(member.name.clone(), member.clone());
        self.persist()?;
        self.publish(AgentEvent::TeammateStateChanged {
            name: member.name.clone(),
            state: member.state.as_str().to_string(),
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let bus = Arc::new(MessageBus::from_inbox(self.team_dir.join("inbox")));
            handle.spawn(run_teammate_loop(
                member.name.clone(),
                member.role.clone(),
                self.team_dir.clone(),
                bus,
            ));
        }
        Ok(member)
    }
    /// Current roster as text for the model.
    pub fn roster(&self) -> String {
        let mut lines: Vec<String> = self
            .read_config()
            .map(|config| config.members)
            .unwrap_or_else(|| self.members.values().cloned().collect())
            .iter()
            .map(|m| format!("- {} ({}): {}", m.name, m.role, m.state.as_str()))
            .collect();
        if lines.is_empty() {
            return "No teammates.".to_string();
        }
        lines.sort();
        lines.join("\n")
    }
}
/// Persist one member's state to config.json (the loop runs outside the manager's lock).
fn set_member_state(team_dir: &PathBuf, name: &str, state: TeammateState) {
    let path = team_dir.join("config.json");
    let mut config = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<TeamConfig>(&text).ok())
        .unwrap_or_default();
    if let Some(member) = config.members.iter_mut().find(|m| m.name == name) {
        member.state = state;
    } else {
        config.members.push(Teammate { name: name.to_string(), role: String::new(), state });
    }
    if let Ok(text) = serde_json::to_string_pretty(&config) {
        let _ = std::fs::create_dir_all(team_dir);
        let _ = std::fs::write(path, text);
    }
}

/// Basic teammate loop (s09-s11): WORK phase bounded to
/// [`TEAMMATE_WORK_TURNS`] turns; each turn drains the inbox. An empty
/// inbox enters IDLE, polling every [`TEAMMATE_IDLE_INTERVAL`] up to
/// [`TEAMMATE_IDLE_POLLS`] times before auto-shutdown (s11). A
/// `shutdown_request` is answered with a `shutdown_response` echoing
/// `request_id` (s10) and the loop exits; other messages go through
/// [`handle_teammate_message`] with the reply sent back as a `response`.
///
/// Basic version: no LLM calls; production injects a `LlmProvider`.
pub async fn run_teammate_loop(
    name: String,
    _role: String,
    team_dir: PathBuf,
    bus: Arc<MessageBus>,
) {
    let mut idle_polls: u32 = 0;
    let mut should_exit = false;
    for _ in 0..TEAMMATE_WORK_TURNS {
        let messages = bus.read_inbox(&name);
        if messages.is_empty() {
            set_member_state(&team_dir, &name, TeammateState::Idle);
            if idle_polls >= TEAMMATE_IDLE_POLLS {
                set_member_state(&team_dir, &name, TeammateState::Shutdown);
                return;
            }
            tokio::time::sleep(TEAMMATE_IDLE_INTERVAL).await;
            idle_polls += 1;
            continue;
        }
        idle_polls = 0;
        set_member_state(&team_dir, &name, TeammateState::Working);
        for msg in messages {
            if msg.msg_type == "shutdown_request" {
                let _ = bus.send(&TeamMessage {
                    from: name.clone(),
                    to: msg.from.clone(),
                    msg_type: "shutdown_response".to_string(),
                    request_id: msg.request_id.clone(),
                    content: "approved".to_string(),
                });
                should_exit = true;
                break;
            }
            let reply = handle_teammate_message(&bus, &name, &msg);
            let _ = bus.send(&TeamMessage {
                from: name.clone(),
                to: msg.from.clone(),
                msg_type: "response".to_string(),
                request_id: msg.request_id.clone(),
                content: reply,
            });
        }
        if should_exit {
            break;
        }
    }
    let final_state = if should_exit { TeammateState::Shutdown } else { TeammateState::Idle };
    set_member_state(&team_dir, &name, final_state);
}
/// Basic message handler: two mini-tools plus a plain echo. A JSON content
/// with `"tool":"send_message"` forwards a message; `"tool":"read_inbox"`
/// drains the teammate's own inbox; anything else echoes back. Returns the
/// reply text the loop sends to the message's sender.
pub fn handle_teammate_message(bus: &MessageBus, name: &str, msg: &TeamMessage) -> String {
    const ECHO_LIMIT: usize = 2000;
    let echo = |text: &str| -> String {
        let mut out = format!("[{}] {}", name, text);
        if out.len() > ECHO_LIMIT {
            out.truncate(ECHO_LIMIT);
            out.push_str("… (truncated)");
        }
        out
    };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&msg.content) else {
        return echo(&msg.content);
    };
    match payload.get("tool").and_then(|t| t.as_str()) {
        Some("send_message") => {
            let to = payload.get("to").and_then(|v| v.as_str()).unwrap_or_default();
            let content = payload.get("content").and_then(|v| v.as_str()).unwrap_or_default();
            let msg_type = payload.get("msg_type").and_then(|v| v.as_str()).unwrap_or("text");
            let sent = TeamMessage {
                from: name.to_string(),
                to: to.to_string(),
                msg_type: msg_type.to_string(),
                request_id: None,
                content: content.to_string(),
            };
            match bus.send(&sent) {
                Ok(()) => format!("sent {} to {}", msg_type, to),
                Err(e) => format!("send_message error: {}", e),
            }
        }
        Some("read_inbox") => {
            serde_json::to_string(&bus.read_inbox(name)).unwrap_or_else(|_| "[]".to_string())
        }
        _ => echo(&msg.content),
    }
}
// --- Tools -------------------------------------------------------------

/// Extract a required string argument from tool args.
fn arg<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    args[key].as_str().ok_or_else(|| anyhow::anyhow!("Missing '{}' argument", key))
}
/// The four team tools (`spawn_teammate`, `send_message`, `read_inbox`,
/// `list_teammates`), dispatched by [`TeamToolKind`].
pub struct TeamTool {
    pub kind: TeamToolKind,
    pub manager: Arc<Mutex<TeammateManager>>,
    pub bus: Arc<MessageBus>,
}
/// Which team tool an instance of [`TeamTool`] implements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TeamToolKind {
    Spawn,
    Send,
    Read,
    List,
}
impl Tool for TeamTool {
    fn name(&self) -> &str {
        match self.kind {
            TeamToolKind::Spawn => "spawn_teammate",
            TeamToolKind::Send => "send_message",
            TeamToolKind::Read => "read_inbox",
            TeamToolKind::List => "list_teammates",
        }
    }
    fn description(&self) -> &str {
        match self.kind {
            TeamToolKind::Spawn => {
                "Spawn a persistent teammate agent with a role. Teammates share \
                 the filesystem, have their own context, and read their inbox every turn."
            }
            TeamToolKind::Send => {
                "Send a message to a teammate's inbox. Types: text, request, response, \
                 shutdown_request, shutdown_response."
            }
            TeamToolKind::Read => "Read and drain your inbox.",
            TeamToolKind::List => "List the current team roster.",
        }
    }
    fn parameters(&self) -> serde_json::Value {
        match self.kind {
            TeamToolKind::Spawn => serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" }, "role": { "type": "string" } },
                "required": ["name", "role"]
            }),
            TeamToolKind::Send => serde_json::json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string" }, "to": { "type": "string" },
                    "msg_type": { "type": "string", "enum": VALID_MSG_TYPES },
                    "request_id": { "type": "string" }, "content": { "type": "string" }
                },
                "required": ["to", "content"]
            }),
            TeamToolKind::Read => serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": []
            }),
            TeamToolKind::List => serde_json::json!({ "type": "object", "properties": {} }),
        }
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        match self.kind {
            TeamToolKind::Spawn => {
                let name = arg(args, "name")?;
                let role = arg(args, "role")?;
                let mut manager = match self.manager.lock() {
                    Ok(m) => m,
                    Err(_) => return Ok(ToolResult::err("team manager lock poisoned")),
                };
                let spawned = match manager.spawn(name, role) {
                    Ok(member) => member,
                    Err(e) => return Ok(ToolResult::err(e.to_string())),
                };
                Ok(ToolResult::ok(format!("Spawned '{}' (role: {})", spawned.name, spawned.role)))
            }
            TeamToolKind::Send => {
                let to = arg(args, "to")?;
                let content = arg(args, "content")?;
                let msg = TeamMessage {
                    from: args["from"].as_str().unwrap_or("lead").to_string(),
                    to: to.to_string(),
                    msg_type: args["msg_type"].as_str().unwrap_or("text").to_string(),
                    request_id: args["request_id"].as_str().map(String::from),
                    content: content.to_string(),
                };
                match self.bus.send(&msg) {
                    Ok(()) => Ok(ToolResult::ok(format!("Sent {} to {}", msg.msg_type, to))),
                    Err(e) => Ok(ToolResult::err(e.to_string())),
                }
            }
            TeamToolKind::Read => {
                let messages = self.bus.read_inbox(args["name"].as_str().unwrap_or("lead"));
                if messages.is_empty() {
                    return Ok(ToolResult::ok("(no messages)"));
                }
                Ok(ToolResult::ok(format!(
                    "{} message(s):\n{}",
                    messages.len(),
                    serde_json::to_string_pretty(&messages)?
                )))
            }
            TeamToolKind::List => match self.manager.lock() {
                Ok(manager) => Ok(ToolResult::ok(manager.roster())),
                Err(_) => Ok(ToolResult::err("team manager lock poisoned")),
            },
        }
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let bus = Arc::new(MessageBus::new(workspace));
    let manager = Arc::new(Mutex::new(TeammateManager::new(workspace)));
    for kind in [TeamToolKind::Spawn, TeamToolKind::Send, TeamToolKind::Read, TeamToolKind::List] {
        registry.register(Box::new(TeamTool { kind, manager: manager.clone(), bus: bus.clone() }));
    }
}
