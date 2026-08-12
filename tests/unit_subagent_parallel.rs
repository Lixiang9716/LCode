//! Unit tests for parallel subagents (#11).
//!
//! Drives `run_subagents_parallel` with a mock LLM provider: correct
//! fan-out (labels + summaries), input-order preservation, genuine
//! concurrency, per-subagent failure isolation, and the `task_parallel`
//! tool end to end.

use lcode::agent::{run_subagents_parallel, TaskParallelTool};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, LlmResponse, Usage};
use lcode::tools::{Tool, ToolRegistry};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn response(
    content: &str,
    finish_reason: FinishReason,
    tool_calls: Option<Vec<lcode::llm::ToolCallRequest>>,
) -> LlmResponse {
    LlmResponse { content: content.to_string(), tool_calls, usage: Usage::default(), finish_reason }
}

type SharedProvider = Arc<dyn lcode::llm::LlmProvider>;
type SeenMessages = Arc<Mutex<Vec<Vec<lcode::llm::ChatMessage>>>>;

/// Build a mock provider serving `responses` from a queue; every received
/// message batch is recorded into `seen`.
fn mock_with_queue(responses: Vec<LlmResponse>) -> (SharedProvider, SeenMessages) {
    let queue: Arc<Mutex<VecDeque<LlmResponse>>> = Arc::new(Mutex::new(VecDeque::from(responses)));
    let seen: Arc<Mutex<Vec<Vec<lcode::llm::ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));

    let mut mock = MockLlmProvider::new();
    let queue_clone = queue.clone();
    let seen_clone = seen.clone();
    mock.expect_chat().times(0..).returning(move |messages, _tools| {
        seen_clone.lock().unwrap().push(messages.to_vec());
        let resp =
            queue_clone.lock().unwrap().pop_front().expect("mock provider ran out of responses");
        Ok(resp)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    (Arc::new(mock), seen)
}

fn empty_registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new(&Config::default()).expect("build tool registry"))
}

#[tokio::test]
async fn test_parallel_returns_all_labels_and_summaries() {
    let (provider, seen) = mock_with_queue(vec![
        response("summary one", FinishReason::Stop, None),
        response("summary two", FinishReason::Stop, None),
        response("summary three", FinishReason::Stop, None),
    ]);
    let prompts = vec![
        ("one".to_string(), "first".to_string()),
        ("two".to_string(), "second".to_string()),
        ("three".to_string(), "third".to_string()),
    ];

    let results = run_subagents_parallel(prompts, provider, empty_registry(), 30, None, None).await;

    assert_eq!(results.len(), 3);
    let labels: Vec<&str> = results.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["one", "two", "three"], "labels keep input order");

    // With a shared response queue the label/summary pairing is
    // nondeterministic, but every summary must arrive exactly once.
    let mut summaries: HashSet<&str> = results.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(summaries.len(), 3);
    for expected in ["summary one", "summary two", "summary three"] {
        assert!(summaries.remove(expected), "missing summary {expected}");
    }

    // Each subagent ran with a fresh context: exactly one user message
    // carrying its own prompt.
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 3);
    let mut prompts_seen: HashSet<&str> =
        calls.iter().map(|batch| batch[0].content.as_str()).collect();
    assert_eq!(prompts_seen.len(), 3);
    for prompt in ["first", "second", "third"] {
        assert!(prompts_seen.remove(prompt), "missing prompt {prompt}");
    }
}

#[tokio::test]
async fn test_parallel_preserves_input_order() {
    // Identical responses: the pairing is irrelevant, so the returned
    // (label, summary) pairs must exactly match input order.
    let (provider, _seen) = mock_with_queue(vec![
        response("same", FinishReason::Stop, None),
        response("same", FinishReason::Stop, None),
        response("same", FinishReason::Stop, None),
    ]);
    let prompts = vec![
        ("a".to_string(), "p1".to_string()),
        ("b".to_string(), "p2".to_string()),
        ("c".to_string(), "p3".to_string()),
    ];

    let results = run_subagents_parallel(prompts, provider, empty_registry(), 30, None, None).await;
    let expected: Vec<(String, String)> = vec![
        ("a".to_string(), "same".to_string()),
        ("b".to_string(), "same".to_string()),
        ("c".to_string(), "same".to_string()),
    ];
    assert_eq!(results, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_runs_subagents_concurrently() {
    // Each chat call sleeps 150ms; three subagents serially would take
    // >= 450ms, so finishing well under that proves they overlapped.
    // (A MockLlmProvider cannot demonstrate this: mockall serializes
    // concurrent calls behind its expectation lock, so use a hand-rolled
    // async provider instead.)
    struct SleepingProvider;
    #[async_trait::async_trait]
    impl lcode::llm::LlmProvider for SleepingProvider {
        async fn chat(
            &self,
            _messages: &[lcode::llm::ChatMessage],
            _tools: &[lcode::llm::ToolDefinition],
        ) -> anyhow::Result<LlmResponse> {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(response("slow", FinishReason::Stop, None))
        }
        fn name(&self) -> &str {
            "sleeping"
        }
        fn validate(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }
    let provider: SharedProvider = Arc::new(SleepingProvider);

    let prompts = vec![
        ("x".to_string(), "p".to_string()),
        ("y".to_string(), "p".to_string()),
        ("z".to_string(), "p".to_string()),
    ];
    let start = Instant::now();
    let results = run_subagents_parallel(prompts, provider, empty_registry(), 30, None, None).await;
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 3);
    assert!(elapsed < Duration::from_millis(350), "subagents ran serially? elapsed = {elapsed:?}");
}

#[tokio::test]
async fn test_parallel_isolates_subagent_failures() {
    // The first chat call fails; the other two succeed. The failing
    // subagent must surface its error as text without cancelling its
    // siblings.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut mock = MockLlmProvider::new();
    let calls_clone = calls.clone();
    mock.expect_chat().times(0..).returning(move |_messages, _tools| {
        if calls_clone.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(anyhow::anyhow!("mock provider down"))
        } else {
            Ok(response("ok summary", FinishReason::Stop, None))
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    let provider: SharedProvider = Arc::new(mock);

    let prompts = vec![
        ("p1".to_string(), "one".to_string()),
        ("p2".to_string(), "two".to_string()),
        ("p3".to_string(), "three".to_string()),
    ];
    let results = run_subagents_parallel(prompts, provider, empty_registry(), 30, None, None).await;

    assert_eq!(results.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "every subagent got its turn");
    let failed = results
        .iter()
        .filter(|(_, s)| s.starts_with("(subagent failed: mock provider down"))
        .count();
    let succeeded = results.iter().filter(|(_, s)| s == "ok summary").count();
    assert_eq!(failed, 1, "exactly one subagent failed");
    assert_eq!(succeeded, 2, "the others still produced summaries");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_task_parallel_tool_end_to_end_via_registry() {
    let (provider, _seen) = mock_with_queue(vec![
        response("summary alpha", FinishReason::Stop, None),
        response("summary beta", FinishReason::Stop, None),
    ]);
    let mut registry = ToolRegistry::new(&Config::default()).expect("build tool registry");
    registry.register(Box::new(TaskParallelTool {
        provider,
        registry: empty_registry(),
        events: None,
        hooks: None,
    }));

    let args = serde_json::json!({
        "tasks": [
            { "label": "alpha", "prompt": "do alpha" },
            { "label": "beta", "prompt": "do beta" }
        ]
    });
    let result = registry.execute("task_parallel", &args).expect("tool executes");
    assert!(result.success, "output: {}", result.output);
    assert!(result.output.contains("[alpha]"), "output: {}", result.output);
    assert!(result.output.contains("[beta]"), "output: {}", result.output);
    assert!(result.output.contains("summary alpha"), "output: {}", result.output);
    assert!(result.output.contains("summary beta"), "output: {}", result.output);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_task_parallel_tool_empty_tasks() {
    let (provider, _seen) = mock_with_queue(vec![]);
    let mut registry = ToolRegistry::new(&Config::default()).expect("build tool registry");
    registry.register(Box::new(TaskParallelTool {
        provider,
        registry: empty_registry(),
        events: None,
        hooks: None,
    }));

    let result = registry.execute("task_parallel", &serde_json::json!({ "tasks": [] })).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "(no tasks)");
}

#[test]
fn test_task_parallel_tool_requires_runtime_context() {
    // No tokio runtime on this thread: the synchronous execute must fail
    // cleanly instead of panicking.
    let (provider, _seen) = mock_with_queue(vec![]);
    let tool = TaskParallelTool { provider, registry: empty_registry(), hooks: None, events: None };

    let args = serde_json::json!({ "tasks": [{ "label": "a", "prompt": "p" }] });
    let err = tool.execute(&args).unwrap_err();
    assert!(err.to_string().contains("runtime"), "error: {err}");
}

#[test]
fn test_task_parallel_tool_requires_tasks_argument() {
    let (provider, _seen) = mock_with_queue(vec![]);
    let tool = TaskParallelTool { provider, registry: empty_registry(), hooks: None, events: None };

    assert!(tool.execute(&serde_json::json!({})).is_err());
    assert!(tool.execute(&serde_json::json!({ "tasks": "nope" })).is_err());
    assert!(tool.execute(&serde_json::json!({ "tasks": [{ "label": "a" }] })).is_err());
}

#[test]
fn test_task_parallel_tool_parameters_schema() {
    let (provider, _seen) = mock_with_queue(vec![]);
    let tool = TaskParallelTool { provider, registry: empty_registry(), hooks: None, events: None };

    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert_eq!(params["required"][0], "tasks");
    assert_eq!(params["properties"]["tasks"]["type"], "array");
    let item = &params["properties"]["tasks"]["items"];
    assert_eq!(item["required"][0], "label");
    assert_eq!(item["required"][1], "prompt");
    assert!(item["properties"]["label"]["type"].is_string());
    assert!(params["properties"]["max_turns"]["type"].is_string());
}

/// Each subagent publishes exactly one `SubagentCompleted` (regression:
/// both the loop exit and the parallel wrapper used to publish, doubling
/// the event count).
#[tokio::test]
async fn subagent_completed_published_once_per_subagent() {
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(0..).returning(|_, _| {
        Ok(lcode::llm::LlmResponse {
            content: "done".to_string(),
            tool_calls: None,
            usage: lcode::llm::Usage::default(),
            finish_reason: lcode::llm::FinishReason::Stop,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let prompts =
        vec![("a".to_string(), "task a".to_string()), ("b".to_string(), "task b".to_string())];
    let results = lcode::agent::run_subagents_parallel(
        prompts,
        Arc::new(mock),
        empty_registry(),
        5,
        None,
        Some(tx),
    )
    .await;
    assert_eq!(results.len(), 2);

    let mut completed = 0usize;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, lcode::agent::AgentEvent::SubagentCompleted { .. }) {
            completed += 1;
        }
    }
    assert_eq!(completed, 2, "one SubagentCompleted per subagent");
}
