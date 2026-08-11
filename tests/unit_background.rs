//! Unit tests for background tasks (learn-claude-code s08).
//!
//! Exercises the `BackgroundManager` lifecycle: spawn/drain/check, safety
//! validation, timeout handling, event publishing, and the
//! `background_run` / `background_check` tools.

use lcode::agent::{AgentEvent, BackgroundCheckTool, BackgroundManager, BackgroundRunTool};
use lcode::config::Config;
use lcode::tools::ToolRegistry;
use std::sync::Arc;
use std::time::Duration;

/// Drain notifications until at least one arrives (the spawned task runs
/// concurrently) or the deadline expires.
async fn wait_for_notification(manager: &BackgroundManager, timeout: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let notifications = manager.drain_notifications();
        if !notifications.is_empty() {
            return notifications;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no background notification arrived within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn test_spawn_echo_completes_and_notifies() {
    let manager = Arc::new(BackgroundManager::new(&Config::default()).unwrap());

    let id = manager.spawn("echo hello background", 10).unwrap();
    assert_eq!(id.len(), 8, "task ids are short (8 chars)");

    // The completion notification is a "{id} [{status}] {command}\n{result}"
    // ping that the loop drains before the next LLM call.
    let notifications = wait_for_notification(&manager, Duration::from_secs(10)).await;
    assert!(notifications[0].contains(&id), "notification: {}", notifications[0]);
    assert!(notifications[0].contains("[completed]"), "notification: {}", notifications[0]);
    assert!(notifications[0].contains("echo hello background"));
    assert!(notifications[0].contains("hello"));

    // The full result is available through check.
    let full = manager.check(Some(&id));
    assert!(full.contains("completed"), "check: {full}");
    assert!(full.contains("hello background"), "check: {full}");
    assert!(full.contains("hello"), "check: {full}");
}

#[tokio::test]
async fn test_check_lists_all_tasks_and_rejects_unknown_id() {
    let manager = Arc::new(BackgroundManager::new(&Config::default()).unwrap());
    let id = manager.spawn("echo task-a", 10).unwrap();
    let _ = wait_for_notification(&manager, Duration::from_secs(10)).await;

    // No id: a listing of every task.
    let listing = manager.check(None);
    assert!(listing.contains(&id), "listing: {listing}");
    assert!(listing.contains("echo task-a"), "listing: {listing}");

    // Unknown id: explicit error, like the tutorial.
    assert!(manager.check(Some("deadbeef")).contains("Unknown task deadbeef"));
}

#[tokio::test]
async fn test_dangerous_command_rejected() {
    let manager = Arc::new(BackgroundManager::new(&Config::default()).unwrap());

    // Config deny list ("sudo") and built-in destructive patterns.
    let err = manager.spawn("sudo echo hi", 10).unwrap_err();
    assert!(err.to_string().contains("blocked"), "error: {err}");
    assert!(manager.spawn("rm -rf /", 10).is_err());
    assert!(manager.spawn("dd if=/dev/zero of=/dev/sda", 10).is_err());

    // Rejected commands never enter the task table.
    assert_eq!(manager.check(None), "No background tasks.");
}

#[test]
fn test_default_manager_rejects_dangerous_commands() {
    // A default-constructed manager (no config) still refuses the
    // classics via the built-in fallback patterns; this rejects before
    // any runtime context is needed.
    let manager = Arc::new(BackgroundManager::default());
    let err = manager.spawn("shutdown -h now", 10).unwrap_err();
    assert!(err.to_string().contains("blocked"), "error: {err}");
    assert!(manager.spawn("sudo rm -rf /", 10).is_err());
    assert!(manager.spawn(":(){ :|:& };:", 10).is_err());
}

#[test]
fn test_spawn_outside_runtime_returns_error() {
    let manager = Arc::new(BackgroundManager::new(&Config::default()).unwrap());
    // No tokio runtime on this thread: spawn must fail cleanly without
    // recording a task.
    let err = manager.spawn("echo hi", 10).unwrap_err();
    assert!(err.to_string().contains("runtime"), "error: {err}");
    assert_eq!(manager.check(None), "No background tasks.");
}

#[tokio::test]
async fn test_spawn_timeout_kills_command() {
    let manager = Arc::new(BackgroundManager::new(&Config::default()).unwrap());
    let id = manager.spawn("sleep 5", 1).unwrap();

    let notifications = wait_for_notification(&manager, Duration::from_secs(10)).await;
    assert!(notifications[0].contains("[timeout]"), "notification: {}", notifications[0]);
    assert!(notifications[0].contains("Timeout"), "notification: {}", notifications[0]);
    assert!(manager.check(Some(&id)).contains("Timeout"), "check: {}", manager.check(Some(&id)));
}

#[tokio::test]
async fn test_events_published_on_start_and_completion() {
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let manager = Arc::new(
        BackgroundManager::new(&Config::default())
            .unwrap()
            .with_events(tx),
    );

    let id = manager.spawn("echo event test", 10).unwrap();

    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Ok(AgentEvent::BackgroundTaskStarted { id: event_id, command })) => {
            assert_eq!(event_id, id);
            assert_eq!(command, "echo event test");
        }
        other => panic!("expected BackgroundTaskStarted event, got: {other:?}"),
    }
    match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
        Ok(Ok(AgentEvent::BackgroundTaskCompleted { id: event_id, status, output })) => {
            assert_eq!(event_id, id);
            assert_eq!(status, "completed");
            assert!(output.contains("event test"), "output: {output}");
        }
        other => panic!("expected BackgroundTaskCompleted event, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_background_tools_execute() {
    let manager = Arc::new(BackgroundManager::new(&Config::default()).unwrap());
    let mut registry = ToolRegistry::new(&Config::default()).unwrap();
    registry.register(Box::new(BackgroundRunTool { manager: manager.clone() }));
    registry.register(Box::new(BackgroundCheckTool { manager: manager.clone() }));

    // background_run returns the id immediately.
    let result = registry
        .execute(
            "background_run",
            &serde_json::json!({ "command": "echo tool ran", "timeout": 10 }),
        )
        .unwrap();
    assert!(result.success);
    assert!(result.output.contains("started"), "output: {}", result.output);

    let _ = wait_for_notification(&manager, Duration::from_secs(10)).await;

    // background_check returns the full result.
    let check = registry.execute("background_check", &serde_json::json!({})).unwrap();
    assert!(check.success);
    assert!(check.output.contains("completed"), "output: {}", check.output);
    assert!(check.output.contains("tool ran"), "output: {}", check.output);
}
