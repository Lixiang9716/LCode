//! Unit tests for the worktree module (learn-claude-code s12): event log
//! append, git worktree create/remove against a temp git repo, the index
//! tombstone, command execution, and name validation.

use lcode::agent::{AgentEvent, EventLog, WorktreeManager};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

/// Run a git command in `dir`; panic on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Init a git repo in `dir` with one commit on `main`.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "initial"]);
}

fn manager(ws: &Path) -> WorktreeManager {
    WorktreeManager::new(&ws.to_path_buf())
}

fn read_events(ws: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(ws.join(".worktrees").join("events.jsonl")).unwrap();
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn event_names(ws: &Path) -> Vec<String> {
    read_events(ws).iter().map(|v| v["event"].as_str().unwrap().to_string()).collect()
}

fn index_entry(ws: &Path, name: &str) -> serde_json::Value {
    let index: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join(".worktrees").join("index.json")).unwrap(),
    )
    .unwrap();
    index["worktrees"][name].clone()
}

// ---------------------------------------------------------------------------
// EventLog
// ---------------------------------------------------------------------------

#[test]
fn event_log_emits_and_appends() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let log = EventLog::new(&ws);

    log.emit("worktree.create.before", 7, Some("alpha"), None);
    log.emit("worktree.create.failed", 7, Some("alpha"), Some("boom"));

    let text = std::fs::read_to_string(ws.join(".worktrees").join("events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "events must append, one JSON line per event");

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["event"], "worktree.create.before");
    assert_eq!(first["task_id"], 7);
    assert_eq!(first["worktree"], "alpha");
    assert!(first["error"].is_null());

    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["event"], "worktree.create.failed");
    assert_eq!(second["error"], "boom");
}

// ---------------------------------------------------------------------------
// WorktreeManager::create
// ---------------------------------------------------------------------------

#[test]
fn create_makes_worktree_branch_and_index() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let m = manager(&ws);
    let path: PathBuf = m.create("feature-x", 42).unwrap();

    assert!(path.is_dir(), "worktree directory must exist");
    assert_eq!(path, ws.join(".worktrees").join("feature-x"));
    // The worktree checks out the same commit.
    assert_eq!(std::fs::read_to_string(path.join("README.md")).unwrap(), "hello\n");
    // Branch wt/{name} exists.
    assert!(git(&ws, &["branch", "--list", "wt/feature-x"]).contains("wt/feature-x"));

    // index.json: name → {task_id, state: active}.
    let entry = index_entry(&ws, "feature-x");
    assert_eq!(entry["task_id"], 42);
    assert_eq!(entry["state"], "active");

    // Lifecycle events were emitted in order.
    let names = event_names(&ws);
    assert!(names.contains(&"worktree.create.before".to_string()));
    assert!(names.contains(&"worktree.create.after".to_string()));
    assert!(!names.contains(&"worktree.create.failed".to_string()));
}

#[test]
fn create_rejects_invalid_and_duplicate_names() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let m = manager(&ws);
    // Name regex [A-Za-z0-9._-]{1,40}.
    let invalid = ["", "has space", "sla/sh", "über", "x!y", &"x".repeat(41)];
    for name in invalid {
        let err = m.create(name, 1).unwrap_err();
        assert!(
            err.to_string().contains("Invalid worktree name"),
            "name {:?} should be rejected, got: {}",
            name,
            err
        );
    }

    m.create("feature-x", 1).unwrap();
    let err = m.create("feature-x", 2).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn create_publishes_runtime_event() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let mut m = manager(&ws);
    m.set_events(tx);
    m.create("feature-x", 42).unwrap();

    match rx.try_recv().unwrap() {
        AgentEvent::WorktreeCreated { name, task_id } => {
            assert_eq!(name, "feature-x");
            assert_eq!(task_id, 42);
        }
        other => panic!("expected WorktreeCreated, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// WorktreeManager::remove
// ---------------------------------------------------------------------------

#[test]
fn remove_leaves_tombstone() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let m = manager(&ws);
    m.create("feature-x", 42).unwrap();
    m.remove("feature-x", false).unwrap();

    assert!(
        !ws.join(".worktrees").join("feature-x").exists(),
        "worktree directory must be removed"
    );
    // Tombstone: task_id preserved, state → removed.
    let entry = index_entry(&ws, "feature-x");
    assert_eq!(entry["task_id"], 42);
    assert_eq!(entry["state"], "removed");

    let names = event_names(&ws);
    assert!(names.contains(&"worktree.remove.before".to_string()));
    assert!(names.contains(&"worktree.remove.after".to_string()));
}

#[test]
fn remove_unknown_worktree_errors() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let m = manager(&ws);
    let err = m.remove("ghost", false).unwrap_err();
    assert!(err.to_string().contains("Unknown worktree 'ghost'"));
}

// ---------------------------------------------------------------------------
// WorktreeManager::run
// ---------------------------------------------------------------------------

#[test]
fn run_executes_in_worktree_with_safety_checks() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let m = manager(&ws);
    m.create("feature-x", 1).unwrap();

    // Commands run with cwd inside the worktree.
    let out = m.run("feature-x", "pwd").unwrap();
    assert!(
        out.contains(".worktrees/feature-x"),
        "pwd should print the worktree path, got: {}",
        out
    );
    // The worktree sees the repo state.
    let out = m.run("feature-x", "git branch --show-current").unwrap();
    assert_eq!(out, "wt/feature-x");

    // Non-zero exit → error.
    assert!(m.run("feature-x", "exit 3").is_err());
    // Dangerous command blocked by the shell tool's safety check.
    let err = m.run("feature-x", "rm -rf /").unwrap_err();
    assert!(err.to_string().contains("blocked"));
    // Unknown worktree → error.
    assert!(m.run("ghost", "pwd").is_err());
}

// ---------------------------------------------------------------------------
// register: event-bus wiring (G14)
// ---------------------------------------------------------------------------

#[test]
fn register_wires_runtime_events_to_create_and_remove() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    init_repo(&ws);

    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let mut registry = lcode::tools::ToolRegistry::new(&lcode::config::Config::default()).unwrap();
    lcode::agent::register_worktree_tools(&mut registry, &ws, Some(tx));

    // Executing the registered tools publishes WorktreeCreated / Removed.
    let result = registry
        .execute("worktree_create", &serde_json::json!({ "name": "feature-x", "task_id": 42 }))
        .unwrap();
    assert!(result.success, "output: {}", result.output);
    match rx.try_recv().unwrap() {
        AgentEvent::WorktreeCreated { name, task_id } => {
            assert_eq!(name, "feature-x");
            assert_eq!(task_id, 42);
        }
        other => panic!("expected WorktreeCreated, got {:?}", other),
    }

    let result =
        registry.execute("worktree_remove", &serde_json::json!({ "name": "feature-x" })).unwrap();
    assert!(result.success, "output: {}", result.output);
    match rx.try_recv().unwrap() {
        AgentEvent::WorktreeRemoved { name } => assert_eq!(name, "feature-x"),
        other => panic!("expected WorktreeRemoved, got {:?}", other),
    }
}

#[test]
fn register_without_events_still_registers_tools() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let mut registry = lcode::tools::ToolRegistry::new(&lcode::config::Config::default()).unwrap();
    lcode::agent::register_worktree_tools(&mut registry, &ws, None);
    assert!(registry.list_tools().contains(&"worktree_create"));
    assert!(registry.list_tools().contains(&"worktree_run"));
    assert!(registry.list_tools().contains(&"worktree_remove"));
}
