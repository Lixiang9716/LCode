//! Unit tests for the teammate LLM agent loop (learn-claude-code s15/s16/
//! s17): a real provider-driven conversation (tool use + final stop), the
//! shutdown handshake correlated by request_id, autonomous task claiming
//! from the board during IDLE, and identity re-injection after context
//! compression.

use lcode::agent::{
    reinject_identity, run_teammate_loop, MessageBus, ProtocolManager, ProtocolState,
    ProtocolStatus, ResponseMatch, TaskManager, TaskStatus, TeamMessage, TeammateEnv,
    TeammateManager, TeammateTools,
};
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmResponse, Role, ToolCallRequest, Usage,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

type SharedProvider = Arc<dyn lcode::llm::LlmProvider>;
type SeenMessages = Arc<Mutex<Vec<Vec<ChatMessage>>>>;
type SeenToolNames = Arc<Mutex<Vec<Vec<String>>>>;

fn response(
    content: &str,
    finish_reason: FinishReason,
    tool_calls: Option<Vec<ToolCallRequest>>,
) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason,
    }
}

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall { name: name.to_string(), arguments: args.to_string() },
    }
}

/// Mock provider serving `responses` from a queue; records every received
/// message batch and the tool definitions of each chat call.
fn mock_with_queue(responses: Vec<LlmResponse>) -> (SharedProvider, SeenMessages, SeenToolNames) {
    let queue: Arc<Mutex<VecDeque<LlmResponse>>> = Arc::new(Mutex::new(VecDeque::from(responses)));
    let seen: SeenMessages = Arc::new(Mutex::new(Vec::new()));
    let seen_tools: SeenToolNames = Arc::new(Mutex::new(Vec::new()));

    let mut mock = MockLlmProvider::new();
    let queue_clone = queue.clone();
    let seen_clone = seen.clone();
    let seen_tools_clone = seen_tools.clone();
    mock.expect_chat().times(0..).returning(move |messages, tools| {
        seen_clone.lock().unwrap().push(messages.to_vec());
        seen_tools_clone
            .lock()
            .unwrap()
            .push(tools.iter().map(|t| t.function.name.clone()).collect());
        let resp =
            queue_clone.lock().unwrap().pop_front().expect("mock provider ran out of responses");
        Ok(resp)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    (Arc::new(mock), seen, seen_tools)
}

/// TeammateEnv over `ws` with a short IDLE cycle (fast tests).
fn env_for(ws: &Path, bus: Arc<MessageBus>, provider: SharedProvider) -> TeammateEnv {
    let protocol = Arc::new(ProtocolManager::new());
    let tasks = Arc::new(Mutex::new(TaskManager::new(ws)));
    let manager = Arc::new(Mutex::new(TeammateManager::new(&ws.to_path_buf())));
    TeammateEnv {
        team_dir: ws.join(".team"),
        tools: TeammateTools::new(ws, manager, bus.clone(), protocol.clone(), tasks.clone()),
        bus,
        provider: Some(provider),
        protocol,
        tasks,
        idle_interval: std::time::Duration::from_millis(10),
        idle_polls: 2,
    }
}

/// Seed `.team/config.json` with the teammate record so the loop's
/// `set_member_state` persists the role (normally `spawn` writes it).
fn seed_roster(ws: &Path, name: &str, role: &str) {
    let config = serde_json::json!({
        "team_name": "default",
        "members": [{ "name": name, "role": role, "state": "working" }]
    });
    std::fs::create_dir_all(ws.join(".team")).unwrap();
    std::fs::write(
        ws.join(".team").join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn teammate_loop_runs_real_llm_conversation() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let bus = Arc::new(MessageBus::new(&ws));

    // Seed one work message; the mock then drives a tool use + a final stop.
    bus.send(&TeamMessage {
        from: "lead".to_string(),
        to: "alice".to_string(),
        msg_type: "text".to_string(),
        request_id: None,
        content: "hello, please summarize team.rs".to_string(),
    })
    .unwrap();

    let (provider, seen, seen_tools) = mock_with_queue(vec![
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call(
                "call-1",
                "send_message",
                serde_json::json!({
                    "to": "lead", "msg_type": "text",
                    "content": "Hello lead, I will summarize."
                }),
            )]),
        ),
        response("Done.", FinishReason::Stop, None),
    ]);
    let env = env_for(&ws, bus.clone(), provider);
    seed_roster(&ws, "alice", "coder");
    run_teammate_loop("alice".to_string(), "coder".to_string(), env).await;

    // The teammate's message and the idle-timeout summary reached the lead.
    let lead_msgs = bus.read_inbox("lead");
    let contents: Vec<&str> = lead_msgs.iter().map(|m| m.content.as_str()).collect();
    assert!(contents.contains(&"Hello lead, I will summarize."));
    assert!(contents.contains(&"Done."));
    assert!(lead_msgs.iter().all(|m| m.from == "alice"), "loop stamps its own name");

    // First chat call: system prompt (identity+role), <identity>, the
    // initial prompt, and the injected <inbox> message.
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2, "tool-use turn, then final stop");
    assert_eq!(calls[0].len(), 4);
    assert_eq!(calls[0][0].role, Role::System);
    assert!(calls[0][0].content.contains("alice"));
    assert!(calls[0][0].content.contains("coder"));
    assert!(calls[0][1].content.contains("<identity>"));
    assert!(calls[0][3].content.contains("<inbox>"));

    // The teammate tool subset is exactly what the model sees.
    let names = &seen_tools.lock().unwrap()[0];
    for expected in [
        "bash",
        "read_file",
        "write_file",
        "send_message",
        "submit_plan",
        "task_list",
        "task_claim",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    assert!(!names.contains(&"spawn_teammate".to_string()));
    assert!(!names.contains(&"read_inbox".to_string()));

    // The loop persisted its final state.
    let roster = TeammateManager::new(&ws).roster();
    assert!(roster.contains("alice (coder): shutdown"), "{roster}");
}

#[tokio::test]
async fn teammate_loop_shutdown_handshake_correlates_request_id() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let bus = Arc::new(MessageBus::new(&ws));

    // The lead requested shutdown (registering the pending request, s16)
    // and the message is already in the inbox when the loop starts.
    let protocol = Arc::new(ProtocolManager::new());
    protocol.register(ProtocolState {
        request_id: "req-shutdown-1".to_string(),
        msg_type: "shutdown_request".to_string(),
        sender: "lead".to_string(),
        target: "alice".to_string(),
        status: ProtocolStatus::Pending,
        payload: String::new(),
    });
    bus.send(&TeamMessage {
        from: "lead".to_string(),
        to: "alice".to_string(),
        msg_type: "shutdown_request".to_string(),
        request_id: Some("req-shutdown-1".to_string()),
        content: "please stop".to_string(),
    })
    .unwrap();

    // The loop must answer the handshake without consulting the model.
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(0);
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    let env = env_for(&ws, bus.clone(), Arc::new(mock));
    seed_roster(&ws, "alice", "coder");
    run_teammate_loop("alice".to_string(), "coder".to_string(), env).await;

    let inbox = bus.read_inbox("lead");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "alice");
    assert_eq!(inbox[0].msg_type, "shutdown_response");
    assert_eq!(inbox[0].request_id.as_deref(), Some("req-shutdown-1"));
    assert_eq!(inbox[0].content, "approved");

    // Lead-side correlation: the request is now approved.
    assert_eq!(protocol.match_response(&inbox[0]), ResponseMatch::Matched { approved: true });

    let roster = TeammateManager::new(&ws).roster();
    assert!(roster.contains("alice (coder): shutdown"), "{roster}");
}

#[tokio::test]
async fn teammate_loop_auto_claims_task_from_board() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let bus = Arc::new(MessageBus::new(&ws));
    let tasks = Arc::new(Mutex::new(TaskManager::new(&ws)));
    tasks.lock().unwrap().create("fix login bug", vec![]).unwrap();

    // Two stops: one before the IDLE scan, one after the auto-claim.
    let (provider, seen, _) = mock_with_queue(vec![
        response("Nothing to do yet.", FinishReason::Stop, None),
        response("Fixing the login bug.", FinishReason::Stop, None),
    ]);
    let env = env_for(&ws, bus.clone(), provider);
    seed_roster(&ws, "alice", "coder");
    run_teammate_loop("alice".to_string(), "coder".to_string(), env).await;

    // The second work batch saw the <auto-claimed> injection (s17).
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].iter().any(|m| m.content.contains("<auto-claimed>Task 1: fix login bug")));

    // The board reflects the claim (owner = teammate, in_progress).
    let board = TaskManager::new(&ws);
    let task = board.get(1).unwrap();
    assert_eq!(task.status, TaskStatus::InProgress);
    assert_eq!(task.owner.as_deref(), Some("alice"));

    // Idle timeout: the final assistant text went to the lead as summary.
    let lead_msgs = bus.read_inbox("lead");
    assert!(lead_msgs.iter().any(|m| m.content == "Fixing the login bug."));

    let roster = TeammateManager::new(&ws).roster();
    assert!(roster.contains("alice (coder): shutdown"), "{roster}");
}

#[test]
fn identity_reinjected_after_context_shrink() {
    let mut messages = vec![
        ChatMessage::system("system"),
        ChatMessage::user("one"),
        ChatMessage::user("two"),
        ChatMessage::user("three"),
        ChatMessage::user("four"),
    ];
    // Full context: no injection.
    reinject_identity(&mut messages, "alice", "coder", 5);
    assert_eq!(messages.len(), 5);

    // Simulated context compression: the list shrinks to 3 entries.
    messages.truncate(3);
    reinject_identity(&mut messages, "alice", "coder", 5);
    assert_eq!(messages.len(), 4);
    assert!(messages[1].content.contains("<identity>"));
    assert!(messages[1].content.contains("alice"));

    // Already present: no duplicate injection.
    reinject_identity(&mut messages, "alice", "coder", 5);
    assert_eq!(messages.len(), 4);
}
