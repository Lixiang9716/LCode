//! Unit tests for the task module (learn-claude-code s07): id
//! allocation, dependency resolution, disk persistence, and the
//! `task_create` / `task_update` / `task_list` tools.

use lcode::agent::{TaskCreateTool, TaskListTool, TaskManager, TaskStatus, TaskUpdateTool};
use lcode::tools::Tool;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// A manager rooted at `tmp/.tasks` (created on demand).
fn manager_in(tmp: &TempDir) -> TaskManager {
    TaskManager::new(tmp.path())
}

#[test]
fn test_new_creates_tasks_dir() {
    let tmp = tempfile::tempdir().unwrap();
    manager_in(&tmp);
    assert!(tmp.path().join(".tasks").is_dir(), ".tasks dir must be created");
}

#[test]
fn test_create_assigns_incrementing_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);

    let setup = manager.create("Setup project", vec![]).unwrap();
    let code = manager.create("Write code", vec![setup.id]).unwrap();

    assert_eq!(setup.id, 1);
    assert_eq!(code.id, 2);
    assert_eq!(setup.status, TaskStatus::Pending);
    assert_eq!(setup.title, "Setup project");
    assert_eq!(code.blocked_by, vec![1]);
}

#[test]
fn test_state_survives_manager_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let mut manager = manager_in(&tmp);
        manager.create("one", vec![]).unwrap();
        manager.create("two", vec![]).unwrap();
    }

    // A fresh manager over the same directory must see the disk state.
    let mut manager = manager_in(&tmp);
    let three = manager.create("three", vec![]).unwrap();
    assert_eq!(three.id, 3, "ids continue past the on-disk maximum");

    let one = manager.get(1).unwrap();
    assert_eq!(one.title, "one");
    assert_eq!(one.status, TaskStatus::Pending);
    assert_eq!(manager.list(), "[ ] #1: one\n[ ] #2: two\n[ ] #3: three");
}

#[test]
fn test_update_changes_status_and_replaces_blocked_by() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let a = manager.create("a", vec![]).unwrap();
    let b = manager.create("b", vec![a.id]).unwrap();

    let updated = manager.update(b.id, TaskStatus::InProgress, None).unwrap();
    assert_eq!(updated.status, TaskStatus::InProgress);
    assert_eq!(manager.get(b.id).unwrap().blocked_by, vec![1]);

    // Some(...) replaces the dependency list entirely.
    manager.update(b.id, TaskStatus::Pending, Some(vec![1, 3])).unwrap();
    assert_eq!(manager.get(b.id).unwrap().blocked_by, vec![1, 3]);
}

#[test]
fn test_completed_task_clears_its_blocked_by_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let a = manager.create("a", vec![]).unwrap();
    let b = manager.create("b", vec![a.id]).unwrap();
    let c = manager.create("c", vec![a.id, b.id]).unwrap();

    manager.update(a.id, TaskStatus::Completed, None).unwrap();

    assert_eq!(manager.get(a.id).unwrap().status, TaskStatus::Completed);
    assert!(manager.get(b.id).unwrap().blocked_by.is_empty(), "b unblocked by a's completion");
    assert_eq!(manager.get(c.id).unwrap().blocked_by, vec![b.id], "only a removed from c");
}

#[test]
fn test_update_missing_task_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let err = manager.update(99, TaskStatus::Completed, None).expect_err("no such task");
    assert_eq!(err.to_string(), "Task 99 not found");
}

#[test]
fn test_list_format_and_empty_board() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    assert_eq!(manager.list(), "No tasks.");

    manager.create("Setup", vec![]).unwrap();
    manager.create("Tests", vec![1]).unwrap();
    let ship = manager.create("Ship", vec![]).unwrap();
    manager.update(ship.id, TaskStatus::Completed, None).unwrap();
    manager.update(2, TaskStatus::InProgress, None).unwrap();

    assert_eq!(manager.list(), "[ ] #1: Setup\n[>] #2: Tests (blocked by: 1)\n[x] #3: Ship");
}

#[test]
fn test_on_disk_format_is_camel_case_json() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    manager.create("Setup", vec![7]).unwrap();

    let content = std::fs::read_to_string(tmp.path().join(".tasks/task_1.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(value["id"], 1);
    assert_eq!(value["title"], "Setup");
    assert_eq!(value["status"], "pending");
    assert_eq!(value["blockedBy"], serde_json::json!([7]));
}

#[test]
fn test_tools_share_one_manager_and_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = Arc::new(Mutex::new(manager_in(&tmp)));
    let create = TaskCreateTool { manager: manager.clone() };
    let update = TaskUpdateTool { manager: manager.clone() };
    let list = TaskListTool { manager: manager.clone() };

    let result = create.execute(&serde_json::json!({"title": "Setup"})).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "[ ] #1: Setup");

    let result = create.execute(&serde_json::json!({"title": "Tests", "blocked_by": [1]})).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "[ ] #2: Tests (blocked by: 1)");

    // The list tool sees what the create tool wrote (shared manager).
    let result = list.execute(&serde_json::json!({})).unwrap();
    assert_eq!(result.output, "[ ] #1: Setup\n[ ] #2: Tests (blocked by: 1)");

    // Completing task 1 clears it from task 2's edges.
    let result = update.execute(&serde_json::json!({"id": 1, "status": "completed"})).unwrap();
    assert_eq!(result.output, "[x] #1: Setup");
    let result = list.execute(&serde_json::json!({})).unwrap();
    assert_eq!(result.output, "[x] #1: Setup\n[ ] #2: Tests");

    // add_blocked_by merges without duplicating existing edges.
    let result = update.execute(&serde_json::json!({"id": 2, "add_blocked_by": [3, 3]})).unwrap();
    assert_eq!(result.output, "[ ] #2: Tests (blocked by: 3)");
    // remove_blocked_by drops edges; status updates in the same call.
    let result = update
        .execute(&serde_json::json!({"id": 2, "remove_blocked_by": [3], "status": "in_progress"}))
        .unwrap();
    assert_eq!(result.output, "[>] #2: Tests");

    // The board persists through the shared manager.
    let result = list.execute(&serde_json::json!({})).unwrap();
    assert_eq!(result.output, "[x] #1: Setup\n[>] #2: Tests");
}

#[test]
fn test_tools_validate_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = Arc::new(Mutex::new(manager_in(&tmp)));
    let create = TaskCreateTool { manager: manager.clone() };
    let update = TaskUpdateTool { manager: manager.clone() };

    let err = create.execute(&serde_json::json!({})).expect_err("title required");
    assert!(err.to_string().contains("missing required argument 'title'"));

    let err = update.execute(&serde_json::json!({})).expect_err("id required");
    assert!(err.to_string().contains("missing required argument 'id'"));

    // Unknown task id -> failed tool result.
    let result = update.execute(&serde_json::json!({"id": 42})).unwrap();
    assert!(!result.success);
    assert_eq!(result.output, "Task 42 not found");

    // Invalid status -> failed tool result, not a panic.
    let result = update.execute(&serde_json::json!({"id": 42, "status": "bogus"})).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("invalid status"));
}
