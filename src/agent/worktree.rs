//! Worktree task isolation (learn-claude-code s12).
//!
//! Control plane and execution plane are separated: the task board says
//! WHAT to do, git worktrees say WHERE to do it, bound by task_id.
//! Every lifecycle mutation emits before/after/failed events into
//! `.worktrees/events.jsonl` as a persistent audit stream.
//!
//! Index format: `.worktrees/index.json` maps `name` → `{task_id, state}`
//! with state `active` → `removed` (tombstone) on close-out.

// The scaffold API takes `&PathBuf` (matching `register`'s skeleton
// signature); keep it, so silence the ptr_arg lint.
#![allow(clippy::ptr_arg)]

use crate::agent::event::AgentEvent;
use crate::tools::{Tool, ToolResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Persistent append-only event log (JSONL).
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(workspace: &PathBuf) -> Self {
        Self { path: workspace.join(".worktrees").join("events.jsonl") }
    }

    /// Append an event line; tolerant of write failures.
    ///
    /// Writes `{"event":..,"task_id":..,"worktree":..,"error":..}` to
    /// `.worktrees/events.jsonl`, creating parent directories on demand.
    pub fn emit(&self, event: &str, task_id: u32, worktree: Option<&str>, error: Option<&str>) {
        let line = serde_json::json!({
            "event": event,
            "task_id": task_id,
            "worktree": worktree,
            "error": error,
        });
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file =
                std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
            writeln!(file, "{}", line)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::debug!(error = %e, event, "Failed to append worktree event");
        }
    }
}

/// One row of `.worktrees/index.json`: `name` → `{task_id, state}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    task_id: u32,
    /// `active` or `removed` (tombstone).
    state: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    worktrees: HashMap<String, IndexEntry>,
}

/// Manages git worktrees bound to tasks.
#[derive(Debug)]
pub struct WorktreeManager {
    workspace: PathBuf,
    worktrees_dir: PathBuf,
    log: EventLog,
    /// Runtime event bus publisher. Tools can't reach the runtime, so this
    /// is wired via [`Self::set_events`] by the caller when a runtime
    /// exists; publishing is skipped while `None`.
    events: Option<broadcast::Sender<AgentEvent>>,
}

impl WorktreeManager {
    pub fn new(workspace: &PathBuf) -> Self {
        Self {
            workspace: workspace.clone(),
            worktrees_dir: workspace.join(".worktrees"),
            log: EventLog::new(workspace),
            events: None,
        }
    }

    /// Attach the runtime event bus so lifecycle events are also published
    /// to observers (`WorktreeCreated` / `WorktreeRemoved`).
    pub fn set_events(&mut self, events: broadcast::Sender<AgentEvent>) {
        self.events = Some(events);
    }

    /// Worktree names must match `[A-Za-z0-9._-]{1,40}`.
    fn validate_name(name: &str) -> anyhow::Result<()> {
        static NAME_RE: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
        let re = NAME_RE.get_or_init(|| {
            regex_lite::Regex::new(r"^[A-Za-z0-9._-]{1,40}$").expect("static regex is valid")
        });
        if !re.is_match(name) {
            anyhow::bail!(
                "Invalid worktree name '{}': use 1-40 chars of letters, numbers, ., _, -",
                name
            );
        }
        Ok(())
    }

    fn index_path(&self) -> PathBuf {
        self.worktrees_dir.join("index.json")
    }

    fn load_index(&self) -> anyhow::Result<IndexFile> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(IndexFile::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    fn save_index(&self, index: &IndexFile) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.worktrees_dir)?;
        std::fs::write(self.index_path(), serde_json::to_string_pretty(index)?)?;
        Ok(())
    }

    /// Insert (or update) the index row for `name` — name → {task_id, state}.
    fn record_state(&self, name: &str, task_id: u32, state: &str) -> anyhow::Result<()> {
        let mut index = self.load_index()?;
        index.worktrees.insert(name.to_string(), IndexEntry { task_id, state: state.to_string() });
        self.save_index(&index)
    }

    fn find_entry(&self, name: &str) -> anyhow::Result<Option<IndexEntry>> {
        Ok(self.load_index()?.worktrees.get(name).cloned())
    }

    fn publish(&self, event: AgentEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event);
        }
    }

    /// Run `git` with `cwd` = the workspace root; error on non-zero exit.
    fn run_git<I, S>(&self, args: I) -> anyhow::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&self.workspace)
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to run git: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            anyhow::bail!(
                "git command failed: {}",
                if stderr.is_empty() { "(no output)".to_string() } else { stderr }
            );
        }
        Ok(())
    }

    /// Create a worktree `wt/{name}` for a task: emit create.before →
    /// `git worktree add -b wt/{name} {workspace}/.worktrees/{name} HEAD` →
    /// index.json → emit create.after; on failure emit create.failed.
    pub fn create(&self, name: &str, task_id: u32) -> anyhow::Result<PathBuf> {
        Self::validate_name(name)?;
        if self.find_entry(name)?.is_some() {
            anyhow::bail!("Worktree '{}' already exists in index", name);
        }
        self.log.emit("worktree.create.before", task_id, Some(name), None);
        let path = self.worktrees_dir.join(name);
        let branch = format!("wt/{}", name);
        let result = (|| -> anyhow::Result<()> {
            self.run_git([
                "worktree",
                "add",
                "-b",
                branch.as_str(),
                path.to_string_lossy().as_ref(),
                "HEAD",
            ])?;
            self.record_state(name, task_id, "active")
        })();
        match result {
            Ok(()) => {
                self.log.emit("worktree.create.after", task_id, Some(name), None);
                self.publish(AgentEvent::WorktreeCreated { name: name.to_string(), task_id });
                Ok(path)
            }
            Err(e) => {
                self.log.emit("worktree.create.failed", task_id, Some(name), Some(&e.to_string()));
                Err(e)
            }
        }
    }

    /// Remove a worktree; optionally complete the bound task.
    ///
    /// `complete_task` is reserved for the task board (task module): this
    /// module owns the execution plane only, so callers complete the bound
    /// task through the task tools. Removal leaves a `removed` tombstone in
    /// index.json (task_id preserved).
    pub fn remove(&self, name: &str, complete_task: bool) -> anyhow::Result<()> {
        let entry =
            self.find_entry(name)?.ok_or_else(|| anyhow::anyhow!("Unknown worktree '{}'", name))?;
        let _ = complete_task;
        let path = self.worktrees_dir.join(name);
        self.log.emit("worktree.remove.before", entry.task_id, Some(name), None);
        let result = (|| -> anyhow::Result<()> {
            self.run_git(["worktree", "remove", "--force", path.to_string_lossy().as_ref()])?;
            self.record_state(name, entry.task_id, "removed")
        })();
        match result {
            Ok(()) => {
                self.log.emit("worktree.remove.after", entry.task_id, Some(name), None);
                self.publish(AgentEvent::WorktreeRemoved { name: name.to_string() });
                Ok(())
            }
            Err(e) => {
                self.log.emit(
                    "worktree.remove.failed",
                    entry.task_id,
                    Some(name),
                    Some(&e.to_string()),
                );
                Err(e)
            }
        }
    }

    /// Run a command inside the worktree.
    ///
    /// The command is safety-checked with the shell tool's deny list and
    /// dangerous-pattern checks, then executed via `sh -c` with `cwd` set
    /// to the worktree directory and a hard 300s timeout.
    pub fn run(&self, name: &str, command: &str) -> anyhow::Result<String> {
        let shell = crate::tools::shell::ShellTool::new_with_root(self.workspace.clone());
        shell.check_safety(command)?;

        let entry =
            self.find_entry(name)?.ok_or_else(|| anyhow::anyhow!("Unknown worktree '{}'", name))?;
        if entry.state != "active" {
            anyhow::bail!("Worktree '{}' is not active (state: {})", name, entry.state);
        }
        let path = self.worktrees_dir.join(name);
        if !path.is_dir() {
            anyhow::bail!("Worktree path missing: {}", path.display());
        }
        run_command_in_dir(&path, command)
    }
}

/// Spawn `sh -c <command>` in `dir` with a 300s deadline; stdout and stderr
/// are drained on reader threads so large output cannot deadlock the pipe.
fn run_command_in_dir(dir: &Path, command: &str) -> anyhow::Result<String> {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let out_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(anyhow::anyhow!("Failed to wait on command: {}", e)),
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Command timed out (300s)");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let output = format!("{}{}", stdout.trim(), stderr.trim());
    if !status.success() {
        let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".to_string());
        anyhow::bail!("Command failed (exit {}): {}", code, output);
    }
    Ok(output)
}

// --- Tools -------------------------------------------------------------

/// Tool: `worktree_create`.
pub struct WorktreeCreateTool {
    pub manager: Arc<WorktreeManager>,
}

impl Tool for WorktreeCreateTool {
    fn name(&self) -> &str {
        "worktree_create"
    }
    fn description(&self) -> &str {
        "Create a git worktree for a task (branch wt/{name}). The task \
         moves to in_progress. Commands run inside the worktree are \
         isolated from the main workspace."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Worktree name (1-40 chars: letters, numbers, ., _, -)"
                },
                "task_id": { "type": "integer", "description": "Bound task id" }
            },
            "required": ["name", "task_id"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name =
            args["name"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;
        let task_id = args["task_id"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' argument"))?;
        let task_id = u32::try_from(task_id).unwrap_or(u32::MAX);
        match self.manager.create(name, task_id) {
            Ok(path) => {
                Ok(ToolResult::ok(format!("Created worktree '{}' at {}", name, path.display())))
            }
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `worktree_run`.
pub struct WorktreeRunTool {
    pub manager: Arc<WorktreeManager>,
}

impl Tool for WorktreeRunTool {
    fn name(&self) -> &str {
        "worktree_run"
    }
    fn description(&self) -> &str {
        "Run a command inside a task's worktree."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Worktree name" },
                "command": { "type": "string", "description": "Shell command to run" }
            },
            "required": ["name", "command"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name =
            args["name"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;
        match self.manager.run(name, command) {
            Ok(output) => {
                let output = if output.is_empty() { "(no output)".to_string() } else { output };
                Ok(ToolResult::ok(output))
            }
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `worktree_remove`.
pub struct WorktreeRemoveTool {
    pub manager: Arc<WorktreeManager>,
}

impl Tool for WorktreeRemoveTool {
    fn name(&self) -> &str {
        "worktree_remove"
    }
    fn description(&self) -> &str {
        "Remove a task's worktree; optionally mark the task completed."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Worktree name" },
                "complete_task": {
                    "type": "boolean",
                    "description": "Mark the bound task completed (reserved; task board owns completion)"
                }
            },
            "required": ["name"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name =
            args["name"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;
        let complete_task = args["complete_task"].as_bool().unwrap_or(false);
        match self.manager.remove(name, complete_task) {
            Ok(()) => Ok(ToolResult::ok(format!("Removed worktree '{}'", name))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let manager = Arc::new(WorktreeManager::new(workspace));
    registry.register(Box::new(WorktreeCreateTool { manager: manager.clone() }));
    registry.register(Box::new(WorktreeRunTool { manager: manager.clone() }));
    registry.register(Box::new(WorktreeRemoveTool { manager }));
}
