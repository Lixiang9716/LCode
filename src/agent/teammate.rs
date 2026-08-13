//! Teammate agent loops (learn-claude-code s15/s17): each teammate runs
//! its own LLM agent loop — a bash/read_file/write_file/send_message tool
//! subset plus task board tools — in a WORK/IDLE cycle.
//!
//! WORK (s15): drain the inbox, dispatch protocol messages (s16), then run
//! LLM turns until the model stops asking for tools. IDLE (s17): poll the
//! inbox and the task board; auto-claim the first unclaimed task; after a
//! 60s timeout send a summary to the lead and shut down. Identity is
//! re-injected when the message list shrank (context compression, s17).
//!
//! The loop runs as a tokio task (dropped automatically when the runtime
//! drops, mirroring s15's daemon threads). When no LLM provider is
//! configured the loop falls back to the basic echo loop (no LLM).

use crate::agent::protocol::{dispatch_message, DispatchAction, ProtocolManager};
use crate::agent::task::{Task, TaskClaimTool, TaskListTool, TaskManager};
use crate::agent::team::{
    set_member_state, MessageBus, TeamMessage, TeamTool, TeamToolKind, TeammateManager,
    TeammateState, TEAMMATE_IDLE_INTERVAL, TEAMMATE_IDLE_POLLS, TEAMMATE_WORK_TURNS,
};
use crate::llm::{ChatMessage, LlmProvider, Role};
use crate::tools::{Tool, ToolResult};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// End of a WORK phase: the loop either shuts down or goes IDLE.
#[derive(Debug, Clone, Copy, PartialEq)]
enum WorkEnd {
    Shutdown,
    Idle,
}

/// End of an IDLE phase: new work arrived, shut down, or timed out.
#[derive(Debug, Clone, Copy, PartialEq)]
enum IdleEnd {
    Shutdown,
    Work,
    Timeout,
}

/// Shared runtime environment for a teammate loop.
#[derive(Clone)]
pub struct TeammateEnv {
    pub team_dir: PathBuf,
    pub bus: Arc<MessageBus>,
    pub provider: Option<Arc<dyn LlmProvider>>,
    pub protocol: Arc<ProtocolManager>,
    pub tasks: Arc<Mutex<TaskManager>>,
    pub tools: TeammateTools,
    /// Poll interval for the IDLE phase (default 5s).
    pub idle_interval: std::time::Duration,
    /// Empty IDLE polls before auto-shutdown.
    pub idle_polls: u32,
}

impl TeammateEnv {
    /// Default environment for provider-less callers (basic echo loop).
    pub(crate) fn basic(team_dir: &Path) -> Self {
        let workspace = team_dir.parent().map(Path::to_path_buf).unwrap_or_default();
        let bus = Arc::new(MessageBus::from_inbox(team_dir.join("inbox")));
        let protocol = Arc::new(ProtocolManager::default());
        let tasks = Arc::new(Mutex::new(TaskManager::new(&workspace)));
        let manager = Arc::new(Mutex::new(TeammateManager::default()));
        Self {
            team_dir: team_dir.to_path_buf(),
            tools: TeammateTools::new(
                &workspace,
                manager,
                bus.clone(),
                protocol.clone(),
                tasks.clone(),
            ),
            bus,
            provider: None,
            protocol,
            tasks,
            idle_interval: TEAMMATE_IDLE_INTERVAL,
            idle_polls: TEAMMATE_IDLE_POLLS,
        }
    }
}

/// The teammate tool subset (s15): bash, read_file, write_file,
/// send_message, submit_plan, task_list, task_claim — dispatched directly
/// without a full [`crate::tools::ToolRegistry`].
#[derive(Clone)]
pub struct TeammateTools {
    workspace: PathBuf,
    manager: Arc<Mutex<TeammateManager>>,
    bus: Arc<MessageBus>,
    protocol: Arc<ProtocolManager>,
    tasks: Arc<Mutex<TaskManager>>,
}

impl TeammateTools {
    pub fn new(
        workspace: &Path,
        manager: Arc<Mutex<TeammateManager>>,
        bus: Arc<MessageBus>,
        protocol: Arc<ProtocolManager>,
        tasks: Arc<Mutex<TaskManager>>,
    ) -> Self {
        Self { workspace: workspace.to_path_buf(), manager, bus, protocol, tasks }
    }

    /// Tool definitions exposed to the teammate's LLM.
    pub fn definitions(&self) -> Vec<crate::llm::ToolDefinition> {
        let mut defs = Vec::with_capacity(7);
        let cmd = serde_json::json!({ "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] });
        defs.push(teammate_tool("bash", "Run a shell command in the workspace.", cmd));
        defs.push(teammate_tool(
            "read_file", "Read a file with line numbers.",
            serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" }, "limit": { "type": "integer" } }, "required": ["path"] }),
        ));
        defs.push(teammate_tool(
            "write_file", "Write content to a file.",
            serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] }),
        ));
        defs.push(teammate_tool(
            "send_message", "Send a message to another agent's inbox.",
            serde_json::json!({ "type": "object", "properties": { "to": { "type": "string" }, "msg_type": { "type": "string" }, "request_id": { "type": "string" }, "content": { "type": "string" } }, "required": ["to", "content"] }),
        ));
        defs.push(teammate_tool(
            "submit_plan", "Submit a plan to the lead for approval.",
            serde_json::json!({ "type": "object", "properties": { "plan": { "type": "string" } }, "required": ["plan"] }),
        ));
        defs.push(teammate_tool(
            "task_list",
            "List the task board.",
            serde_json::json!({ "type": "object", "properties": {} }),
        ));
        defs.push(teammate_tool(
            "task_claim", "Claim a pending task for yourself.",
            serde_json::json!({ "type": "object", "properties": { "id": { "type": "integer" }, "owner": { "type": "string" } }, "required": ["id"] }),
        ));
        defs
    }

    /// Execute a teammate tool by name.
    pub fn execute(&self, name: &str, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let send = TeamTool {
            kind: TeamToolKind::Send,
            manager: self.manager.clone(),
            bus: self.bus.clone(),
            protocol: self.protocol.clone(),
            env: None,
        };
        match name {
            "bash" => {
                crate::tools::shell::ShellTool::new_with_root(self.workspace.clone()).execute(args)
            }
            "read_file" => crate::tools::file::ReadFileTool::new_with_root(self.workspace.clone())
                .execute(args),
            "write_file" => {
                crate::tools::file::WriteFileTool::new_with_root(self.workspace.clone())
                    .execute(args)
            }
            "send_message" => send.execute(args),
            "submit_plan" => {
                let submit = crate::agent::protocol::SubmitPlanTool {
                    bus: self.bus.clone(),
                    protocol: self.protocol.clone(),
                };
                submit.execute(args)
            }
            "task_list" => TaskListTool { manager: self.tasks.clone() }.execute(args),
            "task_claim" => TaskClaimTool { manager: self.tasks.clone() }.execute(args),
            other => anyhow::bail!("Unknown teammate tool: {other}"),
        }
    }
}

/// Build one teammate tool definition.
fn teammate_tool(
    name: &str,
    description: &str,
    parameters: serde_json::Value,
) -> crate::llm::ToolDefinition {
    crate::llm::ToolDefinition {
        tool_type: "function".to_string(),
        function: crate::llm::FunctionDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}

/// Run a teammate loop (WORK/IDLE cycle; basic echo loop without an
/// LLM provider).
pub async fn run_teammate_loop(name: String, role: String, env: TeammateEnv) {
    if env.provider.is_none() {
        run_teammate_loop_basic(name, env.team_dir.clone(), env.bus.clone()).await;
        return;
    }
    let system = format!(
        "You are '{name}', a {role}. Use tools to complete tasks, list and \
         claim tasks from the board, check inbox for protocol messages, \
         and send results to 'lead' via send_message."
    );
    let user = format!(
        "You are '{name}', a {role}. Check your inbox and the task board, \
         then work until asked to shut down."
    );
    let mut messages: Vec<ChatMessage> = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let mut last_len = messages.len();
    let mut wake = true;
    let mut total_usage = crate::llm::Usage::default();
    loop {
        reinject_identity(&mut messages, &name, &role, last_len);
        last_len = messages.len();
        set_member_state(&env.team_dir, &name, TeammateState::Working);
        let worked = work_phase(&name, &env, &mut messages, wake, &mut total_usage).await;
        crate::agent::usage_tracking::record_agent_usage(&env.team_dir, &name, &total_usage);
        if matches!(worked, WorkEnd::Shutdown) {
            break;
        }
        wake = match idle_phase(&name, &env, &mut messages).await {
            IdleEnd::Work => true,
            IdleEnd::Shutdown | IdleEnd::Timeout => break,
        };
    }
    crate::agent::usage_tracking::record_agent_usage(&env.team_dir, &name, &total_usage);
    set_member_state(&env.team_dir, &name, TeammateState::Shutdown);
}

/// WORK phase (s15): drain the inbox, dispatch protocol messages, then
/// run LLM turns until the model stops asking for tools.
async fn work_phase(
    name: &str,
    env: &TeammateEnv,
    messages: &mut Vec<ChatMessage>,
    mut wake: bool,
    total_usage: &mut crate::llm::Usage,
) -> WorkEnd {
    let mut worked = false;
    for _ in 0..TEAMMATE_WORK_TURNS {
        if env.bus.is_shutdown() {
            return WorkEnd::Shutdown;
        }
        let (shutdown, injected) = inject_inbox(name, env, messages);
        if shutdown {
            return WorkEnd::Shutdown;
        }
        if !injected && !worked && !wake {
            return WorkEnd::Idle;
        }
        let (did_work, usage) = teammate_llm_turn(name, env, messages).await;
        crate::agent::usage_tracking::accumulate_usage(total_usage, &usage);
        if did_work {
            worked = true;
            wake = false;
        } else {
            return WorkEnd::Idle;
        }
    }
    WorkEnd::Idle
}

/// IDLE phase (s17): poll the inbox and task board; after `idle_polls`
/// empty polls, send a summary to the lead and shut down.
async fn idle_phase(name: &str, env: &TeammateEnv, messages: &mut Vec<ChatMessage>) -> IdleEnd {
    let mut idle_polls: u32 = 0;
    loop {
        tokio::time::sleep(env.idle_interval).await;
        idle_polls += 1;
        if env.bus.is_shutdown() {
            return IdleEnd::Shutdown;
        }
        set_member_state(&env.team_dir, name, TeammateState::Idle);

        let (shutdown, injected) = inject_inbox(name, env, messages);
        if shutdown {
            return IdleEnd::Shutdown;
        }
        if injected {
            return IdleEnd::Work; // new inbox messages: back to WORK
        }
        if let Some(task) = claim_unclaimed(env, name) {
            messages.push(ChatMessage::user(format!(
                "<auto-claimed>Task {}: {}</auto-claimed>",
                task.id, task.title
            )));
            return IdleEnd::Work; // claimed task: back to WORK
        }
        if idle_polls >= env.idle_polls {
            send_idle_summary(name, env, messages);
            return IdleEnd::Timeout;
        }
    }
}

/// Drain the teammate's inbox: protocol messages are dispatched (s16);
/// plain messages are injected as `<inbox>` user turns. Returns
/// (shutdown?, injected?).
fn inject_inbox(name: &str, env: &TeammateEnv, messages: &mut Vec<ChatMessage>) -> (bool, bool) {
    let mut shutdown = false;
    let mut injected = false;
    for msg in env.bus.read_inbox(name) {
        match dispatch_message(name, &env.bus, &msg) {
            DispatchAction::Shutdown => shutdown = true,
            DispatchAction::PlanNote(note) => {
                messages.push(ChatMessage::user(note));
                injected = true;
            }
            DispatchAction::None => {
                let body = serde_json::to_string(&msg).unwrap_or_else(|_| "[]".to_string());
                messages.push(ChatMessage::user(format!("<inbox>{body}</inbox>")));
                injected = true;
            }
        }
    }
    (shutdown, injected)
}

/// Run one LLM turn: chat, record the assistant message, execute any tool
/// calls (results backfilled as tool messages). Returns true when the
/// model asked for tools (loop keeps working), false when it stopped.
async fn teammate_llm_turn(
    name: &str,
    env: &TeammateEnv,
    messages: &mut Vec<ChatMessage>,
) -> (bool, crate::llm::Usage) {
    let provider = match &env.provider {
        Some(provider) => provider.clone(),
        None => return (false, crate::llm::Usage::default()),
    };
    let response = match provider.chat(messages, &env.tools.definitions()).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(teammate = name, %error, "teammate LLM call failed");
            return (false, crate::llm::Usage::default());
        }
    };
    let usage = response.usage.clone();
    messages.push(ChatMessage {
        role: Role::Assistant,
        content: response.content.clone(),
        tool_call_id: None,
        tool_calls: response.tool_calls.clone(),
    });
    let Some(calls) = response.tool_calls else {
        return (false, usage); // stop_reason != tool_use -> IDLE
    };
    for call in calls {
        let mut args: serde_json::Value = serde_json::from_str(&call.function.arguments)
            .unwrap_or_else(|_| serde_json::json!({}));
        // The loop knows the teammate's identity; stamp it on outgoing
        // messages so tools attribute them without the model passing names.
        match call.function.name.as_str() {
            "send_message" | "submit_plan" => args["from"] = serde_json::json!(name),
            "task_claim" => args["owner"] = serde_json::json!(name),
            _ => {}
        }
        let output = match env.tools.execute(&call.function.name, &args) {
            Ok(result) => format!("{result}"),
            Err(error) => format!("Error: {error}"),
        };
        messages.push(ChatMessage::tool(output, call.id.clone()));
    }
    (true, usage)
}

/// Scan the task board for an unclaimed task and claim it atomically
/// (read-check-write under the manager lock) on behalf of `name` (s17).
fn claim_unclaimed(env: &TeammateEnv, name: &str) -> Option<Task> {
    let tasks = env.tasks.lock().ok()?;
    let next = tasks.scan_unclaimed().into_iter().next()?;
    tasks.claim(next.id, name).ok()
}

/// 60s idle timeout (s11/s17): send the last assistant text — or a
/// default — to the lead, then shut down.
fn send_idle_summary(name: &str, env: &TeammateEnv, messages: &[ChatMessage]) {
    let summary = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant && !m.content.trim().is_empty())
        .map(|m| m.content.clone())
        .unwrap_or_else(|| "Done.".to_string());
    let _ = env.bus.send(&TeamMessage {
        from: name.to_string(),
        to: "lead".to_string(),
        msg_type: "text".to_string(),
        request_id: None,
        content: summary,
    });
}

/// s17 identity re-injection: when the message list shrank (context
/// compression), re-insert the `<identity>` message.
pub fn reinject_identity(messages: &mut Vec<ChatMessage>, name: &str, role: &str, last_len: usize) {
    let shrank = messages.len() < last_len;
    let tiny = messages.len() <= 3;
    let present = messages.iter().any(|m| m.content.contains("<identity>"));
    if (shrank || tiny) && !present {
        messages.insert(
            1,
            ChatMessage::user(format!(
                "<identity>You are '{name}', role: {role}. Continue your work.</identity>"
            )),
        );
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
/// No LLM: kept as the fallback for provider-less callers (tests).
async fn run_teammate_loop_basic(name: String, team_dir: PathBuf, bus: Arc<MessageBus>) {
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
        let mut out = format!("[{name}] {text}");
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
                Ok(()) => format!("sent {msg_type} to {to}"),
                Err(e) => format!("send_message error: {e}"),
            }
        }
        Some("read_inbox") => {
            serde_json::to_string(&bus.read_inbox(name)).unwrap_or_else(|_| "[]".to_string())
        }
        _ => echo(&msg.content),
    }
}
