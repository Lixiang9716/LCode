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

/// A persistent task record. Serialized to `task_{id}.json` with
/// camelCase keys (`blockedBy`), matching the s07 reference format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// Render one task line: `[ ]` / `[>]` / `[x] #id: title (blocked by: ...)`.
fn render_task(task: &Task) -> String {
    let marker = match task.status {
        TaskStatus::Pending => "[ ]",
        TaskStatus::InProgress => "[>]",
        TaskStatus::Completed => "[x]",
    };
    let blocked = if task.blocked_by.is_empty() {
        String::new()
    } else {
        format!(" (blocked by: {})", join_ids(&task.blocked_by))
    };
    format!("{marker} #{id}: {title}{blocked}", id = task.id, title = task.title)
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
            Ok(task) => Ok(ToolResult::ok(render_task(&task))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `task_update`.
pub struct TaskUpdateTool {
    pub manager: Arc<Mutex<TaskManager>>,
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
        match manager.update(id, status.unwrap_or(current.status), blocked_by) {
            Ok(task) => Ok(ToolResult::ok(render_task(&task))),
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
        "List the task board: status, ids, and dependencies."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let manager = self.manager.lock().unwrap();
        Ok(ToolResult::ok(manager.list()))
    }
}

/// Register this module's tools with the registry. All three tools share
/// a single [`TaskManager`] so the board stays consistent across tools.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &Path) {
    let manager = Arc::new(Mutex::new(TaskManager::new(workspace)));
    registry.register(Box::new(TaskCreateTool { manager: manager.clone() }));
    registry.register(Box::new(TaskUpdateTool { manager: manager.clone() }));
    registry.register(Box::new(TaskListTool { manager }));
}
