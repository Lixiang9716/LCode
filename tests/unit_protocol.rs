//! Unit tests for team protocols (learn-claude-code s16): the
//! ProtocolState FSM (Pending/Approved/Rejected), `match_response`
//! request_id correlation with type validation and duplicate
//! suppression, `dispatch_message` routing, and the protocol tools
//! (`request_shutdown`, `request_plan`, `review_plan`, `submit_plan`).

use lcode::agent::{
    dispatch_message, parse_plan_verdict, plan_verdict_content, DispatchAction, MessageBus,
    ProtocolManager, ProtocolState, ProtocolStatus, RequestPlanTool, RequestShutdownTool,
    ResponseMatch, ReviewPlanTool, SubmitPlanTool, TeamMessage,
};
use lcode::tools::Tool;
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::tempdir;

fn msg(
    from: &str,
    to: &str,
    msg_type: &str,
    request_id: Option<&str>,
    content: &str,
) -> TeamMessage {
    TeamMessage {
        from: from.to_string(),
        to: to.to_string(),
        msg_type: msg_type.to_string(),
        request_id: request_id.map(String::from),
        content: content.to_string(),
    }
}

fn shutdown_state(request_id: &str) -> ProtocolState {
    ProtocolState {
        request_id: request_id.to_string(),
        msg_type: "shutdown_request".to_string(),
        sender: "lead".to_string(),
        target: "alice".to_string(),
        status: ProtocolStatus::Pending,
        payload: String::new(),
    }
}

fn plan_state(request_id: &str) -> ProtocolState {
    ProtocolState {
        request_id: request_id.to_string(),
        msg_type: "plan_approval_request".to_string(),
        sender: "alice".to_string(),
        target: "lead".to_string(),
        status: ProtocolStatus::Pending,
        payload: "step 1: refactor".to_string(),
    }
}

// ---------------------------------------------------------------------------
// ProtocolState FSM: match_response correlation
// ---------------------------------------------------------------------------

#[test]
fn match_response_correlates_shutdown_response() {
    let manager = ProtocolManager::new();
    manager.register(shutdown_state("req-1"));

    let response = msg("alice", "lead", "shutdown_response", Some("req-1"), "approved");
    assert_eq!(manager.match_response(&response), ResponseMatch::Matched { approved: true });
    assert_eq!(manager.get("req-1").unwrap().status, ProtocolStatus::Approved);
}

#[test]
fn match_response_rejects_unknown_and_wrong_type() {
    let manager = ProtocolManager::new();
    let unknown = msg("alice", "lead", "shutdown_response", Some("nope"), "approved");
    assert_eq!(manager.match_response(&unknown), ResponseMatch::Unknown);

    // A shutdown_response answering a plan request is a type mismatch.
    manager.register(plan_state("req-2"));
    let wrong = msg("alice", "lead", "shutdown_response", Some("req-2"), "approved");
    assert_eq!(
        manager.match_response(&wrong),
        ResponseMatch::TypeMismatch { expected: "plan_approval_response".to_string() }
    );
    // The request stays pending: the mismatch is not an answer.
    assert_eq!(manager.get("req-2").unwrap().status, ProtocolStatus::Pending);
}

#[test]
fn match_response_ignores_duplicates() {
    let manager = ProtocolManager::new();
    manager.register(shutdown_state("req-3"));

    let first = msg("alice", "lead", "shutdown_response", Some("req-3"), "approved");
    assert_eq!(manager.match_response(&first), ResponseMatch::Matched { approved: true });

    // A second response for the same request_id is ignored.
    let second = msg("alice", "lead", "shutdown_response", Some("req-3"), "approved");
    assert_eq!(
        manager.match_response(&second),
        ResponseMatch::Duplicate { status: ProtocolStatus::Approved }
    );
}

#[test]
fn match_response_records_plan_rejection() {
    let manager = ProtocolManager::new();
    manager.register(plan_state("req-4"));

    let verdict = plan_verdict_content(false, "missing tests");
    let response = msg("lead", "alice", "plan_approval_response", Some("req-4"), &verdict);
    assert_eq!(manager.match_response(&response), ResponseMatch::Matched { approved: false });
    assert_eq!(manager.get("req-4").unwrap().status, ProtocolStatus::Rejected);
}

#[test]
fn new_request_ids_are_unique() {
    let manager = ProtocolManager::new();
    let mut seen = HashSet::new();
    for _ in 0..100 {
        assert!(seen.insert(manager.new_request_id()), "request ids must be unique");
    }
    assert!(manager.is_empty());
}

// ---------------------------------------------------------------------------
// dispatch_message routing (teammate side)
// ---------------------------------------------------------------------------

#[test]
fn dispatch_message_answers_shutdown_request() {
    let tmp = tempdir().unwrap();
    let bus = MessageBus::new(&tmp.path().to_path_buf());

    let incoming = msg("lead", "alice", "shutdown_request", Some("req-5"), "please stop");
    assert_eq!(dispatch_message("alice", &bus, &incoming), DispatchAction::Shutdown);

    // The response echoes the request_id (s10/s16 correlation).
    let inbox = bus.read_inbox("lead");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "alice");
    assert_eq!(inbox[0].msg_type, "shutdown_response");
    assert_eq!(inbox[0].request_id.as_deref(), Some("req-5"));
    assert_eq!(inbox[0].content, "approved");
}

#[test]
fn dispatch_message_routes_plan_verdicts() {
    let tmp = tempdir().unwrap();
    let bus = MessageBus::new(&tmp.path().to_path_buf());

    let approved = msg(
        "lead",
        "alice",
        "plan_approval_response",
        Some("req-6"),
        &plan_verdict_content(true, ""),
    );
    assert_eq!(
        dispatch_message("alice", &bus, &approved),
        DispatchAction::PlanNote("[Plan approved] Proceed with the task.".to_string())
    );

    let rejected = msg(
        "lead",
        "alice",
        "plan_approval_response",
        Some("req-6"),
        &plan_verdict_content(false, "needs more tests"),
    );
    assert_eq!(
        dispatch_message("alice", &bus, &rejected),
        DispatchAction::PlanNote("[Plan rejected] Feedback: needs more tests".to_string())
    );
}

#[test]
fn dispatch_message_passes_plain_types_through() {
    let tmp = tempdir().unwrap();
    let bus = MessageBus::new(&tmp.path().to_path_buf());

    for msg_type in ["text", "request", "response", "shutdown_response", "plan_approval_request"] {
        let m = msg("lead", "alice", msg_type, None, "hi");
        assert_eq!(dispatch_message("alice", &bus, &m), DispatchAction::None, "{msg_type}");
    }
}

#[test]
fn parse_plan_verdict_falls_back_to_plain_text() {
    let approved = msg("lead", "alice", "plan_approval_response", None, "Approved - looks good");
    assert_eq!(parse_plan_verdict(&approved), (true, "Approved - looks good".to_string()));

    let rejected = msg("lead", "alice", "plan_approval_response", None, "rewrite it");
    assert_eq!(parse_plan_verdict(&rejected), (false, "rewrite it".to_string()));
}

// ---------------------------------------------------------------------------
// Protocol tools
// ---------------------------------------------------------------------------

#[test]
fn request_shutdown_tool_registers_and_sends() {
    let tmp = tempdir().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    let protocol = Arc::new(ProtocolManager::new());
    let tool = RequestShutdownTool { bus: bus.clone(), protocol: protocol.clone() };

    let result = tool.execute(&serde_json::json!({ "teammate": "alice" })).unwrap();
    assert!(result.success);
    assert!(result.output.contains("req_"));

    let inbox = bus.read_inbox("alice");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "lead");
    assert_eq!(inbox[0].to, "alice");
    assert_eq!(inbox[0].msg_type, "shutdown_request");

    let request_id = inbox[0].request_id.clone().unwrap();
    let state = protocol.get(&request_id).unwrap();
    assert_eq!(state.status, ProtocolStatus::Pending);
    assert_eq!(state.sender, "lead");
    assert_eq!(state.target, "alice");
}

#[test]
fn request_plan_tool_sends_text_message() {
    let tmp = tempdir().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    let tool = RequestPlanTool { bus: bus.clone() };

    let result =
        tool.execute(&serde_json::json!({ "teammate": "alice", "task": "fix login" })).unwrap();
    assert!(result.success);
    assert!(result.output.contains("Asked alice"));

    let inbox = bus.read_inbox("alice");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].msg_type, "text");
    assert!(inbox[0].content.contains("fix login"));
}

#[test]
fn submit_plan_tool_registers_and_sends_request() {
    let tmp = tempdir().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    let protocol = Arc::new(ProtocolManager::new());
    let tool = SubmitPlanTool { bus: bus.clone(), protocol: protocol.clone() };

    let result =
        tool.execute(&serde_json::json!({ "plan": "step 1: refactor", "from": "alice" })).unwrap();
    assert!(result.success);
    assert!(result.output.contains("Waiting for approval"));

    let inbox = bus.read_inbox("lead");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "alice");
    assert_eq!(inbox[0].msg_type, "plan_approval_request");

    let request_id = inbox[0].request_id.clone().unwrap();
    let state = protocol.get(&request_id).unwrap();
    assert_eq!(state.sender, "alice");
    assert_eq!(state.msg_type, "plan_approval_request");
    assert_eq!(state.payload, "step 1: refactor");
    assert_eq!(state.status, ProtocolStatus::Pending);
}

#[test]
fn review_plan_tool_approves_and_sends_response() {
    let tmp = tempdir().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    let protocol = Arc::new(ProtocolManager::new());
    protocol.register(plan_state("req-review-1"));
    let tool = ReviewPlanTool { bus: bus.clone(), protocol: protocol.clone() };

    let result = tool
        .execute(&serde_json::json!({
            "request_id": "req-review-1", "approve": true, "feedback": "go"
        }))
        .unwrap();
    assert!(result.success);
    assert!(result.output.contains("Plan approved"));

    // The verdict is delivered to the plan's author, correlated by request_id.
    let inbox = bus.read_inbox("alice");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].msg_type, "plan_approval_response");
    assert_eq!(inbox[0].request_id.as_deref(), Some("req-review-1"));
    assert!(parse_plan_verdict(&inbox[0]).0);
    assert_eq!(protocol.get("req-review-1").unwrap().status, ProtocolStatus::Approved);
}

#[test]
fn review_plan_tool_rejects_unknown_and_duplicate() {
    let tmp = tempdir().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    let protocol = Arc::new(ProtocolManager::new());
    protocol.register(plan_state("req-review-2"));
    let tool = ReviewPlanTool { bus: bus.clone(), protocol: protocol.clone() };

    // Unknown request_id -> failed tool result.
    let result =
        tool.execute(&serde_json::json!({ "request_id": "req-nope", "approve": true })).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("not found"));

    // Reject once, then a second review is refused (already answered).
    let result = tool
        .execute(&serde_json::json!({ "request_id": "req-review-2", "approve": false }))
        .unwrap();
    assert!(result.success);
    assert!(result.output.contains("Plan rejected"));

    let result = tool
        .execute(&serde_json::json!({ "request_id": "req-review-2", "approve": true }))
        .unwrap();
    assert!(!result.success);
    assert!(result.output.contains("already rejected"));
}

// ---------------------------------------------------------------------------
// Full handshake: request_shutdown -> dispatch -> match_response
// ---------------------------------------------------------------------------

#[test]
fn shutdown_handshake_correlates_end_to_end() {
    let tmp = tempdir().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    let protocol = Arc::new(ProtocolManager::new());
    let request = RequestShutdownTool { bus: bus.clone(), protocol: protocol.clone() };
    request.execute(&serde_json::json!({ "teammate": "alice" })).unwrap();

    // Teammate side: dispatch the shutdown_request.
    let inbox = bus.read_inbox("alice");
    assert_eq!(dispatch_message("alice", &bus, &inbox[0]), DispatchAction::Shutdown);

    // Lead side: correlate the shutdown_response via match_response.
    let responses = bus.read_inbox("lead");
    assert_eq!(responses.len(), 1);
    assert_eq!(protocol.match_response(&responses[0]), ResponseMatch::Matched { approved: true });
    let request_id = responses[0].request_id.clone().unwrap();
    assert_eq!(protocol.get(&request_id).unwrap().status, ProtocolStatus::Approved);
}
