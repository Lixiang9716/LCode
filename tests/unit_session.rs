//! Unit tests for session persistence (#7).
//!
//! Covers the save/load roundtrip (messages + todos completeness), list
//! ordering and bad-file tolerance, id generation/uniqueness, and input
//! validation.

use lcode::agent::{
    snapshot, ConversationMemory, SessionSnapshot, SessionStore, TodoItem, TodoManager, TodoStatus,
};
use std::collections::HashSet;
use std::path::Path;

/// A store rooted at a fresh tempdir.
fn store(workspace: &Path) -> SessionStore {
    SessionStore::new(workspace)
}

/// A memory with a user/assistant/tool message trio.
fn memory_with_messages() -> ConversationMemory {
    let mut memory = ConversationMemory::new("system".to_string());
    memory.add_user("Fix the bug");
    memory.add_assistant("Let me look");
    memory.add_tool_result("Found it", "call_1".to_string());
    memory
}

/// A todo manager with three items in mixed states (via `update`, which
/// assigns 1-based ids).
fn todos_with_items() -> TodoManager {
    let mut todos = TodoManager::default();
    todos
        .update(vec![
            TodoItem { id: 0, text: "Explore".to_string(), status: TodoStatus::Completed },
            TodoItem { id: 0, text: "Implement".to_string(), status: TodoStatus::InProgress },
            TodoItem { id: 0, text: "Verify".to_string(), status: TodoStatus::Pending },
        ])
        .expect("todos update ok");
    todos
}

#[test]
fn test_save_load_roundtrip_preserves_messages_and_todos() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());

    let snap =
        snapshot("Implement session persistence", &memory_with_messages(), &todos_with_items());
    let id = store.save(&snap).expect("save snapshot");

    // The .sessions directory and {id}.json were created.
    assert!(tmp.path().join(".sessions").is_dir());
    assert!(tmp.path().join(".sessions").join(format!("{id}.json")).is_file());

    let loaded = store.load(&id).expect("load snapshot");
    assert_eq!(loaded.task, "Implement session persistence");
    assert_eq!(loaded.id, id);

    // Full message history survived, in order.
    assert_eq!(loaded.messages.len(), 3);
    assert_eq!(loaded.messages[0].content, "Fix the bug");
    assert_eq!(loaded.messages[1].content, "Let me look");
    assert_eq!(loaded.messages[2].content, "Found it");
    assert_eq!(loaded.messages[2].tool_call_id.as_deref(), Some("call_1"));

    // Full todo list survived, ids/statuses intact.
    assert_eq!(loaded.todos.len(), 3);
    assert_eq!(loaded.todos[0].text, "Explore");
    assert_eq!(loaded.todos[0].status, TodoStatus::Completed);
    assert_eq!(loaded.todos[1].status, TodoStatus::InProgress);
    assert_eq!(loaded.todos[2].status, TodoStatus::Pending);
    assert_eq!(loaded.todos[1].id, 2);

    // Roundtrip is exact: loading returns the snapshot that was written
    // (ChatMessage has no PartialEq, so compare serialized forms).
    let mut expected = snap;
    expected.id = id;
    assert_eq!(
        serde_json::to_value(&loaded).expect("serialize loaded"),
        serde_json::to_value(&expected).expect("serialize expected")
    );
}

#[test]
fn test_snapshot_captures_live_state() {
    let memory = memory_with_messages();
    let todos = todos_with_items();

    let snap = snapshot("task", &memory, &todos);
    assert_eq!(snap.task, "task");
    assert!(snap.id.is_empty(), "ids are assigned at save time");
    assert!(snap.created_at > 0);
    assert_eq!(snap.messages.len(), 3);
    assert_eq!(snap.todos.len(), 3);
}

#[test]
fn test_save_uses_explicit_id() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());
    let mut snap = snapshot("task", &memory_with_messages(), &todos_with_items());
    snap.id = "cafe0001".to_string();

    let id = store.save(&snap).expect("save snapshot");
    assert_eq!(id, "cafe0001");
    assert!(tmp.path().join(".sessions/cafe0001.json").is_file());
}

#[test]
fn test_list_returns_snapshots_newest_first() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());

    let old = SessionSnapshot {
        id: "aaaa0001".to_string(),
        task: "old".to_string(),
        created_at: 100,
        messages: vec![],
        todos: vec![],
    };
    let new = SessionSnapshot {
        id: "aaaa0002".to_string(),
        task: "new".to_string(),
        created_at: 200,
        messages: vec![],
        todos: vec![],
    };
    store.save(&new).expect("save new");
    store.save(&old).expect("save old");

    let listed = store.list();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "aaaa0002", "newest first");
    assert_eq!(listed[1].id, "aaaa0001");
}

#[test]
fn test_list_skips_bad_files() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());

    store.save(&snapshot("good", &memory_with_messages(), &todos_with_items())).expect("save");

    // A malformed JSON file and a non-JSON file must be skipped.
    std::fs::write(tmp.path().join(".sessions/ffff0001.json"), "{ not json").expect("write bad");
    std::fs::write(tmp.path().join(".sessions/notes.txt"), "hello").expect("write txt");
    std::fs::write(tmp.path().join(".sessions/ffff0002.json"), "[]").expect("write wrong type");

    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].task, "good");
}

#[test]
fn test_list_is_empty_without_directory() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());
    assert!(store.list().is_empty());
}

#[test]
fn test_save_generates_unique_short_ids() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());
    let mut ids = HashSet::new();

    for _ in 0..50 {
        let id =
            store.save(&snapshot("task", &memory_with_messages(), &todos_with_items())).unwrap();
        assert_eq!(id.len(), 8, "ids are 8 hex chars");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(ids.insert(id), "ids must be unique");
    }
}

#[test]
fn test_load_missing_id_errors() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());
    let err = store.load("deadbeef").expect_err("missing session must error");
    assert!(err.to_string().contains("not found"), "error: {err}");
}

#[test]
fn test_load_rejects_invalid_ids() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = store(tmp.path());
    // Path traversal and garbage ids must never escape the .sessions dir.
    for bad in ["", "../outside", "a/b", "id;rm", "session.json"] {
        assert!(store.load(bad).is_err(), "id `{bad}` must be rejected");
    }
}
