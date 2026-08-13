//! Phase B tests: read_file/write_file URL mode + permission checks
//! (host policy, allowed_dirs, network approval gate).

use lcode::config::{Config, RuntimeTuning, ToolsConfig};
use lcode::llm::{FinishReason, FunctionCall, LlmResponse, ToolCallRequest, Usage};
use lcode::tools::file::{ReadFileTool, WriteFileTool};
use lcode::tools::{Tool, ToolRegistry};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Tool settings that allow fetching from the local wiremock server
/// (the default denied_hosts blocks 127.0.0.1 by design).
fn test_tools_config() -> ToolsConfig {
    ToolsConfig { denied_hosts: Vec::new(), fetch_timeout_secs: 15, ..ToolsConfig::default() }
}

async fn file_server(body: &[u8]) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    server
}

// --- URL fetch: happy path ---

#[tokio::test]
async fn write_file_url_fetches_and_writes_atomically() {
    let server = file_server(b"hello from wiremock").await;
    let dir = tempfile::TempDir::new().unwrap();
    let tool =
        WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), test_tools_config());
    let url = format!("{}/data.bin", server.uri());

    let result = tool.execute(&serde_json::json!({ "path": "fetched.bin", "url": url })).unwrap();
    assert!(result.success, "{}", result.output);
    assert_eq!(std::fs::read(dir.path().join("fetched.bin")).unwrap(), b"hello from wiremock");
    // Atomic write leaves no temp files behind.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "no temp files left: {leftovers:?}");
}

#[tokio::test]
async fn read_file_url_returns_content_with_numbers() {
    let server = file_server(b"line one\nline two\n").await;
    let dir = tempfile::TempDir::new().unwrap();
    let tool =
        ReadFileTool::new_with_root_and_config(dir.path().to_path_buf(), test_tools_config());
    let url = format!("{}/data.bin", server.uri());

    let result = tool.execute(&serde_json::json!({ "path": url })).unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("Fetched 18 bytes"), "{}", result.output);
    assert!(result.output.contains("1\tline one"));
    assert!(result.output.contains("2\tline two"));
}

#[tokio::test]
async fn write_file_url_404_is_an_error() {
    let server = file_server(b"x").await;
    let dir = tempfile::TempDir::new().unwrap();
    let tool =
        WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), test_tools_config());
    let url = format!("{}/missing", server.uri());

    let err = tool.execute(&serde_json::json!({ "path": "out.bin", "url": url })).unwrap_err();
    assert!(err.to_string().contains("404"), "{err}");
    assert!(!dir.path().join("out.bin").exists());
    std::mem::forget(server);
}

// --- URL fetch: limits and gating ---

#[tokio::test]
async fn fetch_over_size_cap_aborts_and_cleans_up() {
    let server = file_server(&vec![b'x'; 2048]).await;
    let dir = tempfile::TempDir::new().unwrap();
    let config = ToolsConfig { max_fetch_bytes: 512, ..test_tools_config() };
    let tool = WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), config);
    let url = format!("{}/data.bin", server.uri());

    let err = tool.execute(&serde_json::json!({ "path": "out.bin", "url": url })).unwrap_err();
    assert!(err.to_string().contains("max_fetch_bytes"), "{err}");
    assert!(!dir.path().join("out.bin").exists());
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("tmp-"))
        .count();
    assert_eq!(leftovers, 0, "temp file must be cleaned up");
}

#[test]
fn file_scheme_rejected_without_network() {
    let dir = tempfile::TempDir::new().unwrap();
    let tool =
        WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), test_tools_config());

    let err = tool
        .execute(&serde_json::json!({ "path": "out.txt", "url": "file:///etc/passwd" }))
        .unwrap_err();
    assert!(err.to_string().contains("http/https"), "{err}");
    assert!(!dir.path().join("out.txt").exists());
}

#[test]
fn enable_web_false_blocks_url_modes() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = ToolsConfig { enable_web: false, ..test_tools_config() };
    let read = ReadFileTool::new_with_root_and_config(dir.path().to_path_buf(), config.clone());
    let write = WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), config);

    let err = read.execute(&serde_json::json!({ "path": "https://example.com/x" })).unwrap_err();
    assert!(err.to_string().contains("enable_web"), "{err}");

    let err = write
        .execute(&serde_json::json!({ "path": "x", "url": "https://example.com/y" }))
        .unwrap_err();
    assert!(err.to_string().contains("enable_web"), "{err}");
}

// --- host policy ---

#[tokio::test]
async fn denied_hosts_blocks_loopback_by_default() {
    let server = file_server(b"x").await;
    let dir = tempfile::TempDir::new().unwrap();
    // Default config: 127.0.0.1 is on the denylist.
    let tool = WriteFileTool::new_with_root_and_config(
        dir.path().to_path_buf(),
        ToolsConfig { fetch_timeout_secs: 15, ..ToolsConfig::default() },
    );
    let url = format!("{}/data.bin", server.uri());

    let err = tool.execute(&serde_json::json!({ "path": "x", "url": url })).unwrap_err();
    assert!(err.to_string().contains("denied"), "{err}");
    assert!(!dir.path().join("x").exists());
}

#[tokio::test]
async fn allowed_hosts_whitelist_enforces_exact_host() {
    let server = file_server(b"x").await;
    let dir = tempfile::TempDir::new().unwrap();
    let config = ToolsConfig {
        denied_hosts: Vec::new(),
        allowed_hosts: vec!["example.com".to_string()],
        fetch_timeout_secs: 15,
        ..ToolsConfig::default()
    };
    let tool = WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), config);
    let url = format!("{}/data.bin", server.uri());

    let err = tool.execute(&serde_json::json!({ "path": "x", "url": url })).unwrap_err();
    assert!(err.to_string().contains("allowed_hosts"), "{err}");
}

// --- allowed_dirs ---

#[test]
fn read_outside_allowed_dirs_is_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/secret.txt"), "s").unwrap();
    std::fs::write(dir.path().join("outside.txt"), "o").unwrap();
    // Allow only the sub dir.
    let config = ToolsConfig { allowed_dirs: vec!["sub".to_string()], ..ToolsConfig::default() };
    let tool = ReadFileTool::new_with_root_and_config(dir.path().to_path_buf(), config);

    let ok = tool.execute(&serde_json::json!({ "path": "sub/secret.txt" })).unwrap();
    assert!(ok.success, "{}", ok.output);

    let denied = tool.execute(&serde_json::json!({ "path": "outside.txt" })).unwrap_err();
    assert!(denied.to_string().contains("outside allowed directories"), "{denied}");
}

#[test]
fn write_with_dotdot_escape_is_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    let tool =
        WriteFileTool::new_with_root_and_config(dir.path().to_path_buf(), test_tools_config());

    let err =
        tool.execute(&serde_json::json!({ "path": "../escaped.txt", "content": "x" })).unwrap_err();
    assert!(err.to_string().contains("outside allowed directories"), "{err}");
}

// --- network approval gate (executor level) ---

#[tokio::test]
async fn network_call_forces_approval_when_gate_is_on() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(LlmResponse {
                content: String::new(),
                tool_calls: Some(vec![ToolCallRequest {
                    id: "w1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "write_file".to_string(),
                        arguments: serde_json::json!({
                            "path": "x.txt", "url": "https://example.com/x"
                        })
                        .to_string(),
                    },
                }]),
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::ToolCalls,
            })
        } else {
            Ok(LlmResponse {
                content: "done".to_string(),
                tool_calls: None,
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::Stop,
            })
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, mut events_rx, commands_tx) = lcode::agent::AgentRuntime::new();
    let mut tuning_config = Config::default();
    tuning_config.tools.network_requires_approval = true;
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true, // auto-approve on — but the network gate must still hold
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp: Arc::new(Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: Some(Arc::new(RuntimeTuning::from_config(&tuning_config))),
            internal_provider: None,
            web_search: None,
        },
    );

    // Spawn the run; it must block on approval for the URL write.
    let handle = tokio::spawn(async move {
        executor
            .run(
                "fetch x",
                &lcode::agent::Planner::new(10),
                lcode::agent::ConversationMemory::new("sys".to_string()),
                5,
                false,
            )
            .await
    });

    // The ToolCallRequested event must demand approval despite auto_approve.
    let mut requested = false;
    let mut saw_approval_demand = false;
    for _ in 0..50 {
        if let Ok(lcode::agent::AgentEvent::ToolCallRequested { requires_approval, .. }) =
            events_rx.recv().await
        {
            requested = true;
            saw_approval_demand = requires_approval;
            break;
        }
    }
    assert!(requested, "tool call requested");
    assert!(saw_approval_demand, "URL fetch must require approval under the gate");

    // Approve; the session completes.
    commands_tx
        .send(lcode::agent::AgentCommand::ApproveToolCall { id: "w1".to_string() })
        .await
        .unwrap();
    let memory = handle.await.unwrap().expect("run completes");
    assert!(memory.messages().iter().any(|m| m.tool_call_id.as_deref() == Some("w1")));
}

#[tokio::test]
async fn network_call_passes_with_auto_approve_when_gate_is_off() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(LlmResponse {
                content: String::new(),
                tool_calls: Some(vec![ToolCallRequest {
                    id: "w1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "write_file".to_string(),
                        arguments:
                            serde_json::json!({ "path": "x.txt", "url": "https://example.com/x" })
                                .to_string(),
                    },
                }]),
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::ToolCalls,
            })
        } else {
            Ok(LlmResponse {
                content: "done".to_string(),
                tool_calls: None,
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::Stop,
            })
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let mut tuning_config = Config::default();
    tuning_config.tools.network_requires_approval = false;
    let (runtime, mut events_rx, _commands) = lcode::agent::AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp: Arc::new(Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: Some(Arc::new(RuntimeTuning::from_config(&tuning_config))),
            internal_provider: None,
            web_search: None,
        },
    );

    let memory = executor
        .run(
            "fetch x",
            &lcode::agent::Planner::new(10),
            lcode::agent::ConversationMemory::new("sys".to_string()),
            5,
            false,
        )
        .await
        .expect("run completes without approval");
    assert!(memory.messages().iter().any(|m| m.tool_call_id.as_deref() == Some("w1")));
    // requires_approval must be false on the request event.
    while let Ok(event) = events_rx.try_recv() {
        if let lcode::agent::AgentEvent::ToolCallRequested { requires_approval, .. } = event {
            assert!(!requires_approval, "gate off: no approval demanded");
        }
    }
}

// --- config defaults ---

#[test]
fn tools_config_defaults_block_ssrf_hosts() {
    let config = ToolsConfig::default();
    assert_eq!(config.max_fetch_bytes, 52_428_800);
    assert_eq!(config.fetch_timeout_secs, 60);
    assert!(config.network_requires_approval);
    for host in ["127.0.0.1", "localhost", "169.254.169.254"] {
        assert!(config.denied_hosts.iter().any(|h| h == host), "missing deny: {host}");
    }
}
