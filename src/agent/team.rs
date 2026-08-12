//! Agent teams (learn-claude-code s09-s11, s15-s17).
//!
//! Teammates each run their own LLM agent loop (see [`crate::agent::teammate`]:
//! WORK/IDLE cycle, s15), communicating through filesystem JSONL mailboxes
//! (append on send, drain on read). Team protocols — `request_id`-keyed
//! shutdown / plan-approval handshakes — live in [`crate::agent::protocol`]
//! (s16); autonomous task claiming lives in [`crate::agent::task`] (s17).
//!
//! INTEGRATION POINT (main agent / executor): teammate replies land in the
//! lead's inbox (`{workspace}/.team/inbox/lead.jsonl`); the executor's
//! turn-start should drain it via [`MessageBus::read_inbox("lead")`] or
//! [`MessageBus::drain_lead_inbox`] (read + format side lives here;
//! executor wiring is owned by the executor batch).

// Scaffold parity: register takes `&PathBuf`.
#![allow(clippy::ptr_arg)]
use crate::agent::event::AgentEvent;
use crate::agent::protocol::{register as register_protocol_tools, ProtocolManager};
use crate::agent::task::TaskManager;
use crate::agent::teammate::{run_teammate_loop, TeammateEnv, TeammateTools};
use crate::llm::LlmProvider;
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
}
impl MessageBus {
    pub fn new(workspace: &PathBuf) -> Self {
        Self { inbox_dir: workspace.join(".team").join("inbox"), events: None }
    }
    pub(crate) fn from_inbox(inbox_dir: PathBuf) -> Self {
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
#[derive(Default)]
pub struct TeammateManager {
    members: HashMap<String, Teammate>,
    team_dir: PathBuf,
    /// Runtime event bus publisher (skipped while `None`).
    events: Option<broadcast::Sender<AgentEvent>>,
}
impl TeammateManager {
    /// Create a manager rooted at `workspace/.team` (roster loaded from
    /// `.team/config.json` when present; disk is the source of truth).
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
    /// Spawn (or reuse an idle) teammate with the given role; starts
    /// [`run_teammate_loop`] as a tokio task. `env` comes from the
    /// caller; the manager never owns it, so no cycle forms.
    pub fn spawn(
        &mut self,
        name: &str,
        role: &str,
        env: Option<&TeammateEnv>,
    ) -> anyhow::Result<Teammate> {
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
            let env = match env {
                Some(env) => env.clone(),
                None => TeammateEnv::basic(&self.team_dir),
            };
            handle.spawn(run_teammate_loop(member.name.clone(), member.role.clone(), env));
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
/// Persist one member's state to config.json (outside the manager lock).
pub(crate) fn set_member_state(team_dir: &PathBuf, name: &str, state: TeammateState) {
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

// --- Tools -------------------------------------------------------------

/// Extract a required string argument from tool args.
fn arg<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    args[key].as_str().ok_or_else(|| anyhow::anyhow!("Missing '{}' argument", key))
}
/// The four lead team tools (`spawn_teammate`, `send_message`,
/// `read_inbox`, `list_teammates`), dispatched by [`TeamToolKind`].
/// Protocol tools live in [`crate::agent::protocol`].
pub struct TeamTool {
    pub kind: TeamToolKind,
    pub manager: Arc<Mutex<TeammateManager>>,
    pub bus: Arc<MessageBus>,
    pub protocol: Arc<ProtocolManager>,
    /// Runtime env for spawned loops (tool-owned, breaks the cycle).
    pub env: Option<TeammateEnv>,
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
                 shutdown_request, shutdown_response, plan_approval_request, \
                 plan_approval_response."
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
            TeamToolKind::Spawn => self.execute_spawn(args),
            TeamToolKind::Send => self.execute_send(args),
            TeamToolKind::Read => self.execute_read(args),
            TeamToolKind::List => self.execute_list(args),
        }
    }
}
impl TeamTool {
    fn execute_spawn(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = arg(args, "name")?;
        let role = arg(args, "role")?;
        let mut manager = match self.manager.lock() {
            Ok(m) => m,
            Err(_) => return Ok(ToolResult::err("team manager lock poisoned")),
        };
        let spawned = match manager.spawn(name, role, self.env.as_ref()) {
            Ok(member) => member,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        Ok(ToolResult::ok(format!("Spawned '{}' (role: {})", spawned.name, spawned.role)))
    }
    fn execute_send(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
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
    fn execute_read(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let messages = self.bus.read_inbox(args["name"].as_str().unwrap_or("lead"));
        // Route protocol responses (s16) before handing them to the model.
        for msg in &messages {
            if msg.request_id.is_some()
                && matches!(msg.msg_type.as_str(), "shutdown_response" | "plan_approval_response")
            {
                let _ = self.protocol.match_response(msg);
            }
        }
        if messages.is_empty() {
            return Ok(ToolResult::ok("(no messages)"));
        }
        Ok(ToolResult::ok(format!(
            "{} message(s):\n{}",
            messages.len(),
            serde_json::to_string_pretty(&messages)?
        )))
    }
    fn execute_list(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let _ = args;
        match self.manager.lock() {
            Ok(manager) => Ok(ToolResult::ok(manager.roster())),
            Err(_) => Ok(ToolResult::err("team manager lock poisoned")),
        }
    }
}

/// Register this module's tools (lead team tools + protocol tools) and
/// wire the teammate runtime environment (LLM provider, event bus,
/// protocol, task board) so spawned teammates run real agent loops.
///
/// INTEGRATION POINT (main agent / executor): teammate messages land in
/// `{workspace}/.team/inbox/lead.jsonl`; at turn-start the executor should
/// drain it and inject the text into the conversation (`MessageBus::read_inbox`
/// or [`MessageBus::drain_lead_inbox`]). Executor wiring is owned by the
/// executor batch; the read + format side is implemented here.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    workspace: &PathBuf,
    provider: Arc<dyn LlmProvider>,
    events: Option<broadcast::Sender<AgentEvent>>,
) {
    let mut bus = MessageBus::new(workspace);
    if let Some(tx) = &events {
        bus.set_events(tx.clone());
    }
    let bus = Arc::new(bus);
    let protocol = Arc::new(ProtocolManager::default());
    let tasks = Arc::new(Mutex::new(TaskManager::new(workspace)));
    let manager = Arc::new(Mutex::new(TeammateManager::new(workspace)));
    {
        let mut guard = match manager.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(tx) = &events {
            guard.set_events(tx.clone());
        }
    }
    // Owned by the tools, not the manager (they hold a strong `Arc` of
    // it); the cycle would leak the event-bus sender past session end.
    let env = TeammateEnv {
        team_dir: workspace.join(".team"),
        bus: bus.clone(),
        provider: Some(provider),
        protocol: protocol.clone(),
        tasks: tasks.clone(),
        tools: TeammateTools::new(
            workspace,
            manager.clone(),
            bus.clone(),
            protocol.clone(),
            tasks.clone(),
        ),
        idle_interval: TEAMMATE_IDLE_INTERVAL,
        idle_polls: TEAMMATE_IDLE_POLLS,
    };
    for kind in [TeamToolKind::Spawn, TeamToolKind::Send, TeamToolKind::Read, TeamToolKind::List] {
        registry.register(Box::new(TeamTool {
            kind,
            manager: manager.clone(),
            bus: bus.clone(),
            protocol: protocol.clone(),
            env: Some(env.clone()),
        }));
    }
    register_protocol_tools(registry, bus, protocol);
}
