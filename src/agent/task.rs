//! Persistent task board (learn-claude-code s07).
//!
//! Tasks live on disk (one JSON file per task) so state survives context
//! compression and restarts. A `blockedBy` edge expresses dependencies;
//! completing a task clears its dependencies. "ready" = pending + empty
//! blockedBy.

use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;

/// Status of a persistent task.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

/// A persistent task record.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: u32,
    pub title: String,
    pub status: TaskStatus,
    pub blocked_by: Vec<u32>,
}

/// Disk-backed task manager. One file per task under `tasks_dir`.
#[derive(Debug)]
pub struct TaskManager {
    tasks_dir: PathBuf,
}

impl TaskManager {
    /// Create a manager rooted at `workspace/.tasks`.
    pub fn new(workspace: &PathBuf) -> Self {
        Self { tasks_dir: workspace.join(".tasks") }
    }

    /// Create a task; the id is max(existing ids) + 1.
    pub fn create(&mut self, title: &str, blocked_by: Vec<u32>) -> anyhow::Result<Task> {
        // TODO(s07): allocate id from disk, write task_N.json, return it.
        let _ = (title, blocked_by);
        anyhow::bail!("task.create not implemented yet")
    }

    /// Update status / dependencies. Completing a task clears its id from
    /// every other task's blockedBy.
    pub fn update(&mut self, id: u32, status: TaskStatus, blocked_by: Option<Vec<u32>>) -> anyhow::Result<Task> {
        // TODO(s07): read task_N.json, apply changes, write back.
        let _ = (id, status, blocked_by);
        anyhow::bail!("task.update not implemented yet")
    }

    /// Render the board as a readable list for the model.
    pub fn list(&self) -> String {
        // TODO(s07): `[ ]` / `[>]` / `[x] #id: title (blocked by: ...)`.
        String::new()
    }
}

// --- Tools -------------------------------------------------------------

/// Tool: `task_create`.
pub struct TaskCreateTool {
    pub manager: std::sync::Mutex<TaskManager>,
}

impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "task_create"
    }
    fn description(&self) -> &str {
        "Create a persistent task on the task board. Dependencies are \
         given as task ids in blocked_by."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s07): { title: string, blocked_by?: [int] }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("task_create not implemented yet"))
    }
}

/// Tool: `task_update`.
pub struct TaskUpdateTool {
    pub manager: std::sync::Mutex<TaskManager>,
}

impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "task_update"
    }
    fn description(&self) -> &str {
        "Update a task's status (pending/in_progress/completed) or \
         dependencies."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s07): { id: int, status?: enum, add_blocked_by?: [int], remove_blocked_by?: [int] }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("task_update not implemented yet"))
    }
}

/// Tool: `task_list`.
pub struct TaskListTool {
    pub manager: std::sync::Mutex<TaskManager>,
}

impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }
    fn description(&self) -> &str {
        "List the task board: status, ids, and dependencies."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("task_list not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let manager = std::sync::Mutex::new(TaskManager::new(workspace));
    registry.register(Box::new(TaskCreateTool { manager: std::sync::Mutex::new(TaskManager::new(workspace)) }));
    registry.register(Box::new(TaskUpdateTool { manager }));
    registry.register(Box::new(TaskListTool { manager: std::sync::Mutex::new(TaskManager::new(workspace)) }));
}
