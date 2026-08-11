//! Unit tests for the cron module (learn-claude-code s14): expression
//! validation, schedule/list/cancel, durable persistence across restarts,
//! due-job matching with injected clocks, one-shot removal, and the
//! three cron tools.

use lcode::agent::{CancelCronTool, CronScheduler, ListCronsTool, ScheduleCronTool};
use lcode::tools::Tool;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// A scheduler rooted at `tmp` (durable store: `tmp/.scheduled_tasks.json`).
fn scheduler_in(tmp: &TempDir) -> CronScheduler {
    CronScheduler::new(&tmp.path().to_path_buf())
}

// --- expression validation ----------------------------------------------

#[test]
fn test_valid_expressions_are_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    for expression in [
        "* * * * *",
        "*/5 * * * *",
        "0 9 * * 1-5",
        "0,30 8,20 * * *",
        "30 8 1,15 * *",
        "0 0 1 1 *",
        "5 4 * * 0",
    ] {
        let job = scheduler.schedule(expression, "ping", true, false).unwrap();
        assert_eq!(job.expression, expression);
    }
}

#[test]
fn test_invalid_expressions_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    for expression in [
        "0 9 * *",      // 4 fields
        "0 9 * * * *",  // 6 fields
        "60 * * * *",   // minute > 59
        "* 24 * * *",   // hour > 23
        "0 * 0 * *",    // day-of-month 0
        "0 * 32 * *",   // day-of-month > 31
        "0 * * 13 *",   // month > 12
        "0 * * * 7",    // day-of-week > 6
        "bad * * * *",  // not a number
        "*/0 * * * *",  // zero step
        "5-2 * * * *",  // reversed range
        "1-99 * * * *", // range out of bounds
    ] {
        let err = scheduler.schedule(expression, "ping", true, false).unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error for {expression}");
    }
}

// --- schedule / list / cancel -------------------------------------------

#[test]
fn test_schedule_list_cancel_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);

    let first = scheduler.schedule("0 9 * * *", "morning standup", true, false).unwrap();
    let second = scheduler.schedule("30 12 * * 1-5", "lunch reminder", false, false).unwrap();
    assert_ne!(first.id, second.id);
    assert!(first.id.starts_with("cron_"), "ids are short and prefixed: {}", first.id);
    assert!(first.recurring);
    assert!(!second.durable);

    let listing = scheduler.list();
    assert!(
        listing.contains(&format!("{} [0 9 * * *] morning standup (recurring, session)", first.id))
    );
    assert!(listing
        .contains(&format!("{} [30 12 * * 1-5] lunch reminder (one-shot, session)", second.id)));

    scheduler.cancel(&first.id).unwrap();
    let listing = scheduler.list();
    assert!(!listing.contains(&first.id));
    assert!(listing.contains(&second.id));

    let err = scheduler.cancel(&first.id).unwrap_err();
    assert!(err.to_string().contains("not found"), "got: {err}");
}

#[test]
fn test_list_empty_has_helpful_message() {
    let tmp = tempfile::tempdir().unwrap();
    let scheduler = scheduler_in(&tmp);
    assert!(scheduler.list().contains("No cron jobs"));
}

// --- durable persistence -------------------------------------------------

#[test]
fn test_durable_jobs_survive_restart() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let mut scheduler = scheduler_in(&tmp);
        scheduler.schedule("0 9 * * *", "daily build", true, true).unwrap();
        scheduler.schedule("30 12 * * *", "session only", true, false).unwrap();
    }
    // A fresh scheduler over the same directory restores durable jobs.
    let scheduler = scheduler_in(&tmp);
    let listing = scheduler.list();
    assert!(listing.contains("daily build"));
    assert!(!listing.contains("session only"));
}

#[test]
fn test_cancel_persists_durable_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let job_id;
    {
        let mut scheduler = scheduler_in(&tmp);
        job_id = scheduler.schedule("0 9 * * *", "morning", true, true).unwrap().id;
    }
    {
        let mut scheduler = scheduler_in(&tmp);
        scheduler.cancel(&job_id).unwrap();
    }
    let scheduler = scheduler_in(&tmp);
    assert!(!scheduler.list().contains(&job_id));
}

#[test]
fn test_invalid_durable_entries_are_skipped_on_load() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".scheduled_tasks.json"),
        r#"{"tasks":[
            {"id":"cron_000001","expression":"99 99 * * *","prompt":"bad","recurring":true,"durable":true},
            {"id":"cron_000002","expression":"0 9 * * *","prompt":"good","recurring":true,"durable":true}
        ]}"#,
    )
    .unwrap();
    let scheduler = scheduler_in(&tmp);
    let listing = scheduler.list();
    assert!(listing.contains("good"));
    assert!(!listing.contains("bad"), "invalid expression must be skipped");
}

// --- due matching with an injected clock --------------------------------
// `now` tuples are (minute, hour, day-of-month, month, day-of-week),
// day-of-week 0=Sunday..6=Saturday.

#[test]
fn test_due_prompts_match_the_given_minute() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    scheduler.schedule("30 9 15 * *", "monthly deploy", true, false).unwrap();
    scheduler.schedule("0 9 * * *", "daily", true, false).unwrap();

    assert_eq!(scheduler.due_prompts(Some((30, 9, 15, 6, 1))), vec!["monthly deploy"]);
    assert_eq!(scheduler.due_prompts(Some((0, 9, 1, 1, 4))), vec!["daily"]);
    assert!(scheduler.due_prompts(Some((0, 8, 15, 6, 1))).is_empty(), "hour mismatch");
    assert!(scheduler.due_prompts(Some((30, 9, 16, 6, 1))).is_empty(), "dom mismatch");
}

#[test]
fn test_dom_and_dow_use_or_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    scheduler.schedule("0 9 15 * 1", "15th-or-monday", true, false).unwrap();

    // dom=15 matches even though dow=3 (Wednesday).
    assert_eq!(scheduler.due_prompts(Some((0, 9, 15, 6, 3))), vec!["15th-or-monday"]);
    // dow=1 (Monday) matches even though dom=16.
    assert_eq!(scheduler.due_prompts(Some((0, 9, 16, 6, 1))), vec!["15th-or-monday"]);
    // Neither dom nor dow matches.
    assert!(scheduler.due_prompts(Some((0, 9, 16, 6, 3))).is_empty());
}

#[test]
fn test_one_shot_job_removed_after_firing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    scheduler.schedule("* * * * *", "one shot ping", false, false).unwrap();

    let due = scheduler.due_prompts(Some((5, 10, 20, 3, 2)));
    assert_eq!(due, vec!["one shot ping"]);
    assert!(scheduler.list().contains("No cron jobs"), "one-shot removed after firing");

    // A second call in the same minute has nothing left to fire.
    assert!(scheduler.due_prompts(Some((5, 10, 20, 3, 2))).is_empty());
}

#[test]
fn test_recurring_job_fires_at_most_once_per_minute() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    let job = scheduler.schedule("* * * * *", "recurring ping", true, false).unwrap();

    assert_eq!(scheduler.due_prompts(Some((0, 0, 1, 1, 4))), vec!["recurring ping"]);
    assert!(
        scheduler.due_prompts(Some((0, 0, 1, 1, 4))).is_empty(),
        "same minute must not re-fire"
    );
    assert_eq!(scheduler.due_prompts(Some((1, 0, 1, 1, 4))), vec!["recurring ping"]);
    assert!(scheduler.list().contains(&job.id), "recurring job stays after firing");
}

#[test]
fn test_durable_one_shot_removal_is_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let mut scheduler = scheduler_in(&tmp);
        scheduler.schedule("* * * * *", "ephemeral ping", false, true).unwrap();
        scheduler.due_prompts(Some((0, 0, 1, 1, 4)));
    }
    let scheduler = scheduler_in(&tmp);
    assert!(scheduler.list().contains("No cron jobs"), "removal must be on disk");
}

#[test]
fn test_tick_uses_the_real_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let mut scheduler = scheduler_in(&tmp);
    scheduler.schedule("* * * * *", "one shot", false, false).unwrap();

    let due = scheduler.tick();
    assert!(due.contains(&"one shot".to_string()), "star expression always matches now");
    assert!(scheduler.list().contains("No cron jobs"), "one-shot consumed by tick");
}

// --- tools ---------------------------------------------------------------

/// Three tools sharing one scheduler (as `register` wires them up).
fn tool_set(tmp: &TempDir) -> (ScheduleCronTool, ListCronsTool, CancelCronTool) {
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    (
        ScheduleCronTool { scheduler: scheduler.clone() },
        ListCronsTool { scheduler: scheduler.clone() },
        CancelCronTool { scheduler },
    )
}

#[test]
fn test_schedule_cron_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let (schedule_tool, list_tool, cancel_tool) = tool_set(&tmp);

    let result = schedule_tool
        .execute(&serde_json::json!({ "expression": "0 9 * * *", "prompt": "morning" }))
        .unwrap();
    assert!(result.success, "output: {}", result.output);
    assert!(result.output.starts_with("Scheduled cron_"));
    assert!(result.output.contains("morning"));

    // Missing required arguments are hard errors.
    let err = schedule_tool.execute(&serde_json::json!({ "prompt": "no expr" })).unwrap_err();
    assert!(err.to_string().contains("expression"));

    // Invalid expressions surface as tool error results.
    let result = schedule_tool
        .execute(&serde_json::json!({ "expression": "99 * * * *", "prompt": "bad" }))
        .unwrap();
    assert!(!result.success);
    assert!(result.output.contains("minute"));

    // The list tool sees the job through the shared scheduler.
    let listing = list_tool.execute(&serde_json::json!({})).unwrap();
    assert!(listing.output.contains("morning"));

    // Cancel unknown id -> error result; cancel real id -> success.
    let result = cancel_tool.execute(&serde_json::json!({ "id": "nope" })).unwrap();
    assert!(!result.success);
    let id = listing.output.split(' ').next().unwrap();
    let result = cancel_tool.execute(&serde_json::json!({ "id": id })).unwrap();
    assert!(result.success, "output: {}", result.output);
    let listing = list_tool.execute(&serde_json::json!({})).unwrap();
    assert!(listing.output.contains("No cron jobs"));
}
