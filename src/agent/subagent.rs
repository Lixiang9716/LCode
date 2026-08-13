//! Subagents (learn-claude-code s04).
//!
//! The parent can delegate a subtask through the `task` tool: the
//! subagent runs with a fresh `messages = [user(prompt)]` context (shared
//! filesystem, same tool handlers, no `task` tool → no recursion), and
//! only a text summary comes back as the tool result.
//!
//! Independent subtasks can be fanned out through `task_parallel` (#11):
//! each `(label, prompt)` pair runs `run_subagent` in its own tokio task,
//! and the results come back as `(label, summary)` pairs.

use crate::agent::{HookContext, HookDecision, HookPoint, HookRegistry};
use crate::llm::{ChatMessage, FinishReason};
use crate::tools::{Tool, ToolResult};
use std::sync::Arc;

/// Run a subagent with a fresh context.
///
/// The subagent shares the tool registry but not the conversation;
/// returns the final assistant text (or "(no summary)").
///
/// `hooks` (G12): when present, every subagent tool call passes through
/// the session's PreToolUse hooks first — a `Block` decision cancels the
/// call (the result records the block) exactly like the executor's
/// `handle_tool_call`, so permission policies cannot be bypassed by
/// delegating work to a subagent. `None` keeps the pre-G12 behavior.
/// Monotonic subagent id: `sub-<seq>` unique within a process, so a
/// `SubagentSpawned` event can be correlated with its completion.
fn next_subagent_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("sub-{}", SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

pub async fn run_subagent(
    prompt: &str,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry: &crate::tools::ToolRegistry,
    max_turns: u32,
    hooks: Option<Arc<HookRegistry>>,
    events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
    subagent_cfg: &crate::config::SubagentConfig,
) -> anyhow::Result<String> {
    let subagent_id = next_subagent_id();
    if let Some(tx) = &events {
        let _ = tx.send(crate::agent::AgentEvent::SubagentSpawned {
            id: subagent_id.clone(),
            prompt: prompt.to_string(),
        });
    }
    let (summary, usage) =
        run_subagent_loop(prompt, provider, registry, max_turns, hooks, subagent_cfg).await?;
    if let Some(tx) = &events {
        let _ = tx.send(crate::agent::AgentEvent::SubagentCompleted {
            id: subagent_id,
            summary: summary.clone(),
            usage,
        });
    }
    Ok(summary)
}

/// The subagent turn loop: fresh context, tool use until a final answer
/// or the turn budget runs out.
async fn run_subagent_loop(
    prompt: &str,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry: &crate::tools::ToolRegistry,
    max_turns: u32,
    hooks: Option<Arc<HookRegistry>>,
    subagent_cfg: &crate::config::SubagentConfig,
) -> anyhow::Result<(String, crate::llm::Usage)> {
    let mut messages = vec![ChatMessage::user(prompt.to_string())];
    let tool_defs = registry.definitions();
    let turns = max_turns.clamp(1, subagent_cfg.max_turns);
    let mut total_usage = crate::llm::Usage::default();

    for _ in 0..turns {
        let response = provider.chat(&messages, &tool_defs).await?;
        crate::agent::usage_tracking::accumulate_usage(&mut total_usage, &response.usage);

        if response.finish_reason != FinishReason::ToolCalls {
            // Stop / Length / filter: the final text is the summary.
            let text = response.content.trim();
            let summary =
                if text.is_empty() { "(no summary)".to_string() } else { text.to_string() };
            return Ok((summary, total_usage));
        }

        // Record the assistant message (with its tool calls) so the model
        // can see its own reasoning in the next turn.
        let tool_calls = response.tool_calls.clone().unwrap_or_default();
        let mut assistant_msg = ChatMessage::assistant(response.content.clone());
        assistant_msg.tool_calls = Some(tool_calls.clone());
        messages.push(assistant_msg);

        // Execute every requested tool call and backfill the results.
        for tc in &tool_calls {
            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or_default();

            // PreToolUse hooks: a Block decision cancels the call (G12).
            if let Some(registry_hooks) = &hooks {
                let ctx = HookContext {
                    point: HookPoint::PreToolUse,
                    tool_name: Some(tc.function.name.clone()),
                    tool_args: Some(args.clone()),
                    prompt: None,
                };
                if let HookDecision::Block { reason } = registry_hooks.run(&ctx) {
                    let blocked = format!("Tool call blocked by hook: {}", reason);
                    messages.push(ChatMessage::tool(blocked, tc.id.clone()));
                    continue;
                }
            }

            let output = match registry.execute(&tc.function.name, &args) {
                Ok(result) => format!("{result}"),
                Err(e) => format!("Error: {e}"),
            };
            let output = crate::agent::background::truncate_chars(
                &output,
                subagent_cfg.max_tool_result_chars,
            );
            messages.push(ChatMessage::tool(output, tc.id.clone()));
        }
    }

    // No final answer within the turn budget.
    Ok(("(no summary)".to_string(), total_usage))
}

/// Run several subagents in parallel (fan-out, #11).
///
/// Each `(label, prompt)` pair runs `run_subagent` in its own tokio task
/// with a fresh context; results come back as `(label, summary)` pairs in
/// input order. A subagent that errors surfaces its error text as the
/// summary so one failure does not cancel its siblings.
pub async fn run_subagents_parallel(
    prompts: Vec<(String, String)>,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry: Arc<crate::tools::ToolRegistry>,
    max_turns: u32,
    hooks: Option<Arc<HookRegistry>>,
    events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
    subagent_cfg: crate::config::SubagentConfig,
) -> Vec<(String, String)> {
    let labels: Vec<String> = prompts.iter().map(|(label, _)| label.clone()).collect();
    let handles: Vec<tokio::task::JoinHandle<anyhow::Result<String>>> = prompts
        .into_iter()
        .map(|(_label, prompt)| {
            let provider = provider.clone();
            let registry = registry.clone();
            let hooks = hooks.clone();
            let events = events.clone();
            let subagent_cfg = subagent_cfg.clone();
            tokio::spawn(async move {
                run_subagent(&prompt, provider, &registry, max_turns, hooks, events, &subagent_cfg)
                    .await
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for (handle, label) in handles.into_iter().zip(labels) {
        let summary = match handle.await {
            Ok(Ok(summary)) => summary,
            Ok(Err(e)) => format!("(subagent failed: {e})"),
            Err(e) => format!("(subagent task failed: {e})"),
        };
        results.push((label, summary));
    }
    results
}

/// Tool: `task` — delegate a subtask to a subagent (parent only).
pub struct TaskTool {
    pub provider: Arc<dyn crate::llm::LlmProvider>,
    pub registry: Arc<crate::tools::ToolRegistry>,
    /// Session PreToolUse hooks, forwarded to the subagent (G12).
    pub hooks: Option<Arc<HookRegistry>>,
    /// Session event bus; publishes `SubagentCompleted` when the child
    /// returns (spawn events come from `run_subagent` itself).
    pub events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
    /// User-tunable subagent limits (max turns, tool result size).
    pub subagent_cfg: crate::config::SubagentConfig,
}

impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a subagent with a fresh context. The \
         subagent shares the filesystem and tools but has no conversation \
         history; only its summary is returned. Do not use for trivial \
         file operations."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The subtask to delegate to the subagent"
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Maximum subagent turns (default: 30)"
                }
            },
            "required": ["prompt"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt =
            args["prompt"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'prompt' argument"))?;
        let max_turns =
            args["max_turns"].as_u64().unwrap_or(self.subagent_cfg.max_turns as u64) as u32;

        // The `Tool` trait is synchronous but subagents are async; the
        // executor calls tools from inside the async loop, so block on
        // the current runtime handle. `block_in_place` temporarily moves
        // this worker out of the scheduler, which makes `block_on` legal
        // (see the design note in the scaffold: keep `Tool` as-is, block
        // on a runtime handle).
        tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("task tool requires a tokio runtime context"))?;
        let summary = tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(run_subagent(
                prompt,
                self.provider.clone(),
                &self.registry,
                max_turns,
                self.hooks.clone(),
                self.events.clone(),
                &self.subagent_cfg,
            ))
        })
        .map_err(|e| anyhow::anyhow!("subagent failed: {e}"))?;
        Ok(ToolResult::ok(summary))
    }
}

/// Register this module's tools with the registry.
///
/// `hooks` (G12): the session's PreToolUse hook registry, shared with the
/// subagents these tools spawn; `None` keeps the pre-G12 behavior.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry_ref: Arc<crate::tools::ToolRegistry>,
    hooks: Option<Arc<HookRegistry>>,
    events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
    subagent_cfg: crate::config::SubagentConfig,
) {
    registry.register(Box::new(TaskTool {
        provider: provider.clone(),
        registry: registry_ref.clone(),
        hooks: hooks.clone(),
        events: events.clone(),
        subagent_cfg: subagent_cfg.clone(),
    }));
    registry.register(Box::new(TaskParallelTool {
        provider,
        registry: registry_ref,
        hooks,
        events,
        subagent_cfg,
    }));
}

/// Tool: `task_parallel` — fan out independent subtasks to subagents that
/// run concurrently (parent only, #11).
pub struct TaskParallelTool {
    pub provider: Arc<dyn crate::llm::LlmProvider>,
    pub registry: Arc<crate::tools::ToolRegistry>,
    /// Session PreToolUse hooks, forwarded to the subagents (G12).
    pub hooks: Option<Arc<HookRegistry>>,
    /// Session event bus; publishes `SubagentCompleted` per result.
    pub events: Option<tokio::sync::broadcast::Sender<crate::agent::AgentEvent>>,
    /// User-tunable subagent limits (max turns, tool result size).
    pub subagent_cfg: crate::config::SubagentConfig,
}

impl Tool for TaskParallelTool {
    fn name(&self) -> &str {
        "task_parallel"
    }

    fn description(&self) -> &str {
        "Delegate several independent subtasks to subagents that run in \
         parallel. Each subtask is a {\"label\", \"prompt\"} pair; the \
         result lists one \"[label] summary\" line per subtask. Use when \
         subtasks do not depend on each other — do not use for a single \
         task, and never use with subagents that touch the same files."
    }

    fn parameters(&self) -> serde_json::Value {
        let task = serde_json::json!({
            "type": "object",
            "properties": {
                "label": { "type": "string", "description": "Short name for the subtask" },
                "prompt": { "type": "string", "description": "The subtask to delegate" }
            },
            "required": ["label", "prompt"]
        });
        serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": task,
                    "description": "Independent subtasks to run in parallel"
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Maximum subagent turns (default: 30)"
                }
            },
            "required": ["tasks"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(tasks_val) = args.get("tasks") else {
            return Err(anyhow::anyhow!("Missing 'tasks' argument"));
        };
        let tasks = parse_tasks(tasks_val)?;
        let max_turns =
            args["max_turns"].as_u64().unwrap_or(self.subagent_cfg.max_turns as u64) as u32;

        // Same synchronous-tool-over-async-engine pattern as `task`
        // (block_on through the current runtime handle).
        tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("task_parallel tool requires a tokio runtime context"))?;
        let results = tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(run_subagents_parallel(
                tasks,
                self.provider.clone(),
                self.registry.clone(),
                max_turns,
                self.hooks.clone(),
                self.events.clone(),
                self.subagent_cfg.clone(),
            ))
        });
        let output = if results.is_empty() {
            "(no tasks)".to_string()
        } else {
            results
                .iter()
                .map(|(label, summary)| format!("[{label}] {summary}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ToolResult::ok(output))
    }
}

/// Parse the `tasks` array into `(label, prompt)` pairs.
fn parse_tasks(value: &serde_json::Value) -> anyhow::Result<Vec<(String, String)>> {
    let items = value.as_array().ok_or_else(|| anyhow::anyhow!("'tasks' must be an array"))?;
    let mut tasks = Vec::with_capacity(items.len());
    for item in items {
        let label = item["label"].as_str().unwrap_or("task").to_string();
        let prompt = item["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("each task needs a 'prompt' string"))?;
        tasks.push((label, prompt.to_string()));
    }
    Ok(tasks)
}
