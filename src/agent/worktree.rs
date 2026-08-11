//! Worktree task isolation (learn-claude-code s12).
//!
//! Control plane and execution plane are separated: the task board says
//! WHAT to do, git worktrees say WHERE to do it, bound by task_id.
//! Every lifecycle mutation emits before/after/failed events into
//! `.worktrees/events.jsonl` as a persistent audit stream.

use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;

/// Persistent append-only event log (JSONL).
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(workspace: &PathBuf) -> Self {
        Self { path: workspace.join(".worktrees").join("events.jsonl") }
    }

    /// Append an event line; tolerant of write failures.
    pub fn emit(&self, event: &str, task_id: u32, worktree: Option<&str>, error: Option<&str>) {
        // TODO(s12): append `{"event":..,"task_id":..,"worktree":..,"error":..}`
        // JSON line; create parent dir.
        let _ = (event, task_id, worktree, error);
    }
}

/// Manages git worktrees bound to tasks.
#[derive(Debug)]
pub struct WorktreeManager {
    workspace: PathBuf,
    worktrees_dir: PathBuf,
    log: EventLog,
}

impl WorktreeManager {
    pub fn new(workspace: &PathBuf) -> Self {
        Self {
            workspace: workspace.clone(),
            worktrees_dir: workspace.join(".worktrees"),
            log: EventLog::new(workspace),
        }
    }

    /// Create a worktree `wt/{name}` for a task: emit create.before →
    /// `git worktree add -b wt/{name} path HEAD` → index.json →
    /// task bind → emit create.after; on failure emit create.failed.
    pub fn create(&self, name: &str, task_id: u32) -> anyhow::Result<PathBuf> {
        // TODO(s12): name regex [A-Za-z0-9._-]{1,40}, run git, write index.
        let _ = (name, task_id);
        anyhow::bail!("worktree.create not implemented yet")
    }

    /// Remove a worktree; optionally complete the bound task.
    pub fn remove(&self, name: &str, complete_task: bool) -> anyhow::Result<()> {
        // TODO(s12): before → git worktree remove → complete task →
        // tombstone in index → after.
        let _ = (name, complete_task);
        Ok(())
    }

    /// Run a command inside the worktree.
    pub fn run(&self, name: &str, command: &str) -> anyhow::Result<String> {
        // TODO(s12): subprocess in worktree path, 300s timeout, deny list.
        let _ = (name, command);
        Ok(String::new())
    }
}

// --- Tools -------------------------------------------------------------

/// Tool: `worktree_create`.
pub struct WorktreeCreateTool {
    pub manager: std::sync::Arc<WorktreeManager>,
}

impl Tool for WorktreeCreateTool {
    fn name(&self) -> &str {
        "worktree_create"
    }
    fn description(&self) -> &str {
        "Create a git worktree for a task (branch wt/{name}). The task \
         moves to in_progress. Commands run inside the worktree are \
         isolated from the main workspace."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s12): { name: string, task_id: int }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("worktree_create not implemented yet"))
    }
}

/// Tool: `worktree_run`.
pub struct WorktreeRunTool {
    pub manager: std::sync::Arc<WorktreeManager>,
}

impl Tool for WorktreeRunTool {
    fn name(&self) -> &str {
        "worktree_run"
    }
    fn description(&self) -> &str {
        "Run a command inside a task's worktree."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s12): { name: string, command: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("worktree_run not implemented yet"))
    }
}

/// Tool: `worktree_remove`.
pub struct WorktreeRemoveTool {
    pub manager: std::sync::Arc<WorktreeManager>,
}

impl Tool for WorktreeRemoveTool {
    fn name(&self) -> &str {
        "worktree_remove"
    }
    fn description(&self) -> &str {
        "Remove a task's worktree; optionally mark the task completed."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s12): { name: string, complete_task?: bool }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("worktree_remove not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let manager = std::sync::Arc::new(WorktreeManager::new(workspace));
    registry.register(Box::new(WorktreeCreateTool { manager: manager.clone() }));
    registry.register(Box::new(WorktreeRunTool { manager: manager.clone() }));
    registry.register(Box::new(WorktreeRemoveTool { manager }));
}
