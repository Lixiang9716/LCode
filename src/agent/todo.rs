//! Todo tracking (learn-claude-code s03).
//!
//! The model owns the plan: it writes progress through the `todo_update`
//! tool, and the harness only constrains (one `in_progress` at a time) and
//! reminds (nag when the model forgets to update).

use crate::tools::{Tool, ToolResult};
use serde::{Deserialize, Serialize};

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub id: usize,
    pub text: String,
    pub status: TodoStatus,
}

/// Todo lifecycle status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// The todo manager holds the model-owned plan and renders it back to the
/// model on every update (state echo).
#[derive(Debug, Default)]
pub struct TodoManager {
    items: Vec<TodoItem>,
    next_id: usize,
}

impl TodoManager {
    /// Replace the whole list with the given items (max 20, one
    /// `in_progress` allowed).
    pub fn update(&mut self, items: Vec<TodoItem>) -> anyhow::Result<()> {
        // TODO(s03): validate count <= 20, require text, enforce the
        // single-in-progress invariant, assign ids, render back.
        self.items = items;
        Ok(())
    }

    /// Render the list as a readable snapshot for the model.
    pub fn render(&self) -> String {
        // TODO(s03): `[ ]` / `[>]` / `[x] #id: text` + (done/total).
        self.items.iter().map(|i| format!("{:?}: {}", i.status, i.text)).collect::<Vec<_>>().join("\n")
    }

    /// Number of turns since the last update (used by the nag).
    pub fn turns_since_update(&self) -> u32 {
        // TODO(s03): track last-update turn; return 0 when never updated.
        0
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Tool: `todo_update` — the model writes its plan through this tool.
pub struct TodoUpdateTool {
    pub manager: std::sync::Arc<std::sync::Mutex<TodoManager>>,
}

impl Tool for TodoUpdateTool {
    fn name(&self) -> &str {
        "todo_update"
    }

    fn description(&self) -> &str {
        "Update the todo list. Items must have text and status \
         (pending/in_progress/completed). Only one item may be in_progress."
    }

    fn parameters(&self) -> serde_json::Value {
        // TODO(s03): full schema with items array + status enum.
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        // TODO(s03): parse items, delegate to manager.update, return render().
        Ok(ToolResult::err("todo_update not implemented yet"))
    }
}

/// Register this module's tools with the registry.
///
/// The manager is created by the caller (session scope) so the executor
/// can share it for the nag counter.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    manager: std::sync::Arc<std::sync::Mutex<TodoManager>>,
) {
    registry.register(Box::new(TodoUpdateTool { manager }));
}
