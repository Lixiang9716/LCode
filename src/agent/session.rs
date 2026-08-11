//! Session persistence (#7 — session save/restore).
//!
//! A session snapshot captures the conversation memory, todo state, and
//! metadata so a run can be resumed or forked later.

use crate::agent::{ConversationMemory, TodoManager};
use std::path::{Path, PathBuf};

/// A session snapshot.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub id: String,
    pub task: String,
    pub created_at: u64,
    pub messages: Vec<crate::llm::ChatMessage>,
    pub todos: Vec<crate::agent::TodoItem>,
}

/// Stores session snapshots as JSON files under `.sessions/`.
#[derive(Debug)]
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn new(workspace: &Path) -> Self {
        Self { sessions_dir: workspace.join(".sessions") }
    }

    /// Save a snapshot; returns the session id.
    pub fn save(&self, snapshot: &SessionSnapshot) -> anyhow::Result<String> {
        // TODO(#7): mkdir, write {id}.json; id = short uuid or timestamp.
        let _ = snapshot;
        anyhow::bail!("session.save not implemented yet")
    }

    /// Load a snapshot by id.
    pub fn load(&self, id: &str) -> anyhow::Result<SessionSnapshot> {
        // TODO(#7): read + parse {id}.json.
        let _ = id;
        anyhow::bail!("session.load not implemented yet")
    }

    /// List saved sessions (id, task, created_at).
    pub fn list(&self) -> Vec<SessionSnapshot> {
        // TODO(#7): scan .sessions/*.json.
        Vec::new()
    }
}

/// Build a snapshot from a live conversation + todo state.
pub fn snapshot(
    task: &str,
    memory: &ConversationMemory,
    todos: &TodoManager,
) -> SessionSnapshot {
    SessionSnapshot {
        id: String::new(),
        task: task.to_string(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        messages: memory.messages().to_vec(),
        todos: Vec::new(), // TODO(#7): expose TodoManager items.
    }
}
