//! Tests for the `lcode session` CLI (G5 — save / list / resume).
//!
//! Covers clap parsing of the `Session` subcommand, app-level routing
//! (empty-task rejection, resume of a missing id), and the save → list →
//! load → resume roundtrip through [`SessionStore`].

use clap::Parser;

use lcode::agent::{ConversationMemory, SessionSnapshot, SessionStore};
use lcode::app::run;
use lcode::cli::{Cli, Command, SessionAction};
use lcode::config::Config;

fn cli_with(action: SessionAction) -> Cli {
    Cli {
        command: Some(Command::Session { action }),
        verbose: false,
        project: ".".to_string(),
        config_file: None,
    }
}

fn run_blocking(cli: Cli, cfg: Config) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new().expect("tokio runtime").block_on(run(cli, cfg))
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

#[test]
fn session_save_parses_task_with_auto_id() {
    let cli = Cli::try_parse_from(["lcode", "session", "save", "fix", "the", "bug"]).unwrap();
    match cli.command {
        Some(Command::Session { action }) => match action {
            SessionAction::Save { task, id } => {
                assert_eq!(task, vec!["fix", "the", "bug"]);
                assert!(id.is_none(), "id is auto-generated when omitted");
            }
            other => panic!("expected Save, got {other:?}"),
        },
        other => panic!("expected Session command, got {other:?}"),
    }
}

#[test]
fn session_save_parses_explicit_id_flag() {
    let cli =
        Cli::try_parse_from(["lcode", "session", "save", "task", "--id", "cafe1234"]).unwrap();
    match cli.command {
        Some(Command::Session { action }) => match action {
            SessionAction::Save { id, .. } => assert_eq!(id.as_deref(), Some("cafe1234")),
            other => panic!("expected Save, got {other:?}"),
        },
        other => panic!("expected Session command, got {other:?}"),
    }
}

#[test]
fn session_list_parses() {
    let cli = Cli::try_parse_from(["lcode", "session", "list"]).unwrap();
    match cli.command {
        Some(Command::Session { action }) => assert!(matches!(action, SessionAction::List)),
        other => panic!("expected Session command, got {other:?}"),
    }
}

#[test]
fn session_resume_parses() {
    let cli = Cli::try_parse_from(["lcode", "session", "resume", "cafe1234"]).unwrap();
    match cli.command {
        Some(Command::Session { action }) => match action {
            SessionAction::Resume { id } => assert_eq!(id, "cafe1234"),
            other => panic!("expected Resume, got {other:?}"),
        },
        other => panic!("expected Session command, got {other:?}"),
    }
}

#[test]
fn session_resume_requires_an_id() {
    assert!(Cli::try_parse_from(["lcode", "session", "resume"]).is_err());
}

// ---------------------------------------------------------------------------
// App routing
// ---------------------------------------------------------------------------

#[test]
fn save_with_empty_task_returns_error() {
    let cli = cli_with(SessionAction::Save { task: vec![], id: None });
    let err = run_blocking(cli, Config::default()).unwrap_err();
    assert!(
        err.to_string().contains("Task description cannot be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn resume_missing_session_returns_error() {
    // Reads the current dir's .sessions (which won't contain this id) —
    // no writes, so this is safe to run anywhere.
    let cli = cli_with(SessionAction::Resume { id: "deadbeef".to_string() });
    let err = run_blocking(cli, Config::default()).unwrap_err();
    assert!(err.to_string().contains("not found"), "unexpected error: {err}");
}

// ---------------------------------------------------------------------------
// Save → list → load → resume roundtrip through the store
// ---------------------------------------------------------------------------

#[test]
fn empty_snapshot_save_list_load_roundtrip() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = SessionStore::new(tmp.path());

    // `session save` semantics: a snapshot with a task and no history yet.
    let id = store.save(&SessionSnapshot::empty("Implement persistence", None)).expect("save");
    assert_eq!(id.len(), 8, "auto ids are 8 hex chars");
    assert!(tmp.path().join(".sessions").join(format!("{id}.json")).is_file());

    // `session list` shows it.
    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].task, "Implement persistence");

    // `session resume` loads it back.
    let loaded = store.load(&id).expect("load");
    assert_eq!(loaded.task, "Implement persistence");
    assert!(loaded.messages.is_empty(), "fresh saves have no conversation");
    assert!(loaded.todos.is_empty());
}

#[test]
fn empty_snapshot_with_explicit_id_is_validated() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = SessionStore::new(tmp.path());

    let id = store
        .save(&SessionSnapshot::empty("task", Some("cafe0001".to_string())))
        .expect("valid hex id saves");
    assert_eq!(id, "cafe0001");

    // Non-hex / path-traversal ids must be rejected at save time too
    // (otherwise `{id}.json` could escape `.sessions`). An empty id is
    // not an error — it means "auto-generate".
    for bad in ["../outside", "a/b", "session.json", "dead.beef"] {
        let err = store
            .save(&SessionSnapshot::empty("task", Some(bad.to_string())))
            .expect_err("bad id must be rejected");
        assert!(err.to_string().contains("invalid session id"), "id `{bad}`: {err}");
    }
}

#[test]
fn resume_builds_memory_with_restored_messages() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let store = SessionStore::new(tmp.path());

    // A snapshot with history, as produced by a previous run.
    let snapshot = SessionSnapshot {
        id: String::new(),
        task: "Continue the work".to_string(),
        created_at: 1,
        messages: vec![
            lcode::llm::ChatMessage::user("Step one done"),
            lcode::llm::ChatMessage::assistant("Continuing"),
        ],
        todos: vec![],
    };
    let id = store.save(&snapshot).expect("save");

    // `session resume` path: load → ConversationMemory::from_messages.
    let loaded = store.load(&id).expect("load");
    let memory = ConversationMemory::from_messages("system prompt".to_string(), loaded.messages);
    let context = memory.get_context();

    assert_eq!(context.len(), 3, "system prompt + 2 restored messages");
    assert_eq!(context[0].content, "system prompt");
    assert_eq!(context[1].content, "Step one done");
    assert_eq!(context[2].content, "Continuing");
}
