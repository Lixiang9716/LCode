//! Cron scheduler (learn-claude-code s14).
//!
//! `schedule_cron` / `list_crons` / `cancel_cron` tools turn the agent
//! from a one-shot tool into an always-on assistant: jobs fire on 5-field
//! cron expressions, run while the agent is idle, and durable jobs
//! survive restarts.
//!
//! Firing is pull-based: nothing ticks on its own. [`CronScheduler::tick`]
//! (or [`CronScheduler::due_prompts`] with an injected "now") returns the
//! prompts due at the current minute; the future executor integration
//! calls it when the agent is idle. One-shot jobs are removed after they
//! fire, recurring jobs stay until cancelled.

// The scaffold API takes `&PathBuf` (matching `register`'s skeleton
// signature); keep it, so silence the ptr_arg lint.
#![allow(clippy::ptr_arg)]

use crate::tools::{Tool, ToolResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A scheduled cron job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    /// 5-field cron expression: minute hour day-of-month month day-of-week.
    pub expression: String,
    pub prompt: String,
    pub recurring: bool,
    pub durable: bool,
}

/// Durable store contents: `{"tasks": [...]}`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TasksFile {
    tasks: Vec<CronJob>,
}

/// Clock fields used for matching: (minute, hour, day-of-month, month,
/// day-of-week), with day-of-week 0=Sunday..6=Saturday (cron convention).
pub type CronTime = (u32, u32, u32, u32, u32);

/// Disk-backed cron scheduler.
#[derive(Debug)]
pub struct CronScheduler {
    jobs: HashMap<String, CronJob>,
    /// `.scheduled_tasks.json` — durable jobs survive restarts.
    store: PathBuf,
    /// job id -> last minute it fired, so two ticks in the same minute
    /// don't fire a job twice.
    last_fired: HashMap<String, CronTime>,
    /// Next numeric suffix for short ids (`cron_{:06x}`).
    next_id: u64,
}

impl CronScheduler {
    pub fn new(workspace: &PathBuf) -> Self {
        let mut scheduler = Self {
            jobs: HashMap::new(),
            store: workspace.join(".scheduled_tasks.json"),
            last_fired: HashMap::new(),
            next_id: 1,
        };
        scheduler.load_durable();
        scheduler
    }

    /// Schedule a job; validates the cron expression. Durable jobs are
    /// written to `.scheduled_tasks.json` and restored on restart.
    pub fn schedule(
        &mut self,
        expression: &str,
        prompt: &str,
        recurring: bool,
        durable: bool,
    ) -> anyhow::Result<CronJob> {
        let expression = expression.trim().to_string();
        validate_cron(&expression).map_err(anyhow::Error::msg)?;
        let job = CronJob {
            id: self.allocate_id(),
            expression,
            prompt: prompt.to_string(),
            recurring,
            durable,
        };
        self.jobs.insert(job.id.clone(), job.clone());
        if durable {
            self.save_durable().map_err(|e| anyhow::anyhow!("failed to persist cron job: {e}"))?;
        }
        Ok(job)
    }

    /// List scheduled jobs, one per line.
    pub fn list(&self) -> String {
        if self.jobs.is_empty() {
            return "No cron jobs. Use schedule_cron to add one.".to_string();
        }
        let mut jobs: Vec<&CronJob> = self.jobs.values().collect();
        jobs.sort_by(|a, b| a.id.cmp(&b.id));
        jobs.iter()
            .map(|job| {
                format!(
                    "{} [{}] {} ({}, {})",
                    job.id,
                    job.expression,
                    job.prompt,
                    if job.recurring { "recurring" } else { "one-shot" },
                    if job.durable { "durable" } else { "session" },
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Cancel a job by id; removes it from memory and, when durable, from
    /// the on-disk store.
    pub fn cancel(&mut self, id: &str) -> anyhow::Result<()> {
        let job = self.jobs.remove(id).ok_or_else(|| anyhow::anyhow!("Job {} not found", id))?;
        if job.durable {
            self.save_durable()?;
        }
        Ok(())
    }

    /// Tick once with the real clock: return prompts of due jobs. Future
    /// executor integration calls this when the agent is idle.
    pub fn tick(&mut self) -> Vec<String> {
        self.due_prompts(None)
    }

    /// Return prompts of jobs due at the given minute (default: now).
    /// Non-recurring jobs are removed after firing (durable removals are
    /// persisted); each job fires at most once per minute.
    pub fn due_prompts(&mut self, now: Option<CronTime>) -> Vec<String> {
        let now = now.unwrap_or_else(current_time);
        let mut due = Vec::new();
        let mut fired = Vec::new(); // (id, durable, recurring)
        for (id, job) in &self.jobs {
            if cron_matches(&job.expression, &now) {
                if self.last_fired.get(id) != Some(&now) {
                    due.push(job.prompt.clone());
                }
                fired.push((id.clone(), job.durable, job.recurring));
            }
        }
        let mut persist = false;
        for (id, durable, recurring) in fired {
            self.last_fired.insert(id.clone(), now);
            if !recurring {
                self.jobs.remove(&id);
                persist |= durable;
            }
        }
        if persist && self.save_durable().is_err() {
            tracing::warn!("cron: failed to persist one-shot job removals");
        }
        due
    }

    /// Allocate a unique short id (`cron_{:06x}`).
    fn allocate_id(&mut self) -> String {
        loop {
            let id = format!("cron_{:06x}", self.next_id);
            self.next_id += 1;
            if !self.jobs.contains_key(&id) {
                return id;
            }
        }
    }

    /// Persist durable jobs to `.scheduled_tasks.json` (temp file +
    /// rename so a crash can't truncate the store).
    fn save_durable(&self) -> std::io::Result<()> {
        let durable: Vec<CronJob> = self.jobs.values().filter(|j| j.durable).cloned().collect();
        let data = serde_json::to_vec_pretty(&TasksFile { tasks: durable })
            .map_err(std::io::Error::other)?;
        if let Some(parent) = self.store.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.store.with_extension("json.tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &self.store)
    }

    /// Restore durable jobs from disk; skips invalid expressions so one
    /// bad entry can't poison startup.
    fn load_durable(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.store) else { return };
        let Ok(file) = serde_json::from_str::<TasksFile>(&text) else {
            tracing::warn!("cron: ignoring unreadable {}", self.store.display());
            return;
        };
        for job in file.tasks {
            if validate_cron(&job.expression).is_err() {
                tracing::warn!("cron: skipping invalid job {} ({})", job.id, job.expression);
                continue;
            }
            self.bump_id(&job.id);
            self.jobs.insert(job.id.clone(), job);
        }
    }

    /// Keep id suffixes unique across restarts.
    fn bump_id(&mut self, id: &str) {
        if let Some(suffix) = id.strip_prefix("cron_") {
            if let Ok(n) = u64::from_str_radix(suffix, 16) {
                self.next_id = self.next_id.max(n + 1);
            }
        }
    }
}

// --- Cron expression parsing --------------------------------------------

/// Field ranges for the five cron positions.
const FIELD_BOUNDS: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];
const FIELD_NAMES: [&str; 5] = ["minute", "hour", "day-of-month", "month", "day-of-week"];

/// Validate a 5-field cron expression; returns an error message on failure.
fn validate_cron(expression: &str) -> Result<(), String> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!("expected 5 fields, got {}", fields.len()));
    }
    for ((field, (lo, hi)), name) in fields.iter().zip(FIELD_BOUNDS).zip(FIELD_NAMES) {
        validate_field(field, lo, hi).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(())
}

/// Validate one field: `*`, `*/step`, comma lists, `lo-hi` ranges, or a
/// plain value, all within `[lo, hi]`.
fn validate_field(field: &str, lo: u32, hi: u32) -> Result<(), String> {
    if field == "*" {
        return Ok(());
    }
    if let Some(step) = field.strip_prefix("*/") {
        let step: u32 = step.parse().map_err(|_| format!("invalid step: {field}"))?;
        if step == 0 {
            return Err(format!("step must be > 0: {field}"));
        }
        return Ok(());
    }
    if field.contains(',') {
        return field.split(',').try_for_each(|part| validate_field(part.trim(), lo, hi));
    }
    if let Some((a, b)) = field.split_once('-') {
        let a: u32 = a.trim().parse().map_err(|_| format!("invalid range: {field}"))?;
        let b: u32 = b.trim().parse().map_err(|_| format!("invalid range: {field}"))?;
        if a < lo || a > hi || b < lo || b > hi {
            return Err(format!("range {field} out of bounds [{lo}-{hi}]"));
        }
        if a > b {
            return Err(format!("range start > end: {field}"));
        }
        return Ok(());
    }
    let value: u32 = field.parse().map_err(|_| format!("invalid field: {field}"))?;
    if value < lo || value > hi {
        return Err(format!("value {value} out of bounds [{lo}-{hi}]"));
    }
    Ok(())
}

/// Does `field` match `value`? Supports `*`, `*/step`, lists, and ranges.
fn field_matches(field: &str, value: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        return step.parse::<u32>().map(|s| s > 0 && value.is_multiple_of(s)).unwrap_or(false);
    }
    if field.contains(',') {
        return field.split(',').any(|part| field_matches(part.trim(), value));
    }
    if let Some((a, b)) = field.split_once('-') {
        let (Ok(lo), Ok(hi)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) else {
            return false;
        };
        return lo <= value && value <= hi;
    }
    field.parse::<u32>().map(|v| v == value).unwrap_or(false)
}

/// Standard cron matching with DOM/DOW OR semantics: minute, hour, and
/// month must all match; when both day-of-month and day-of-week are
/// constrained, either one matching is enough.
fn cron_matches(expression: &str, now: &CronTime) -> bool {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let (minute, hour, dom, month, dow) = *now;
    if !field_matches(fields[0], minute) || !field_matches(fields[1], hour) {
        return false;
    }
    if !field_matches(fields[3], month) {
        return false;
    }
    let dom_ok = field_matches(fields[2], dom);
    let dow_ok = field_matches(fields[4], dow);
    match (fields[2] == "*", fields[4] == "*") {
        (true, true) => true,
        (true, false) => dow_ok,
        (false, true) => dom_ok,
        (false, false) => dom_ok || dow_ok,
    }
}

/// Current UTC time as a [`CronTime`].
fn current_time() -> CronTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (_, month, day) = civil_from_days((secs / 86_400) as i64);
    let (hour, minute) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    // 1970-01-01 was a Thursday; with Sunday=0, Thursday = 4.
    let weekday = (((secs / 86_400) % 7) + 4) % 7;
    (minute as u32, hour as u32, day, month, weekday as u32)
}

/// Days since epoch -> (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// --- Tools -------------------------------------------------------------

/// Tool: `schedule_cron`.
pub struct ScheduleCronTool {
    pub scheduler: Arc<Mutex<CronScheduler>>,
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
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "description": "5-field cron" },
                "prompt": { "type": "string", "description": "Message to run when due" },
                "recurring": { "type": "boolean", "description": "true = recurring (default)" },
                "durable": { "type": "boolean", "description": "true = durable (default)" }
            },
            "required": ["expression", "prompt"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let expression = args["expression"].as_str().ok_or_else(|| {
            anyhow::anyhow!("schedule_cron: missing required argument 'expression'")
        })?;
        let prompt = args["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule_cron: missing required argument 'prompt'"))?;
        let recurring = args.get("recurring").and_then(serde_json::Value::as_bool).unwrap_or(true);
        let durable = args.get("durable").and_then(serde_json::Value::as_bool).unwrap_or(true);
        let mut scheduler = self.scheduler.lock().unwrap();
        match scheduler.schedule(expression, prompt, recurring, durable) {
            Ok(job) => Ok(ToolResult::ok(format!(
                "Scheduled {}: '{}' -> {}",
                job.id, job.expression, job.prompt
            ))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `list_crons`.
pub struct ListCronsTool {
    pub scheduler: Arc<Mutex<CronScheduler>>,
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
        let scheduler = self.scheduler.lock().unwrap();
        Ok(ToolResult::ok(scheduler.list()))
    }
}

/// Tool: `cancel_cron`.
pub struct CancelCronTool {
    pub scheduler: Arc<Mutex<CronScheduler>>,
}

impl Tool for CancelCronTool {
    fn name(&self) -> &str {
        "cancel_cron"
    }
    fn description(&self) -> &str {
        "Cancel a scheduled cron job by id."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Job id to cancel" }
            },
            "required": ["id"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("cancel_cron: missing required argument 'id'"))?;
        let mut scheduler = self.scheduler.lock().unwrap();
        match scheduler.cancel(id) {
            Ok(()) => Ok(ToolResult::ok(format!("Cancelled {id}"))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Register this module's tools with the registry. All three tools share
/// a single [`CronScheduler`] so the job list stays consistent.
pub fn register(registry: &mut crate::tools::ToolRegistry, workspace: &PathBuf) {
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(workspace)));
    registry.register(Box::new(ScheduleCronTool { scheduler: scheduler.clone() }));
    registry.register(Box::new(ListCronsTool { scheduler: scheduler.clone() }));
    registry.register(Box::new(CancelCronTool { scheduler }));
}
