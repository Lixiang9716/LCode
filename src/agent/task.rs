//! Persistent task board (learn-claude-code s07).
//!
//! Tasks live on disk (one JSON file per task) so state survives context
//! compression and restarts. A `blockedBy` edge expresses dependencies;
//! completing a task clears its dependencies. "ready" = pending + empty
//! blockedBy.

use crate::tools::{Tool, ToolResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Status of a persistent task.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    /// Lowercase form used in error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
        }
    }
}

/// A persistent task record. Serialized to `task_{id}.json` with
/// camelCase keys (`blockedBy`), matching the s07 reference format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: u32,
    pub title: String,
    pub status: TaskStatus,
    pub blocked_by: Vec<u32>,
    /// Claiming agent (s17). `None` while unclaimed; older task files
    /// without the key default to `None` (serde default).
    #[serde(default)]
    pub owner: Option<String>,
}

/// Disk-backed task manager. One file per task under `tasks_dir`.
#[derive(Debug)]
pub struct TaskManager {
    tasks_dir: PathBuf,
}

impl TaskManager {
    /// Create a manager rooted at `workspace/.tasks`, creating the
    /// directory if needed.
    pub fn new(workspace: &Path) -> Self {
        let tasks_dir = workspace.join(".tasks");
        let _ = std::fs::create_dir_all(&tasks_dir);
        Self { tasks_dir }
    }

    /// Create a task; the id is max(existing ids) + 1.
    pub fn create(&mut self, title: &str, blocked_by: Vec<u32>) -> anyhow::Result<Task> {
        let _ = std::fs::create_dir_all(&self.tasks_dir);
        let task = Task {
            id: self.max_id() + 1,
            title: title.to_string(),
            status: TaskStatus::Pending,
            blocked_by,
            owner: None,
        };
        self.save(&task)?;
        Ok(task)
    }

    /// Fetch a task by id.
    pub fn get(&self, id: u32) -> anyhow::Result<Task> {
        let path = self.tasks_dir.join(format!("task_{id}.json"));
        let content =
            std::fs::read_to_string(&path).map_err(|_| anyhow::anyhow!("Task {id} not found"))?;
        Ok(serde_json::from_str(&content)?)
    }

    /// Update status / dependencies. Completing a task clears its id from
    /// every other task's blockedBy.
    pub fn update(
        &mut self,
        id: u32,
        status: TaskStatus,
        blocked_by: Option<Vec<u32>>,
    ) -> anyhow::Result<Task> {
        let mut task = self.get(id)?;
        task.status = status;
        if let Some(blocked_by) = blocked_by {
            task.blocked_by = blocked_by;
        }
        self.save(&task)?;
        if status == TaskStatus::Completed {
            self.clear_dependency(id)?;
        }
        Ok(task)
    }

    /// True when every dependency of `id` exists and is completed (s17).
    /// Missing dependencies block the task.
    pub fn can_start(&self, id: u32) -> anyhow::Result<bool> {
        let task = self.get(id)?;
        for dependency in &task.blocked_by {
            let dependency = match self.get(*dependency) {
                Ok(dependency) => dependency,
                Err(_) => return Ok(false),
            };
            if dependency.status != TaskStatus::Completed {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Claim a task for `owner`: only pending, unclaimed tasks whose
    /// dependencies are all completed may be claimed. The read-check-write
    /// happens under the caller's manager lock (atomic claim, s17).
    pub fn claim(&self, id: u32, owner: &str) -> anyhow::Result<Task> {
        let mut task = self.get(id)?;
        if task.status != TaskStatus::Pending {
            anyhow::bail!("Task {id} is {}, cannot claim", task.status.as_str());
        }
        if let Some(current) = &task.owner {
            anyhow::bail!("Task {id} already owned by {current}");
        }
        if !self.can_start(id)? {
            anyhow::bail!("Task {id} blocked by uncompleted dependencies");
        }
        task.owner = Some(owner.to_string());
        task.status = TaskStatus::InProgress;
        self.save(&task)?;
        Ok(task)
    }

    /// Tasks that are pending, unowned, and ready to start (all
    /// dependencies completed) — the board entries an autonomous
    /// teammate may claim (s17).
    pub fn scan_unclaimed(&self) -> Vec<Task> {
        let mut unclaimed = Vec::new();
        for id in self.ids_on_disk() {
            let Ok(task) = self.get(id) else { continue };
            if task.status == TaskStatus::Pending
                && task.owner.is_none()
                && self.can_start(id).unwrap_or(false)
            {
                unclaimed.push(task);
            }
        }
        unclaimed
    }

    /// Render the board as a readable list for the model.
    pub fn list(&self) -> String {
        let ids = self.ids_on_disk();
        if ids.is_empty() {
            return "No tasks.".to_string();
        }
        let mut lines = Vec::with_capacity(ids.len());
        for id in ids {
            if let Ok(task) = self.get(id) {
                lines.push(render_task(&task));
            }
        }
        lines.join("\n")
    }

    /// Highest task id present on disk (0 when the board is empty).
    fn max_id(&self) -> u32 {
        self.ids_on_disk().into_iter().max().unwrap_or(0)
    }

    /// All task ids present on disk, ascending.
    fn ids_on_disk(&self) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir(&self.tasks_dir) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = task_id_from_file_name(&name) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        ids
    }

    /// Remove `completed_id` from every other task's blockedBy list,
    /// automatically unblocking its dependents.
    fn clear_dependency(&self, completed_id: u32) -> anyhow::Result<()> {
        for id in self.ids_on_disk() {
            if id == completed_id {
                continue;
            }
            let mut task = match self.get(id) {
                Ok(task) => task,
                Err(_) => continue,
            };
            if task.blocked_by.contains(&completed_id) {
                task.blocked_by.retain(|blocked| *blocked != completed_id);
                self.save(&task)?;
            }
        }
        Ok(())
    }

    /// Write `task_{id}.json`.
    fn save(&self, task: &Task) -> anyhow::Result<()> {
        let path = self.tasks_dir.join(format!("task_{}.json", task.id));
        std::fs::write(&path, serde_json::to_string_pretty(task)?)?;
        Ok(())
    }
}

/// Render one task line: `[ ]` / `[>]` / `[x] #id: title (owner / blocked by)`.
fn render_task(task: &Task) -> String {
    let marker = match task.status {
        TaskStatus::Pending => "[ ]",
        TaskStatus::InProgress => "[>]",
        TaskStatus::Completed => "[x]",
    };
    let mut detail: Vec<String> = Vec::new();
    if let Some(owner) = &task.owner {
        detail.push(format!("owner: {owner}"));
    }
    if !task.blocked_by.is_empty() {
        detail.push(format!("blocked by: {}", join_ids(&task.blocked_by)));
    }
    let suffix =
        if detail.is_empty() { String::new() } else { format!(" ({})", detail.join("; ")) };
    format!("{marker} #{id}: {title}{suffix}", id = task.id, title = task.title)
}

/// `1, 2, 3` — comma-separated task ids.
fn join_ids(ids: &[u32]) -> String {
    ids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

/// Parse `task_7.json` file names into their task id.
fn task_id_from_file_name(name: &str) -> Option<u32> {
    let stem = name.strip_prefix("task_")?.strip_suffix(".json")?;
    stem.parse().ok()
}

// --- Tools -------------------------------------------------------------

/// Parse an optional array of task ids from the tool arguments.
fn opt_ids(args: &serde_json::Value, key: &str) -> anyhow::Result<Vec<u32>> {
    match args.get(key) {
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Vec::new()),
    }
}

/// Tool: `task_create`.
pub struct TaskCreateTool {
    pub manager: Arc<Mutex<TaskManager>>,
    /// Session event bus; publishes `TaskCreated` after creation.
    pub events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
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
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "What the task is about" },
                "blocked_by": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Task ids this task depends on"
                }
            },
            "required": ["title"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let title = args["title"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("task_create: missing required argument 'title'"))?;
        let blocked_by = opt_ids(args, "blocked_by")?;
        let mut manager = self.manager.lock().unwrap();
        match manager.create(title, blocked_by) {
            Ok(task) => {
                let event = crate::agent::AgentEvent::TaskCreated {
                    id: task.id,
                    title: task.title.clone(),
                };
                publish(&self.events, event);
                Ok(ToolResult::ok(render_task(&task)))
            }
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `task_update`.
pub struct TaskUpdateTool {
    pub manager: Arc<Mutex<TaskManager>>,
    /// Session event bus; publishes `TaskUpdated` after updates.
    pub events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
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
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "description": "Task id to update" },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "New status"
                },
                "add_blocked_by": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Task ids to add as dependencies"
                },
                "remove_blocked_by": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Task ids to remove from dependencies"
                }
            },
            "required": ["id"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = args["id"]
            .as_u64()
            .map(|id| id as u32)
            .ok_or_else(|| anyhow::anyhow!("task_update: missing required argument 'id'"))?;
        let status = match args.get("status") {
            Some(value) => match serde_json::from_value::<TaskStatus>(value.clone()) {
                Ok(status) => Some(status),
                Err(e) => {
                    return Ok(ToolResult::err(format!("task_update: invalid status: {e}")));
                }
            },
            None => None,
        };
        let add = opt_ids(args, "add_blocked_by")?;
        let remove = opt_ids(args, "remove_blocked_by")?;

        let mut manager = self.manager.lock().unwrap();
        let current = match manager.get(id) {
            Ok(task) => task,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let mut blocked_by = current.blocked_by.clone();
        for extra in &add {
            if !blocked_by.contains(extra) {
                blocked_by.push(*extra);
            }
        }
        if !remove.is_empty() {
            blocked_by.retain(|blocked| !remove.contains(blocked));
        }
        let blocked_by = if add.is_empty() && remove.is_empty() { None } else { Some(blocked_by) };
        let updated = manager.update(id, status.unwrap_or(current.status), blocked_by);
        match updated {
            Ok(task) => {
                let event = crate::agent::AgentEvent::TaskUpdated {
                    id: task.id,
                    status: task.status.as_str().to_string(),
                };
                publish(&self.events, event);
                Ok(ToolResult::ok(render_task(&task)))
            }
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `task_list`.
pub struct TaskListTool {
    pub manager: Arc<Mutex<TaskManager>>,
}

impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }
    fn description(&self) -> &str {
        "List the task board: status, owners, ids, and dependencies."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let manager = self.manager.lock().unwrap();
        Ok(ToolResult::ok(manager.list()))
    }
}

/// Tool: `task_claim` (s17).
pub struct TaskClaimTool {
    pub manager: Arc<Mutex<TaskManager>>,
}

impl Tool for TaskClaimTool {
    fn name(&self) -> &str {
        "task_claim"
    }
    fn description(&self) -> &str {
        "Claim a pending task: assigns the task to an owner and marks it \
         in_progress. Only unowned tasks with all dependencies completed \
         can be claimed."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer", "description": "Task id to claim" },
                "owner": {
                    "type": "string",
                    "description": "Claiming agent's name (default: agent)"
                }
            },
            "required": ["id"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = args["id"]
            .as_u64()
            .map(|id| id as u32)
            .ok_or_else(|| anyhow::anyhow!("task_claim: missing required argument 'id'"))?;
        let owner = args["owner"].as_str().unwrap_or("agent");
        let manager = self.manager.lock().unwrap();
        match manager.claim(id, owner) {
            Ok(task) => Ok(ToolResult::ok(render_task(&task))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Send `event` on the tool's session bus when one is attached.
fn publish(
    events: &Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
    event: crate::agent::AgentEvent,
) {
    if let Some(tx) = events {
        let _ = tx.send(event);
    }
}

/// Register this module's tools with the registry. All four tools share
/// a single [`TaskManager`] so the board stays consistent across tools.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    workspace: &Path,
    events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
) {
    let manager = Arc::new(Mutex::new(TaskManager::new(workspace)));
    registry
        .register(Box::new(TaskCreateTool { manager: manager.clone(), events: events.clone() }));
    registry
        .register(Box::new(TaskUpdateTool { manager: manager.clone(), events: events.clone() }));
    registry.register(Box::new(TaskListTool { manager: manager.clone() }));
    registry.register(Box::new(TaskClaimTool { manager }));
}
