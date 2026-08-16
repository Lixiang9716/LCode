//! Checkpoint persistence (P1 — interrupted-task resume).
//!
//! A checkpoint captures the conversation, the turn counter, the
//! running usage total and the budget-warning state, so a session
//! killed by max turns, budget, a crash or an interrupt can continue
//! exactly where it stopped — remaining turns and budget stay honest.
//! One latest checkpoint per workspace lives at
//! `.sessions/checkpoint.json`; a completed session deletes it.

use crate::agent::ConversationMemory;
use crate::llm::Usage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Everything needed to resume a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub task: String,
    pub messages: Vec<crate::llm::ChatMessage>,
    pub turns_used: u32,
    pub usage: Usage,
    pub budget_warned: bool,
    pub saved_at: u64,
    /// Absolute workspace path, verified on resume so a checkpoint is
    /// never applied to a different project.
    pub workspace: String,
}

/// The turn/cost state carried into a resumed run.
#[derive(Debug, Clone, Default)]
pub struct RunState {
    pub turns_used: u32,
    pub usage: Usage,
    pub budget_warned: bool,
}

/// Writes the latest checkpoint at the configured turn cadence.
#[derive(Debug, Clone)]
pub struct CheckpointSink {
    store: CheckpointStore,
    task: String,
    workspace: String,
    every: u32,
}

impl CheckpointSink {
    pub fn new(store: CheckpointStore, task: &str, workspace: &Path, every: u32) -> Self {
        Self { store, task: task.to_string(), workspace: workspace.display().to_string(), every }
    }

    /// Is a checkpoint due for this turn count?
    pub fn due(&self, turns_used: u32) -> bool {
        self.every > 0 && turns_used > 0 && turns_used.is_multiple_of(self.every)
    }

    pub fn write(
        &self,
        memory: &ConversationMemory,
        turns_used: u32,
        usage: &Usage,
        budget_warned: bool,
    ) {
        let checkpoint = Checkpoint {
            task: self.task.clone(),
            messages: memory.messages().to_vec(),
            turns_used,
            usage: usage.clone(),
            budget_warned,
            saved_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            workspace: self.workspace.clone(),
        };
        if let Err(e) = self.store.write(&checkpoint) {
            tracing::debug!(error = %e, "checkpoint write failed");
        }
    }
}

/// Single-file checkpoint storage under `.sessions/checkpoint.json`.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    path: PathBuf,
}

impl CheckpointStore {
    pub fn new(workspace: &Path) -> Self {
        Self { path: workspace.join(".sessions").join("checkpoint.json") }
    }

    /// Load the latest checkpoint, if any.
    pub fn load(&self) -> Option<Checkpoint> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Write atomically (temp + rename).
    pub fn write(&self, checkpoint: &Checkpoint) -> anyhow::Result<()> {
        let parent = self.path.parent().expect(".sessions parent");
        std::fs::create_dir_all(parent)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(checkpoint)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Remove the checkpoint (the task completed — nothing to resume).
    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// The checkpoint's workspace field must match the current
    /// directory, otherwise the checkpoint belongs to another project.
    pub fn matches_workspace(&self, checkpoint: &Checkpoint) -> bool {
        std::env::current_dir()
            .map(|cwd| cwd.display().to_string() == checkpoint.workspace)
            .unwrap_or(false)
    }
}
