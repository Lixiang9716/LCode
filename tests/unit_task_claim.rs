//! Unit tests for task ownership and autonomous claiming (learn-claude-code
//! s17): the `owner` field, `claim` (pending + deps done + unowned, atomic
//! under the manager lock), `can_start`, `scan_unclaimed`, and the
//! `task_claim` tool.

use lcode::agent::{Task, TaskClaimTool, TaskManager, TaskStatus};
use lcode::tools::Tool;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn manager_in(tmp: &TempDir) -> TaskManager {
    TaskManager::new(tmp.path())
}

// ---------------------------------------------------------------------------
// owner field + backward compatibility
// ---------------------------------------------------------------------------

#[test]
fn old_task_files_default_owner_to_none() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);

    // Pre-owner task file (no `owner` key) must still parse.
    std::fs::write(
        tmp.path().join(".tasks/task_1.json"),
        r#"{"id": 1, "title": "legacy", "status": "pending", "blockedBy": []}"#,
    )
    .unwrap();
    assert_eq!(manager.get(1).unwrap().owner, None);

    // New tasks are created unowned; claiming persists the owner to disk.
    let task = manager.create("fresh", vec![]).unwrap();
    let claimed = manager.claim(task.id, "alice").unwrap();
    assert_eq!(claimed.owner.as_deref(), Some("alice"));
    assert_eq!(manager.get(task.id).unwrap().owner.as_deref(), Some("alice"));
}

// ---------------------------------------------------------------------------
// claim
// ---------------------------------------------------------------------------

#[test]
fn claim_sets_owner_and_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let task = manager.create("Write docs", vec![]).unwrap();

    let claimed = manager.claim(task.id, "alice").unwrap();
    assert_eq!(claimed.status, TaskStatus::InProgress);
    assert_eq!(claimed.owner.as_deref(), Some("alice"));

    // A second claim by someone else is refused (already in_progress).
    let err = manager.claim(task.id, "bob").unwrap_err();
    assert!(err.to_string().contains("cannot claim"), "{err}");
}

#[test]
fn claim_rejects_non_pending_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let task = manager.create("Done task", vec![]).unwrap();
    manager.update(task.id, TaskStatus::Completed, None).unwrap();

    let err = manager.claim(task.id, "alice").unwrap_err();
    assert!(err.to_string().contains("is completed, cannot claim"), "{err}");
}

#[test]
fn claim_rejects_blocked_and_missing_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let setup = manager.create("Setup", vec![]).unwrap();
    let code = manager.create("Write code", vec![setup.id]).unwrap();
    let missing = manager.create("Depends on nothing on disk", vec![99]).unwrap();

    let err = manager.claim(code.id, "alice").unwrap_err();
    assert!(err.to_string().contains("blocked by uncompleted dependencies"), "{err}");

    let err = manager.claim(missing.id, "alice").unwrap_err();
    assert!(err.to_string().contains("blocked by uncompleted dependencies"), "{err}");

    // Completing the dependency unblocks the claim.
    manager.update(setup.id, TaskStatus::Completed, None).unwrap();
    let claimed = manager.claim(code.id, "alice").unwrap();
    assert_eq!(claimed.owner.as_deref(), Some("alice"));
}

#[test]
fn claim_is_atomic_under_concurrent_claimants() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = Arc::new(Mutex::new(manager_in(&tmp)));
    manager.lock().unwrap().create("hot task", vec![]).unwrap();

    // 8 workers race to claim task 1: exactly one may win.
    let mut handles = Vec::new();
    for i in 0..8 {
        let manager = manager.clone();
        handles.push(std::thread::spawn(move || {
            let manager = manager.lock().unwrap();
            manager.claim(1, &format!("worker-{i}"))
        }));
    }
    let winners: Vec<Task> = handles.into_iter().filter_map(|h| h.join().unwrap().ok()).collect();
    assert_eq!(winners.len(), 1, "exactly one claimant must win the race");

    let task = manager.lock().unwrap().get(1).unwrap();
    assert_eq!(task.status, TaskStatus::InProgress);
    assert!(task.owner.is_some());
}

// ---------------------------------------------------------------------------
// can_start / scan_unclaimed
// ---------------------------------------------------------------------------

#[test]
fn can_start_checks_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let a = manager.create("a", vec![]).unwrap();
    let b = manager.create("b", vec![a.id]).unwrap();
    let c = manager.create("c", vec![99]).unwrap();

    assert!(manager.can_start(a.id).unwrap());
    assert!(!manager.can_start(b.id).unwrap(), "b waits on pending a");
    assert!(!manager.can_start(c.id).unwrap(), "missing dependency blocks");

    manager.update(a.id, TaskStatus::Completed, None).unwrap();
    assert!(manager.can_start(b.id).unwrap(), "b unblocked once a completes");

    assert!(manager.can_start(999).is_err(), "unknown task errors");
}

#[test]
fn scan_unclaimed_lists_only_startable_pending_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = manager_in(&tmp);
    let a = manager.create("a", vec![]).unwrap();
    let b = manager.create("b", vec![a.id]).unwrap();
    let c = manager.create("c", vec![]).unwrap();
    let owned = manager.create("owned", vec![]).unwrap();
    manager.claim(owned.id, "alice").unwrap();
    manager.claim(c.id, "alice").unwrap();

    // b is blocked (a pending); c and owned are claimed; a is pending.
    let ids: Vec<u32> = manager.scan_unclaimed().iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![a.id]);

    // Completing a unblocks b; the scan now offers it.
    manager.update(a.id, TaskStatus::Completed, None).unwrap();
    let ids: Vec<u32> = manager.scan_unclaimed().iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![b.id], "completed and claimed tasks are excluded");
}

// ---------------------------------------------------------------------------
// task_claim tool
// ---------------------------------------------------------------------------

#[test]
fn task_claim_tool_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = Arc::new(Mutex::new(manager_in(&tmp)));
    manager.lock().unwrap().create("setup", vec![]).unwrap();
    manager.lock().unwrap().create("build", vec![1]).unwrap();
    let tool = TaskClaimTool { manager: manager.clone() };

    // Blocked task cannot be claimed.
    let result = tool.execute(&serde_json::json!({ "id": 2, "owner": "alice" })).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("blocked by uncompleted dependencies"));

    // Default owner is "agent" when omitted.
    let result = tool.execute(&serde_json::json!({ "id": 1 })).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "[>] #1: setup (owner: agent)");

    // Claimed twice -> failed tool result (already in_progress).
    let result = tool.execute(&serde_json::json!({ "id": 1, "owner": "alice" })).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("cannot claim"));

    // Unknown id -> failed tool result; missing id -> tool error.
    let result = tool.execute(&serde_json::json!({ "id": 42 })).unwrap();
    assert!(!result.success);
    assert_eq!(result.output, "Task 42 not found");
    let err = tool.execute(&serde_json::json!({})).expect_err("id required");
    assert!(err.to_string().contains("missing required argument 'id'"));
}

#[test]
fn task_claim_tool_claims_with_explicit_owner_and_list_shows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = Arc::new(Mutex::new(manager_in(&tmp)));
    manager.lock().unwrap().create("refactor", vec![]).unwrap();
    let tool = TaskClaimTool { manager: manager.clone() };

    let result = tool.execute(&serde_json::json!({ "id": 1, "owner": "alice" })).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "[>] #1: refactor (owner: alice)");

    // The board lists the owner for the model.
    let list = lcode::agent::TaskListTool { manager: manager.clone() };
    let result = list.execute(&serde_json::json!({})).unwrap();
    assert!(result.output.contains("#1: refactor (owner: alice)"));
}
