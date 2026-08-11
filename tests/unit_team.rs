//! Unit tests for the team module (learn-claude-code s09-s11): message
//! bus round-trips, type whitelist, roster persistence, the s10 shutdown
//! handshake, and the basic-version teammate loop (tool echo, no LLM).

use lcode::agent::{MessageBus, TeamMessage, TeammateManager, TeammateState, VALID_MSG_TYPES};
use std::path::Path;
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

// ---------------------------------------------------------------------------
// MessageBus
// ---------------------------------------------------------------------------

#[test]
fn bus_send_read_roundtrip() {
    let tmp = tempdir().unwrap();
    let bus = MessageBus::new(&tmp.path().to_path_buf());
    bus.send(&msg("lead", "alice", "text", None, "hello alice")).unwrap();

    let inbox = bus.read_inbox("alice");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "lead");
    assert_eq!(inbox[0].to, "alice");
    assert_eq!(inbox[0].msg_type, "text");
    assert_eq!(inbox[0].content, "hello alice");

    // Drain-on-read: a second read is empty.
    assert!(bus.read_inbox("alice").is_empty());
    // Missing inboxes read as empty.
    assert!(bus.read_inbox("nobody").is_empty());
}

#[test]
fn bus_rejects_invalid_msg_type() {
    let tmp = tempdir().unwrap();
    let bus = MessageBus::new(&tmp.path().to_path_buf());
    let err = bus.send(&msg("lead", "alice", "bogus_type", None, "hi")).unwrap_err();
    assert!(err.to_string().contains("Invalid message type 'bogus_type'"));

    // Every whitelisted type is accepted.
    for t in VALID_MSG_TYPES {
        bus.send(&msg("lead", "alice", t, None, "hi")).unwrap();
    }
    assert_eq!(bus.read_inbox("alice").len(), VALID_MSG_TYPES.len());
}

#[test]
fn bus_broadcast_excludes_sender() {
    let tmp = tempdir().unwrap();
    let bus = MessageBus::new(&tmp.path().to_path_buf());
    let members: Vec<String> = ["lead", "alice", "bob"].iter().map(|s| s.to_string()).collect();
    bus.broadcast("lead", &members, &msg("lead", "", "text", None, "hi team")).unwrap();

    assert_eq!(bus.read_inbox("alice").len(), 1);
    assert_eq!(bus.read_inbox("bob").len(), 1);
    assert!(bus.read_inbox("lead").is_empty(), "broadcast must skip the sender");
}

// ---------------------------------------------------------------------------
// TeammateManager: roster persistence (config.json)
// ---------------------------------------------------------------------------

#[test]
fn roster_persists_across_instances() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();

    let mut manager = TeammateManager::new(&ws);
    let alice = manager.spawn("alice", "coder").unwrap();
    assert_eq!(alice.state, TeammateState::Working);
    manager.spawn("bob", "reviewer").unwrap();

    // A fresh manager reloads the roster from .team/config.json.
    let reloaded = TeammateManager::new(&ws);
    let roster = reloaded.roster();
    assert!(roster.contains("alice (coder): working"));
    assert!(roster.contains("bob (reviewer): working"));
    assert!(tmp.path().join(".team").join("config.json").exists());
}

#[test]
fn spawn_rejects_busy_and_reuses_idle() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();

    let mut manager = TeammateManager::new(&ws);
    manager.spawn("alice", "coder").unwrap();
    let err = manager.spawn("alice", "coder").unwrap_err();
    assert!(err.to_string().contains("currently"));

    // Simulate the loop persisting an idle state, then spawn reuses it.
    let config = serde_json::json!({
        "team_name": "default",
        "members": [{ "name": "alice", "role": "coder", "state": "idle" }]
    });
    let path = ws.join(".team").join("config.json");
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut manager = TeammateManager::new(&ws);
    let reused = manager.spawn("alice", "coder").unwrap();
    assert_eq!(reused.state, TeammateState::Working);
    assert!(manager.roster().contains("alice (coder): working"));
}

#[test]
fn spawn_requires_name() {
    let tmp = tempdir().unwrap();
    let mut manager = TeammateManager::new(&tmp.path().to_path_buf());
    assert!(manager.spawn("", "coder").is_err());
}

// ---------------------------------------------------------------------------
// s10 shutdown handshake + basic loop (tool echo, no LLM)
// ---------------------------------------------------------------------------

/// Spawn a teammate with a pre-seeded inbox, then poll the lead's inbox
/// until a `msg_type` reply arrives (bounded by `deadline`).
async fn wait_for_reply(
    bus: &MessageBus,
    deadline: std::time::Duration,
    msg_type: &str,
) -> TeamMessage {
    let start = std::time::Instant::now();
    loop {
        let inbox = bus.read_inbox("lead");
        if let Some(reply) = inbox.into_iter().find(|m| m.msg_type == msg_type) {
            return reply;
        }
        assert!(start.elapsed() < deadline, "timed out waiting for '{}' in lead inbox", msg_type);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

async fn wait_for_state(ws: &Path, expected: &str, deadline: std::time::Duration) {
    let start = std::time::Instant::now();
    loop {
        let manager = TeammateManager::new(&ws.to_path_buf());
        if manager.roster().contains(expected) {
            return;
        }
        assert!(start.elapsed() < deadline, "timed out waiting for roster state '{}'", expected);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn teammate_loop_answers_shutdown_request() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let bus = Arc::new(MessageBus::new(&ws));

    // Seed the shutdown request before spawning so the loop's first poll
    // handles it immediately (no 5s idle wait).
    bus.send(&msg("lead", "alice", "shutdown_request", Some("req-1"), "please stop")).unwrap();

    let mut manager = TeammateManager::new(&ws);
    manager.spawn("alice", "coder").unwrap();

    let reply = wait_for_reply(&bus, std::time::Duration::from_secs(5), "shutdown_response").await;
    assert_eq!(reply.from, "alice");
    // The response echoes the request_id (s10 correlation).
    assert_eq!(reply.request_id.as_deref(), Some("req-1"));
    assert_eq!(reply.content, "approved");

    // The loop exits and persists state `shutdown`.
    wait_for_state(&ws, "alice (coder): shutdown", std::time::Duration::from_secs(5)).await;
}

#[tokio::test]
async fn teammate_loop_echoes_messages_and_runs_mini_tools() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let bus = Arc::new(MessageBus::new(&ws));

    // A send_message command via the mini-tool.
    let cmd = serde_json::json!({ "tool": "send_message", "to": "bob", "content": "hi bob" });
    bus.send(&msg("lead", "alice", "request", Some("r1"), &cmd.to_string())).unwrap();

    let mut manager = TeammateManager::new(&ws);
    manager.spawn("alice", "coder").unwrap();

    let reply = wait_for_reply(&bus, std::time::Duration::from_secs(5), "response").await;
    assert_eq!(reply.from, "alice");
    assert_eq!(reply.request_id.as_deref(), Some("r1"));
    assert!(reply.content.contains("sent text to bob"));

    // The mini-tool forwarded the message to bob's inbox.
    let bob_inbox = bus.read_inbox("bob");
    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].from, "alice");
    assert_eq!(bob_inbox[0].content, "hi bob");
}

#[tokio::test]
async fn teammate_loop_echoes_plain_text() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path().to_path_buf();
    let bus = Arc::new(MessageBus::new(&ws));

    bus.send(&msg("lead", "alice", "text", None, "what is the weather?")).unwrap();

    let mut manager = TeammateManager::new(&ws);
    manager.spawn("alice", "coder").unwrap();

    let reply = wait_for_reply(&bus, std::time::Duration::from_secs(5), "response").await;
    assert_eq!(reply.content, "[alice] what is the weather?");
}
