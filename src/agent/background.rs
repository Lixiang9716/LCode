//! Background tasks (learn-claude-code s08).
//!
//! Long-running commands run as spawned tokio tasks; completion
//! notifications are drained before each LLM call and injected into the
//! context, so the loop stays single-threaded and deterministic.

use crate::agent::event::AgentEvent;
use crate::config::Config;
use crate::tools::shell::ShellTool;
use crate::tools::{Tool, ToolResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// Length of the short task id returned to the caller.
const ID_LEN: usize = 8;
/// Default timeout for background commands, in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Maximum length of the stored result.
const MAX_RESULT_CHARS: usize = 50_000;
/// Command preview length in completion notifications.
const NOTIF_COMMAND_CHARS: usize = 80;
/// Result preview length in completion notifications.
const NOTIF_RESULT_CHARS: usize = 500;
/// Command preview length in `check` listings.
const CHECK_COMMAND_CHARS: usize = 60;

/// Truncate a string to at most `max_chars` characters, keeping only
/// whole chars (no partial UTF-8 sequences).
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    let boundary = s.char_indices().nth(max_chars).map(|(i, _)| i).unwrap_or(s.len());
    &s[..boundary]
}

/// Status of a background task.
#[derive(Debug, Clone, PartialEq)]
pub enum BackgroundStatus {
    Running,
    Completed,
    Timeout,
    Error,
}

impl BackgroundStatus {
    fn label(&self) -> &'static str {
        match self {
            BackgroundStatus::Running => "running",
            BackgroundStatus::Completed => "completed",
            BackgroundStatus::Timeout => "timeout",
            BackgroundStatus::Error => "error",
        }
    }
}

/// A background task record.
#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub command: String,
    pub status: BackgroundStatus,
    /// Full result (truncated to 50k); the notification carries only the
    /// first 500 chars as a "ping".
    pub result: String,
}

/// Manages background tasks and their completion notifications.
#[derive(Debug, Default)]
pub struct BackgroundManager {
    tasks: Mutex<HashMap<String, BackgroundTask>>,
    /// Pending notifications drained before each LLM call.
    notifications: Mutex<Vec<String>>,
    /// Session event bus; lifecycle events are published when present.
    events: Option<broadcast::Sender<AgentEvent>>,
    /// Shell safety policy used to validate commands before spawning.
    shell: Option<ShellTool>,
}

impl BackgroundManager {
    /// Create a manager whose command validation uses the shell safety
    /// policy from `config` (deny list + destructive-pattern checks).
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            tasks: Mutex::new(HashMap::new()),
            notifications: Mutex::new(Vec::new()),
            events: None,
            shell: Some(ShellTool::new(config)?),
        })
    }

    /// Attach the session event bus so task lifecycle events are published.
    pub fn with_events(mut self, events: broadcast::Sender<AgentEvent>) -> Self {
        self.events = Some(events);
        self
    }

    /// Spawn a command in the background; returns the task id immediately.
    ///
    /// The command is validated against the shell safety policy before
    /// spawning, and spawning requires a tokio runtime context (the agent
    /// loop). The spawned task runs with a timeout and, on completion,
    /// pushes a "{id} [{status}] {command} {result}" notification and
    /// publishes [`AgentEvent::BackgroundTaskCompleted`].
    pub fn spawn(self: &Arc<Self>, command: &str, timeout_secs: u64) -> anyhow::Result<String> {
        self.safety_check(command)?;
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("background_run requires a tokio runtime context"))?;

        let id = task_id();
        self.tasks.lock().unwrap().insert(
            id.clone(),
            BackgroundTask {
                id: id.clone(),
                command: command.to_string(),
                status: BackgroundStatus::Running,
                result: String::new(),
            },
        );
        self.publish(AgentEvent::BackgroundTaskStarted {
            id: id.clone(),
            command: command.to_string(),
        });

        let me = Arc::clone(self);
        let command = command.to_string();
        let spawned_id = id.clone();
        handle.spawn(async move {
            let (status, result) = run_command(&command, timeout_secs).await;
            me.complete(spawned_id, command, status, result);
        });

        Ok(id)
    }

    /// Drain pending notifications ("ping" summaries).
    pub fn drain_notifications(&self) -> Vec<String> {
        let mut queue = self.notifications.lock().unwrap();
        std::mem::take(&mut *queue)
    }

    /// Full result of a task (or all tasks when id is empty).
    pub fn check(&self, id: Option<&str>) -> String {
        let tasks = self.tasks.lock().unwrap();
        match id {
            Some(id) => match tasks.get(id) {
                Some(task) => format!(
                    "[{}] {}\n{}",
                    task.status.label(),
                    truncate_chars(&task.command, CHECK_COMMAND_CHARS),
                    task_result_display(task),
                ),
                None => format!("Error: Unknown task {id}"),
            },
            None => list_tasks(&tasks),
        }
    }

    /// Validate a command against the shell safety policy.
    fn safety_check(&self, command: &str) -> anyhow::Result<()> {
        match &self.shell {
            Some(shell) => shell.check_safety(command),
            None => fallback_safety_check(command),
        }
    }

    /// Record completion: update the task, push a notification, publish
    /// the completed event. Runs inside the spawned task.
    fn complete(&self, id: String, command: String, status: BackgroundStatus, result: String) {
        if let Some(task) = self.tasks.lock().unwrap().get_mut(&id) {
            task.status = status.clone();
            task.result = result.clone();
        }
        let label = status.label();
        let cmd = truncate_chars(&command, NOTIF_COMMAND_CHARS);
        let res = truncate_chars(&result, NOTIF_RESULT_CHARS);
        let notification = format!("{id} [{label}] {cmd}\n{res}");
        self.notifications.lock().unwrap().push(notification);
        self.publish(AgentEvent::BackgroundTaskCompleted {
            id,
            status: label.to_string(),
            output: res.to_string(),
        });
    }

    /// Publish an event on the attached bus, if any.
    fn publish(&self, event: AgentEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event);
        }
    }
}

/// Human-readable result of a task: "(running)" while in flight.
fn task_result_display(task: &BackgroundTask) -> String {
    if task.status == BackgroundStatus::Running {
        return "(running)".to_string();
    }
    if task.result.is_empty() {
        return "(no output)".to_string();
    }
    task.result.clone()
}

/// One-line listing of every task, sorted by id.
fn list_tasks(tasks: &HashMap<String, BackgroundTask>) -> String {
    if tasks.is_empty() {
        return "No background tasks.".to_string();
    }
    let mut lines: Vec<String> = tasks
        .iter()
        .map(|(tid, task)| {
            format!(
                "{tid}: [{}] {}",
                task.status.label(),
                truncate_chars(&task.command, CHECK_COMMAND_CHARS),
            )
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

/// Safety fallback for a default-constructed manager (no config): the
/// built-in destructive patterns from `ShellTool` plus the config default
/// deny list, so even a `BackgroundManager::default()` refuses the
/// classics.
fn fallback_safety_check(command: &str) -> anyhow::Result<()> {
    let lower = command.trim().to_lowercase();
    const DENIED: [&str; 10] = [
        "rm -rf /",
        "sudo",
        "chmod 777",
        "mkfs.",
        "dd if=",
        "> /dev/sda",
        "shutdown",
        "reboot",
        "fork bomb",
        ":(){ :|:& };:",
    ];
    for denied in DENIED {
        if lower.contains(denied) {
            anyhow::bail!(
                "Command blocked: matches denied pattern '{}'. If this is intentional, run it manually.",
                denied
            );
        }
    }
    Ok(())
}

/// Execute a command with `sh -c`, capturing stdout + stderr (truncated
/// to 50k chars), with a timeout that kills the child.
async fn run_command(command: &str, timeout_secs: u64) -> (BackgroundStatus, String) {
    let mut child = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (BackgroundStatus::Error, format!("Error: {e}")),
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let collect = async {
        let (out, err) = tokio::join!(read_pipe(stdout), read_pipe(stderr));
        let status = child.wait().await;
        (status, out, err)
    };

    match tokio::time::timeout(Duration::from_secs(timeout_secs), collect).await {
        Ok((_status, out, err)) => {
            let stdout = String::from_utf8_lossy(&out);
            let stderr = String::from_utf8_lossy(&err);
            let text = format!("{stdout}{stderr}");
            let trimmed = text.trim();
            let result = if trimmed.is_empty() {
                "(no output)".to_string()
            } else {
                truncate_chars(trimmed, MAX_RESULT_CHARS).to_string()
            };
            (BackgroundStatus::Completed, result)
        }
        Err(_elapsed) => {
            child.kill().await.ok();
            let _ = child.wait().await;
            (BackgroundStatus::Timeout, format!("Error: Timeout ({timeout_secs}s)"))
        }
    }
}

/// Read an optionally-present pipe to the end.
async fn read_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut pipe) = pipe {
        use tokio::io::AsyncReadExt;
        let _ = pipe.read_to_end(&mut buf).await;
    }
    buf
}

/// Generate a short random task id (8 hex chars, like the tutorial's
/// `uuid4()[:8]`).
fn task_id() -> String {
    let mut id = uuid::Uuid::new_v4().simple().to_string();
    id.truncate(ID_LEN);
    id
}

/// Tool: `background_run` — start a command without blocking the loop.
pub struct BackgroundRunTool {
    pub manager: std::sync::Arc<BackgroundManager>,
}

impl Tool for BackgroundRunTool {
    fn name(&self) -> &str {
        "background_run"
    }

    fn description(&self) -> &str {
        "Start a long-running shell command in the background. Returns a \
         task id immediately; results arrive as <background-results> \
         before the next LLM turn. Check with background_check."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to run in the background"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 300, max: 300)"
                }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;
        let timeout_secs = args["timeout"].as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS).min(300);
        match self.manager.spawn(command, timeout_secs) {
            Ok(id) => Ok(ToolResult::ok(format!(
                "Background task {id} started: {}",
                truncate_chars(command, NOTIF_COMMAND_CHARS)
            ))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `background_check` — query full results.
pub struct BackgroundCheckTool {
    pub manager: std::sync::Arc<BackgroundManager>,
}

impl Tool for BackgroundCheckTool {
    fn name(&self) -> &str {
        "background_check"
    }

    fn description(&self) -> &str {
        "Get the full result of a background task by id, or all tasks \
         when no id is given."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Task id to check; omit to list all tasks"
                }
            },
            "required": []
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = args["id"].as_str();
        Ok(ToolResult::ok(self.manager.check(id)))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, manager: std::sync::Arc<BackgroundManager>) {
    registry.register(Box::new(BackgroundRunTool { manager: manager.clone() }));
    registry.register(Box::new(BackgroundCheckTool { manager }));
}
