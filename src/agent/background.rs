//! Background tasks (learn-claude-code s08).
//!
//! Long-running commands run as spawned tasks; completion notifications
//! are drained before each LLM call and injected into the context, so the
//! loop stays single-threaded and deterministic.

use crate::tools::{Tool, ToolResult};
use std::collections::HashMap;
use std::sync::Mutex;

/// Status of a background task.
#[derive(Debug, Clone, PartialEq)]
pub enum BackgroundStatus {
    Running,
    Completed,
    Timeout,
    Error,
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
}

impl BackgroundManager {
    /// Spawn a command in the background; returns the task id immediately.
    pub fn spawn(&self, command: &str, timeout_secs: u64) -> String {
        // TODO(s08): validate command (deny list), spawn tokio task,
        // on completion push "{id} [{status}] {first 500 chars}" into
        // notifications; return short id.
        let _ = (command, timeout_secs);
        String::new()
    }

    /// Drain pending notifications ("ping" summaries).
    pub fn drain_notifications(&self) -> Vec<String> {
        // TODO(s08): take() the notification queue.
        Vec::new()
    }

    /// Full result of a task (or all tasks when id is empty).
    pub fn check(&self, id: Option<&str>) -> String {
        // TODO(s08): format status + truncated result.
        let _ = id;
        String::new()
    }
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
        // TODO(s08): { command: string, timeout?: int }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        // TODO(s08): spawn and return the id.
        Ok(ToolResult::err("background_run not implemented yet"))
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
        // TODO(s08): { id?: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        // TODO(s08): delegate to manager.check.
        Ok(ToolResult::err("background_check not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, manager: std::sync::Arc<BackgroundManager>) {
    registry.register(Box::new(BackgroundRunTool { manager: manager.clone() }));
    registry.register(Box::new(BackgroundCheckTool { manager }));
}
