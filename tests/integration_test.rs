//! End-to-end integration tests for LCode.
//!
//! These tests exercise the crate's public API end to end:
//!
//! 1. **LLM API integration** — `OpenAiProvider::chat` against a wiremock
//!    HTTP server (no real network), covering both plain text completions
//!    and tool-call responses.
//! 2. **Configuration loading** — user-global `config.toml` and project-local
//!    `.lcode.toml` are loaded and merged over defaults. Environment state is
//!    isolated with `serial_test` and restored afterwards.
//! 3. **Tool end-to-end workflows** — `ToolRegistry` built from the default
//!    config, driving write → read → edit → grep on a temp directory, plus a
//!    real `shell` tool execution.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use lcode::config::{load, Config, LlmConfig};
use lcode::llm::openai::OpenAiProvider;
use lcode::llm::{ChatMessage, FinishReason, LlmProvider, ToolCallRequest};
use lcode::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Restores a single environment variable to its previous value on drop.
struct EnvRestore {
    key: &'static str,
    previous: Option<OsString>,
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Point `XDG_CONFIG_HOME` at `dir` for the duration of the test.
///
/// `dirs::config_dir()` on Linux honors `$XDG_CONFIG_HOME` (falling back to
/// `$HOME/.config`), so this makes the user-global config directory
/// deterministic and isolated.
fn with_xdg_config_home(dir: &Path) -> EnvRestore {
    let key = "XDG_CONFIG_HOME";
    let previous = std::env::var_os(key);
    std::env::set_var(key, dir);
    EnvRestore { key, previous }
}

/// Remove `LCODE_*` environment overrides for the duration of the test.
fn without_lcode_env_overrides() -> Vec<EnvRestore> {
    const KEYS: [&str; 5] = [
        "LCODE_LLM_PROVIDER",
        "LCODE_LLM_API_KEY",
        "LCODE_LLM_MODEL",
        "LCODE_LLM_API_BASE",
        "LCODE_LLM_MAX_TOKENS",
    ];
    KEYS.iter()
        .map(|&key| {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            EnvRestore { key, previous }
        })
        .collect()
}

/// Changes the process working directory and restores it on drop, even if the
/// test panics.
struct CwdGuard(PathBuf);

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        let original = std::env::current_dir().expect("failed to get current dir");
        std::env::set_current_dir(dir).expect("failed to set current dir");
        Self(original)
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// Captures the `Authorization` header of every request it sees.
struct AuthHeaderCapture(Arc<Mutex<Option<String>>>);

impl Match for AuthHeaderCapture {
    fn matches(&self, request: &Request) -> bool {
        if let Some(value) = request.headers.get("authorization") {
            *self.0.lock().unwrap() = Some(String::from_utf8_lossy(value.as_bytes()).to_string());
        }
        true
    }
}

/// Build a wiremock-backed `OpenAiProvider` pointing at `server`.
fn provider_for(server: &MockServer) -> OpenAiProvider {
    let config = LlmConfig {
        provider: "openai".to_string(),
        api_key: "test-key".to_string(),
        model: "gpt-4o-mini".to_string(),
        api_base: Some(format!("{}/v1", server.uri())),
        max_tokens: 256,
        temperature: 0.0,
        fallback_model: None,
    };
    OpenAiProvider::new(&config).expect("provider should validate with a non-empty api_key")
}

/// Mock OpenAI response containing two `tool_calls`.
fn tool_calls_response() -> serde_json::Value {
    serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"src/main.rs\", \"limit\": 10}"
                        }
                    },
                    {
                        "id": "call_def456",
                        "type": "function",
                        "function": {
                            "name": "grep",
                            "arguments": "{\"pattern\": \"pub fn\"}"
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 20,
            "completion_tokens": 12,
            "total_tokens": 32
        }
    })
}

// ---------------------------------------------------------------------------
// 1. LLM API integration tests (wiremock, no real network)
// ---------------------------------------------------------------------------

/// A plain chat completion response is parsed into content, usage and
/// finish_reason.
#[tokio::test]
async fn openai_chat_completion_parses_content_usage_and_finish_reason() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello from mock"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })))
        .mount(&server)
        .await;

    let provider = provider_for(&server);
    let response = provider
        .chat(&[ChatMessage::user("hi")], &[])
        .await
        .expect("chat should succeed against the mock server");

    assert_eq!(response.content, "Hello from mock");
    assert_eq!(response.usage.prompt_tokens, 10);
    assert_eq!(response.usage.completion_tokens, 5);
    assert_eq!(response.usage.total_tokens, 15);
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(response.tool_calls.is_none(), "no tool calls expected");
}

/// A response containing `tool_calls` is parsed into `ToolCallRequest`s with
/// the function name and arguments intact.
#[tokio::test]
async fn openai_chat_completion_parses_tool_calls() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_calls_response()))
        .mount(&server)
        .await;

    let provider = provider_for(&server);
    let response = provider
        .chat(&[ChatMessage::user("inspect the codebase")], &[])
        .await
        .expect("chat should succeed against the mock server");

    assert_eq!(response.finish_reason, FinishReason::ToolCalls);

    let tool_calls: &[ToolCallRequest] =
        response.tool_calls.as_deref().expect("expected tool calls in the response");
    assert_eq!(tool_calls.len(), 2);

    let first = &tool_calls[0];
    assert_eq!(first.id, "call_abc123");
    assert_eq!(first.call_type, "function");
    assert_eq!(first.function.name, "read_file");
    assert_eq!(first.function.arguments, "{\"path\": \"src/main.rs\", \"limit\": 10}");

    let second = &tool_calls[1];
    assert_eq!(second.function.name, "grep");
    assert_eq!(second.function.arguments, "{\"pattern\": \"pub fn\"}");
}

/// The provider sends an `Authorization: Bearer` header with the configured
/// API key.
#[tokio::test]
async fn openai_provider_sends_bearer_auth_header() {
    let server = MockServer::start().await;

    let auth_header = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let capture = AuthHeaderCapture(auth_header.clone());

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(capture)
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let provider = provider_for(&server);
    let response = provider.chat(&[ChatMessage::user("hi")], &[]).await.unwrap();
    assert_eq!(response.content, "ok");

    let captured = auth_header.lock().unwrap().clone();
    assert_eq!(captured.as_deref(), Some("Bearer test-key"));
}

// ---------------------------------------------------------------------------
// 2. Configuration loading integration tests
// ---------------------------------------------------------------------------

/// A user-global `$XDG_CONFIG_HOME/lcode/config.toml` overrides defaults.
#[test]
#[serial]
fn global_config_file_overrides_defaults() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let _xdg = with_xdg_config_home(temp_dir.path());
    let _env = without_lcode_env_overrides();

    let lcode_dir = temp_dir.path().join("lcode");
    std::fs::create_dir_all(&lcode_dir).expect("create lcode config dir");
    std::fs::write(
        lcode_dir.join("config.toml"),
        r#"
[llm]
provider = "openai"
api_key = "sk-test-123"
model = "gpt-4o"
max_tokens = 4096
temperature = 0.7

[agent]
max_turns = 50
require_approval = false

[tools]
enable_web = false
"#,
    )
    .expect("write config.toml");

    let cfg = load().expect("config should load");

    // Defaults are "anthropic" / "claude-sonnet-4-20250514" / 8192 / 0.3.
    assert_eq!(cfg.llm.provider, "openai");
    assert_eq!(cfg.llm.api_key, "sk-test-123");
    assert_eq!(cfg.llm.model, "gpt-4o");
    assert_eq!(cfg.llm.max_tokens, 4096);
    assert_eq!(cfg.llm.temperature, 0.7);

    assert_eq!(cfg.agent.max_turns, 50);
    assert!(!cfg.agent.require_approval);

    assert!(!cfg.tools.enable_web);
}

/// A project-local `.lcode.toml` overrides defaults (and merges on top of the
/// user-global config).
#[test]
#[serial]
fn project_local_config_file_is_loaded() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let _xdg = with_xdg_config_home(temp_dir.path());
    let _env = without_lcode_env_overrides();
    let _cwd = CwdGuard::enter(temp_dir.path());

    std::fs::write(
        temp_dir.path().join(".lcode.toml"),
        r#"
[llm]
provider = "openai_compatible"
model = "local-model"
api_base = "http://localhost:1234/v1"
"#,
    )
    .expect("write .lcode.toml");

    let cfg = load().expect("config should load");

    assert_eq!(cfg.llm.provider, "openai_compatible");
    assert_eq!(cfg.llm.model, "local-model");
    assert_eq!(cfg.llm.api_base.as_deref(), Some("http://localhost:1234/v1"));
}

/// The project-local `.lcode.toml` wins over the user-global `config.toml`.
#[test]
#[serial]
fn project_local_config_wins_over_global_config() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let _xdg = with_xdg_config_home(temp_dir.path());
    let _env = without_lcode_env_overrides();
    let _cwd = CwdGuard::enter(temp_dir.path());

    // User-global config: model A.
    let lcode_dir = temp_dir.path().join("lcode");
    std::fs::create_dir_all(&lcode_dir).expect("create lcode config dir");
    std::fs::write(
        lcode_dir.join("config.toml"),
        "[llm]\nprovider = \"openai\"\nmodel = \"gpt-4o\"\n",
    )
    .expect("write config.toml");

    // Project-local config: model B.
    std::fs::write(
        temp_dir.path().join(".lcode.toml"),
        "[llm]\nprovider = \"anthropic\"\nmodel = \"claude-3-5-sonnet\"\n",
    )
    .expect("write .lcode.toml");

    let cfg = load().expect("config should load");

    assert_eq!(cfg.llm.provider, "anthropic");
    assert_eq!(cfg.llm.model, "claude-3-5-sonnet");
}

// ---------------------------------------------------------------------------
// 3. Tool end-to-end integration tests
// ---------------------------------------------------------------------------

/// All built-in tools are registered and discoverable by name.
#[test]
#[serial]
fn tool_registry_registers_all_builtin_tools() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let _cwd = CwdGuard::enter(temp_dir.path());

    let registry = ToolRegistry::new(&Config::default()).expect("registry");
    let names = registry.list_tools();

    for expected in ["read_file", "write_file", "edit_file", "list_dir", "grep", "glob", "shell"] {
        assert!(
            names.contains(&expected),
            "expected tool '{expected}' to be registered, got: {names:?}"
        );
    }
}

/// End-to-end workflow: write_file → read_file → edit_file → grep, all
/// operating on a temp directory through the `ToolRegistry`.
#[test]
#[serial]
fn file_tools_end_to_end_write_read_edit_grep() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let _cwd = CwdGuard::enter(temp_dir.path());

    let registry = ToolRegistry::new(&Config::default()).expect("registry");

    // write_file
    let result = registry
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "notes.txt",
                "content": "hello world\nsecond line\n"
            }),
        )
        .expect("write_file should run");
    assert!(result.success, "write_file failed: {}", result.output);
    assert!(result.output.contains("notes.txt"));

    // read_file reads the content back
    let result = registry
        .execute("read_file", &serde_json::json!({"path": "notes.txt"}))
        .expect("read_file should run");
    assert!(result.success, "read_file failed: {}", result.output);
    assert!(result.output.contains("hello world"));
    assert!(result.output.contains("second line"));

    // edit_file replaces the unique match
    let result = registry
        .execute(
            "edit_file",
            &serde_json::json!({
                "path": "notes.txt",
                "old_string": "hello world",
                "new_string": "hello rust"
            }),
        )
        .expect("edit_file should run");
    assert!(result.success, "edit_file failed: {}", result.output);

    // read_file confirms the edit
    let result = registry
        .execute("read_file", &serde_json::json!({"path": "notes.txt"}))
        .expect("read_file should run");
    assert!(result.success);
    assert!(result.output.contains("hello rust"));
    assert!(!result.output.contains("hello world"));

    // grep finds the edited content in the workspace
    let result =
        registry.execute("grep", &serde_json::json!({"pattern": "rust"})).expect("grep should run");
    assert!(result.success, "grep failed: {}", result.output);
    assert!(
        result.output.contains("notes.txt"),
        "grep output should mention notes.txt, got: {}",
        result.output
    );

    // grep with no matches reports a friendly "no matches" result
    let result = registry
        .execute("grep", &serde_json::json!({"pattern": "zzz-no-such-token"}))
        .expect("grep should run");
    assert!(result.success);
    assert!(result.output.contains("No matches"));
}

/// The shell tool executes `echo hello` and surfaces its output.
#[test]
#[serial]
fn shell_tool_executes_echo_successfully() {
    let temp_dir = tempfile::TempDir::new().expect("tempdir");
    let _cwd = CwdGuard::enter(temp_dir.path());

    let registry = ToolRegistry::new(&Config::default()).expect("registry");

    let result = registry
        .execute("shell", &serde_json::json!({"command": "echo hello"}))
        .expect("shell should run");
    assert!(result.success, "shell failed: {}", result.output);
    assert!(result.output.contains("hello"));
}
