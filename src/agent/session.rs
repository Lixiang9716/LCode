//! Session persistence (#7 — session save/restore).
//!
//! A session snapshot captures the conversation memory, todo state, and
//! metadata so a run can be resumed or forked later. Snapshots are stored
//! as pretty-printed JSON files under `.sessions/` — one `{id}.json` file
//! per session, mirroring the disk-backed task board pattern (s07).

use crate::agent::{ConversationMemory, TodoManager};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// A session snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    ///
    /// A fresh 8-hex-char id (v4 UUID fragment) is generated when the
    /// snapshot carries none; id collisions with an existing file are
    /// retried so concurrent saves never clobber each other.
    pub fn save(&self, snapshot: &SessionSnapshot) -> anyhow::Result<String> {
        std::fs::create_dir_all(&self.sessions_dir)?;
        let id = if snapshot.id.is_empty() { self.new_id()? } else { snapshot.id.clone() };
        let mut on_disk = snapshot.clone();
        on_disk.id = id.clone();
        let path = self.sessions_dir.join(format!("{id}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&on_disk)?)?;
        Ok(id)
    }

    /// Load a snapshot by id.
    pub fn load(&self, id: &str) -> anyhow::Result<SessionSnapshot> {
        validate_id(id)?;
        let path = self.sessions_dir.join(format!("{id}.json"));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("session `{id}` not found: {e}"))?;
        Ok(serde_json::from_str(&content)?)
    }

    /// List saved sessions, newest first. Files that are unreadable or
    /// fail to parse are skipped.
    pub fn list(&self) -> Vec<SessionSnapshot> {
        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return Vec::new();
        };
        let mut snapshots = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            if let Ok(snapshot) = serde_json::from_str::<SessionSnapshot>(&content) {
                snapshots.push(snapshot);
            }
        }
        snapshots.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        snapshots
    }

    /// Generate an id that does not collide with an existing snapshot.
    fn new_id(&self) -> anyhow::Result<String> {
        loop {
            let full = Uuid::new_v4().simple().to_string();
            let id = full[..8].to_string();
            if !self.sessions_dir.join(format!("{id}.json")).exists() {
                return Ok(id);
            }
        }
    }
}

/// Session ids are short hex strings; reject anything that could escape
/// the sessions directory (e.g. path separators).
fn validate_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid session id: `{id}`");
    }
    Ok(())
}

/// Build a snapshot from a live conversation + todo state.
pub fn snapshot(task: &str, memory: &ConversationMemory, todos: &TodoManager) -> SessionSnapshot {
    SessionSnapshot {
        id: String::new(),
        task: task.to_string(),
        created_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        messages: memory.messages().to_vec(),
        todos: todos.items().to_vec(),
    }
}
