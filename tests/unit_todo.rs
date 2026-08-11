//! Unit tests for the todo module (learn-claude-code s03): list
//! validation, rendering, turn tracking, and the `todo_update` tool.

use lcode::agent::{TodoItem, TodoManager, TodoStatus, TodoUpdateTool};
use lcode::tools::Tool;
use std::sync::{Arc, Mutex};

/// Build a todo item; the `id` is ignored by the manager (ids are
/// assigned positionally on update).
fn item(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem { id: 0, text: text.to_string(), status }
}

#[test]
fn test_update_assigns_ids_and_renders() {
    let mut manager = TodoManager::default();
    manager
        .update(vec![
            item("Plan", TodoStatus::Pending),
            item("Code", TodoStatus::InProgress),
            item("Test", TodoStatus::Completed),
            item("Ship", TodoStatus::Completed),
        ])
        .expect("valid list should update");

    assert_eq!(
        manager.render(),
        "[ ] #1: Plan\n[>] #2: Code\n[x] #3: Test\n[x] #4: Ship\n\n(2/4 completed)"
    );
}

#[test]
fn test_update_accepts_exactly_max_items() {
    let mut manager = TodoManager::default();
    let items: Vec<TodoItem> =
        (0..20).map(|i| item(&format!("task {i}"), TodoStatus::Pending)).collect();
    manager.update(items).expect("20 items is allowed");
    assert!(manager.render().contains("(0/20 completed)"));
}

#[test]
fn test_update_rejects_more_than_max_items() {
    let mut manager = TodoManager::default();
    let items: Vec<TodoItem> =
        (0..21).map(|i| item(&format!("task {i}"), TodoStatus::Pending)).collect();
    let err = manager.update(items).expect_err("21 items must be rejected");
    assert_eq!(err.to_string(), "Max 20 todos allowed");
}

#[test]
fn test_update_rejects_empty_text() {
    let mut manager = TodoManager::default();
    let err = manager.update(vec![item("  ", TodoStatus::Pending)]).expect_err("empty text");
    assert_eq!(err.to_string(), "Item 1: text required");
}

#[test]
fn test_update_rejects_multiple_in_progress() {
    let mut manager = TodoManager::default();
    let err = manager
        .update(vec![item("a", TodoStatus::InProgress), item("b", TodoStatus::InProgress)])
        .expect_err("two in_progress items");
    assert_eq!(err.to_string(), "Only one item can be in_progress at a time");
}

#[test]
fn test_update_error_keeps_previous_state() {
    let mut manager = TodoManager::default();
    manager.update(vec![item("keep me", TodoStatus::Completed)]).unwrap();
    let err = manager
        .update(vec![item("a", TodoStatus::InProgress), item("b", TodoStatus::InProgress)])
        .expect_err("invalid replacement");
    assert_eq!(err.to_string(), "Only one item can be in_progress at a time");
    // The previous list must be untouched after a failed update.
    assert_eq!(manager.render(), "[x] #1: keep me\n\n(1/1 completed)");
}

#[test]
fn test_render_empty() {
    assert_eq!(TodoManager::default().render(), "No todos.");
}

#[test]
fn test_turns_since_update() {
    let mut manager = TodoManager::default();
    manager.note_turn(5);
    // Never updated: the nag counter stays at 0.
    assert_eq!(manager.turns_since_update(), 0);

    manager.update(vec![item("a", TodoStatus::Pending)]).unwrap();
    // Updated during this turn: still 0.
    assert_eq!(manager.turns_since_update(), 0);

    manager.note_turn(8);
    assert_eq!(manager.turns_since_update(), 3);

    // An update in a later turn resets the counter.
    manager.note_turn(10);
    manager.update(vec![item("b", TodoStatus::InProgress)]).unwrap();
    assert_eq!(manager.turns_since_update(), 0);
}

#[test]
fn test_todo_update_tool_round_trip() {
    let manager = Arc::new(Mutex::new(TodoManager::default()));
    let tool = TodoUpdateTool { manager: manager.clone() };

    let result = tool
        .execute(&serde_json::json!({
            "items": [
                {"text": "Plan", "status": "pending"},
                {"text": "Code", "status": "in_progress"},
                {"text": "Ship", "status": "completed"}
            ]
        }))
        .expect("execute should succeed");
    assert!(result.success);
    assert_eq!(result.output, "[ ] #1: Plan\n[>] #2: Code\n[x] #3: Ship\n\n(1/3 completed)");

    // The tool wrote into the shared manager.
    assert!(!manager.lock().unwrap().is_empty());
}

#[test]
fn test_todo_update_tool_parameters_schema() {
    let manager = Arc::new(Mutex::new(TodoManager::default()));
    let tool = TodoUpdateTool { manager };

    let schema = tool.parameters();
    let status_enum = schema["properties"]["items"]["items"]["properties"]["status"]["enum"]
        .as_array()
        .expect("status enum should be an array");
    let status_values: Vec<&str> = status_enum.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(status_values, vec!["pending", "in_progress", "completed"]);

    let required: Vec<&str> =
        schema["required"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(required, vec!["items"]);
}

#[test]
fn test_todo_update_tool_errors() {
    let manager = Arc::new(Mutex::new(TodoManager::default()));
    let tool = TodoUpdateTool { manager };

    // Missing required argument -> hard error.
    let err = tool.execute(&serde_json::json!({})).expect_err("missing items");
    assert!(err.to_string().contains("missing required argument 'items'"));

    // Empty text -> failed tool result with the manager's message.
    let result =
        tool.execute(&serde_json::json!({"items": [{"text": "", "status": "pending"}]})).unwrap();
    assert!(!result.success);
    assert_eq!(result.output, "Item 1: text required");

    // Unknown status -> failed tool result.
    let result =
        tool.execute(&serde_json::json!({"items": [{"text": "x", "status": "bogus"}]})).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("invalid"));
}
