//! Cron scheduler (learn-claude-code s14).
//!
//! `schedule_cron` / `list_crons` / `cancel_cron` tools turn the agent
//! from a one-shot tool into an always-on assistant: jobs fire on 5-field
//! cron expressions, run while the agent is idle, and durable jobs
//! survive restarts.

use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;

/// A scheduled cron job.
#[derive(Debug, Clone)]
pub struct CronJob {
    pub id: String,
    /// 5-field cron expression: minute hour day-of-month month day-of-week.
    pub expression: String,
    pub prompt: String,
    pub recurring: bool,
    pub durable: bool,
}

/// Disk-backed cron scheduler.
#[derive(Debug)]
pub struct CronScheduler {
    jobs_dir: PathBuf,
}

impl CronScheduler {
    pub fn new(workspace: &PathBuf) -> Self {
        Self { jobs_dir: workspace.join(".scheduled_tasks") }
    }

    /// Schedule a job; validates the cron expression.
    pub fn schedule(
        &mut self,
        expression: &str,
        prompt: &str,
        recurring: bool,
        durable: bool,
    ) -> anyhow::Result<CronJob> {
        // TODO(s14): validate 5-field cron; persist durable jobs to
        // .scheduled_tasks.json; return the job with a short id.
        let _ = (expression, prompt, recurring, durable);
        anyhow::bail!("cron.schedule not implemented yet")
    }

    /// List scheduled jobs.
    pub fn list(&self) -> String {
        // TODO(s14): one line per job: id, expression, prompt, flags.
        String::new()
    }

    /// Cancel a job by id.
    pub fn cancel(&mut self, id: &str) -> anyhow::Result<()> {
        // TODO(s14): remove from memory + durable store.
        let _ = id;
        Ok(())
    }

    /// Tick once: return prompts of due jobs.
    pub fn due_prompts(&mut self) -> Vec<String> {
        // TODO(s14): match minute/hour/day fields with OR semantics for
        // DOM/DOW; non-recurring jobs are removed after firing.
        Vec::new()
    }
}

// --- Tools -------------------------------------------------------------

/// Tool: `schedule_cron`.
pub struct ScheduleCronTool {
    pub scheduler: std::sync::Mutex<CronScheduler>,
}

impl Tool for ScheduleCronTool {
    fn name(&self) -> &str {
        "schedule_cron"
    }
    fn description(&self) -> &str {
        "Schedule a prompt to run on a 5-field cron expression \
         (minute hour day-of-month month day-of-week). Durable jobs \
         survive restarts."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s14): { expression, prompt, recurring?, durable? }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("schedule_cron not implemented yet"))
    }
}

/// Tool: `list_crons`.
pub struct ListCronsTool {
    pub scheduler: std::sync::Mutex<CronScheduler>,
}

impl Tool for ListCronsTool {
    fn name(&self) -> &str {
        "list_crons"
    }
    fn description(&self) -> &str {
        "List scheduled cron jobs."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("list_crons not implemented yet"))
    }
}

/// Tool: `cancel_cron`.
pub struct CancelCronTool {
    pub scheduler: std::sync::Mutex<CronScheduler>,
}

impl Tool for CancelCronTool {
    fn name(&self) -> &str {
        "cancel_cron"
    }
    fn description(&self) -> &str {
        "Cancel a scheduled cron job by id."
    }
    fn parameters(&self) -> serde_json::Value {
        // TODO(s14): { id: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("cancel_cron not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let scheduler = std::sync::Mutex::new(CronScheduler::new(workspace));
    registry.register(Box::new(ScheduleCronTool { scheduler: std::sync::Mutex::new(CronScheduler::new(workspace)) }));
    registry.register(Box::new(ListCronsTool { scheduler: std::sync::Mutex::new(CronScheduler::new(workspace)) }));
    registry.register(Box::new(CancelCronTool { scheduler }));
}
