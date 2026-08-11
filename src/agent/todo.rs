//! Todo tracking (learn-claude-code s03).
//!
//! The model owns the plan: it writes progress through the `todo_update`
//! tool, and the harness only constrains (one `in_progress` at a time) and
//! reminds (nag when the model forgets to update).

use crate::tools::{Tool, ToolResult};
use serde::{Deserialize, Serialize};

/// Maximum number of todo items the model may track at once.
const MAX_TODOS: usize = 20;

/// A single todo item.
///
/// Ids are assigned by the manager on every update (the Nth item in the
/// list gets id N); the serde default lets the tool parse model-provided
/// items that carry no id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    #[serde(default)]
    pub id: usize,
    pub text: String,
    pub status: TodoStatus,
}

/// Todo lifecycle status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// The todo manager holds the model-owned plan and renders it back to the
/// model on every update (state echo).
#[derive(Debug, Default)]
pub struct TodoManager {
    items: Vec<TodoItem>,
    /// The current agent turn, recorded by the executor each loop
    /// iteration via [`TodoManager::note_turn`].
    current_turn: u32,
    /// The turn in which the list was last updated, if ever.
    last_update_turn: Option<u32>,
}

impl TodoManager {
    /// Replace the whole list with the given items.
    ///
    /// Constraints (matching the s03 reference): at most [`MAX_TODOS`]
    /// items, every item needs non-empty text, and at most one item may be
    /// `in_progress`. On success the manager assigns positional ids
    /// (1-based) and records the current turn as the last-update turn.
    pub fn update(&mut self, items: Vec<TodoItem>) -> anyhow::Result<()> {
        if items.len() > MAX_TODOS {
            anyhow::bail!("Max {MAX_TODOS} todos allowed");
        }
        let mut validated = Vec::with_capacity(items.len());
        let mut in_progress = 0usize;
        for (index, item) in items.into_iter().enumerate() {
            let id = index + 1;
            let text = item.text.trim();
            if text.is_empty() {
                anyhow::bail!("Item {id}: text required");
            }
            if item.status == TodoStatus::InProgress {
                in_progress += 1;
            }
            validated.push(TodoItem { id, text: text.to_string(), status: item.status });
        }
        if in_progress > 1 {
            anyhow::bail!("Only one item can be in_progress at a time");
        }
        self.items = validated;
        self.last_update_turn = Some(self.current_turn);
        Ok(())
    }

    /// Render the list as a readable snapshot for the model.
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "No todos.".to_string();
        }
        let mut lines = Vec::with_capacity(self.items.len() + 1);
        for item in &self.items {
            let marker = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[>]",
                TodoStatus::Completed => "[x]",
            };
            lines.push(format!("{marker} #{}: {}", item.id, item.text));
        }
        let done = self.items.iter().filter(|i| i.status == TodoStatus::Completed).count();
        lines.push(format!("\n({done}/{total} completed)", done = done, total = self.items.len()));
        lines.join("\n")
    }

    /// Record the current agent turn; the executor calls this at the start
    /// of every loop iteration so the nag can measure how many turns have
    /// passed since the model last updated its plan.
    pub fn note_turn(&mut self, turn: u32) {
        self.current_turn = turn;
    }

    /// Number of turns since the last update (used by the nag); 0 when the
    /// list has never been updated.
    pub fn turns_since_update(&self) -> u32 {
        match self.last_update_turn {
            Some(last) => self.current_turn.saturating_sub(last),
            None => 0,
        }
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
        let status = serde_json::json!({
            "type": "string",
            "enum": ["pending", "in_progress", "completed"],
            "description": "Lifecycle status"
        });
        let item = serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "What to do" },
                "status": status
            },
            "required": ["text", "status"]
        });
        let items = serde_json::json!({
            "type": "array",
            "items": item,
            "description": "The complete todo list; ids are assigned by the harness"
        });
        serde_json::json!({
            "type": "object",
            "properties": { "items": items },
            "required": ["items"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(items_val) = args.get("items") else {
            return Err(anyhow::anyhow!("todo_update: missing required argument 'items'"));
        };
        let items: Vec<TodoItem> = match serde_json::from_value(items_val.clone()) {
            Ok(items) => items,
            Err(e) => return Ok(ToolResult::err(format!("todo_update: invalid items: {e}"))),
        };
        let mut manager = self.manager.lock().unwrap();
        match manager.update(items) {
            Ok(()) => Ok(ToolResult::ok(manager.render())),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
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
