//! Event-publish wiring tests: every `AgentEvent` variant must actually
//! be published by the tool/agent path that owns it, so the audit log
//! (`.transcripts/events_*.jsonl`) records the full session.
//!
//! Covers the six events that historically had renderer branches but no
//! publisher: `TodoUpdated`, `TaskCreated`, `TaskUpdated`, `SkillLoaded`,
//! `SubagentSpawned` and `SubagentCompleted`.

use lcode::agent::{
    LoadSkillTool, SkillRegistry, TaskCreateTool, TaskManager, TaskUpdateTool, TodoManager,
    TodoUpdateTool,
};
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, LlmResponse, Usage};
use lcode::tools::Tool;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::sync::broadcast;

/// A receiver that collects every event published on a fresh channel.
fn collector(
) -> (broadcast::Sender<lcode::agent::AgentEvent>, broadcast::Receiver<lcode::agent::AgentEvent>) {
    broadcast::channel(16)
}

/// Consume the next event, asserting it matches the expected variant tag.
fn assert_event(
    rx: &mut broadcast::Receiver<lcode::agent::AgentEvent>,
    tag: &str,
) -> serde_json::Value {
    let event = rx.try_recv().expect("an event was published");
    let value = serde_json::to_value(&event).expect("event serializes");
    assert!(value.get(tag).is_some(), "expected {tag}, got {value}");
    value
}

#[test]
fn todo_update_publishes_todo_updated() {
    let (tx, mut rx) = collector();
    let manager = Arc::new(Mutex::new(TodoManager::default()));
    let tool = TodoUpdateTool { manager: manager.clone(), events: Some(tx) };

    let result = tool
        .execute(&serde_json::json!({
            "items": [{ "text": "write tests", "status": "pending" }]
        }))
        .expect("todo_update executes");
    assert!(result.success, "update succeeds: {}", result.output);

    let value = assert_event(&mut rx, "TodoUpdated");
    let items = value["TodoUpdated"]["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "write tests");
}

#[test]
fn task_create_and_update_publish_events() {
    let tmp = tempdir().unwrap();
    let (tx, mut rx) = collector();
    let manager = Arc::new(Mutex::new(TaskManager::new(tmp.path())));
    let create = TaskCreateTool { manager: manager.clone(), events: Some(tx.clone()) };
    let update = TaskUpdateTool { manager, events: Some(tx) };

    create.execute(&serde_json::json!({ "title": "ship it" })).expect("create executes");
    let created = assert_event(&mut rx, "TaskCreated");
    assert_eq!(created["TaskCreated"]["title"], "ship it");
    let id = created["TaskCreated"]["id"].as_u64().expect("task id");

    update
        .execute(&serde_json::json!({ "id": id, "status": "in_progress" }))
        .expect("update executes");
    let updated = assert_event(&mut rx, "TaskUpdated");
    assert_eq!(updated["TaskUpdated"]["id"], id);
    assert_eq!(updated["TaskUpdated"]["status"], "in_progress");
}

#[test]
fn load_skill_publishes_skill_loaded() {
    let tmp = tempdir().unwrap();
    let skills = tmp.path().join("skills");
    std::fs::create_dir_all(skills.join("fmt")).unwrap();
    std::fs::write(skills.join("fmt").join("SKILL.md"), "# fmt\n\nRun the formatter.\n").unwrap();

    let (tx, mut rx) = collector();
    let mut registry = SkillRegistry::default();
    registry.load_from(&skills).expect("skill loads");
    let tool = LoadSkillTool { registry: Arc::new(Mutex::new(registry)), events: Some(tx) };

    tool.execute(&serde_json::json!({ "name": "fmt" })).expect("load_skill executes");
    let value = assert_event(&mut rx, "SkillLoaded");
    assert_eq!(value["SkillLoaded"]["name"], "fmt");
}

#[tokio::test]
async fn subagent_publishes_spawned_and_completed() {
    let (tx, mut rx) = collector();

    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_, _| {
        Ok(LlmResponse {
            content: "done".to_string(),
            tool_calls: None,
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let registry =
        lcode::tools::ToolRegistry::new(&lcode::config::Config::default()).expect("tool registry");
    let summary = lcode::agent::run_subagent(
        "summarize",
        Arc::new(mock),
        &registry,
        30,
        None,
        Some(tx.clone()),
    )
    .await
    .expect("subagent runs");
    assert_eq!(summary, "done");

    assert_event(&mut rx, "SubagentSpawned");
    let completed = assert_event(&mut rx, "SubagentCompleted");
    assert_eq!(completed["SubagentCompleted"]["summary"], "done");
    // No further events: the stream is quiet after completion.
    assert!(rx.try_recv().is_err(), "no extra events after SubagentCompleted");
}
