//! Guardrail tests: grep/glob sensitive-path filtering + output
//! scrubbing, and the shell PreToolUse hook (sensitive paths, denied
//! hosts).

use lcode::agent::guardrails;
use lcode::agent::{HookContext, HookDecision, HookPoint, HookRegistry};
use lcode::config::ToolsConfig;
use lcode::tools::search::{GlobTool, GrepTool};
use lcode::tools::Tool;

fn tool_config() -> ToolsConfig {
    ToolsConfig::default()
}

// --- grep ---

#[test]
fn grep_hides_sensitive_path_matches() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("server.pem"), "TOKEN=secret-value\n").unwrap();
    std::fs::write(dir.path().join("code.txt"), "TOKEN=also-in-code\n").unwrap();
    let tool = GrepTool::new_with_root_and_config(dir.path().to_path_buf(), tool_config());

    let result = tool.execute(&serde_json::json!({ "pattern": "TOKEN" })).unwrap();
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("code.txt"), "normal file stays: {}", result.output);
    assert!(!result.output.contains("server.pem"), "sensitive path hidden: {}", result.output);
    assert!(result.output.contains("1 sensitive matches hidden"), "{}", result.output);
}

#[test]
fn grep_output_is_scrubbed() {
    let dir = tempfile::TempDir::new().unwrap();
    let secret = "api_key = sk-abcdefghijklmnop123456";
    std::fs::write(dir.path().join("config.ini"), format!("{secret}\n")).unwrap();
    let tool = GrepTool::new_with_root_and_config(dir.path().to_path_buf(), tool_config());

    let result = tool.execute(&serde_json::json!({ "pattern": "api_key" })).unwrap();
    assert!(!result.output.contains("sk-abcdefghijklmnop123456"), "redacted: {}", result.output);
    assert!(result.output.contains("[REDACTED]"), "{}", result.output);
}

#[test]
fn grep_without_sensitive_matches_keeps_message() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("code.txt"), "hello\n").unwrap();
    let tool = GrepTool::new_with_root_and_config(dir.path().to_path_buf(), tool_config());

    let result = tool.execute(&serde_json::json!({ "pattern": "nomatch" })).unwrap();
    assert!(result.output.contains("No matches found"), "{}", result.output);
}

// --- glob ---

#[test]
fn glob_hides_sensitive_names() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".env"), "x").unwrap();
    std::fs::write(dir.path().join("notes.md"), "x").unwrap();
    let tool = GlobTool::new_with_root_and_config(dir.path().to_path_buf(), tool_config());

    let result = tool.execute(&serde_json::json!({ "pattern": "**/*" })).unwrap();
    assert!(result.output.contains("notes.md"), "{}", result.output);
    assert!(!result.output.contains(".env"), "sensitive name hidden: {}", result.output);
    assert!(result.output.contains("1 sensitive matches hidden"), "{}", result.output);
}

// --- shell guardrail hook ---

fn registry_with(tools: ToolsConfig) -> HookRegistry {
    let mut registry = HookRegistry::default();
    guardrails::register(&mut registry, tools);
    registry
}

fn shell_ctx(command: &str) -> HookContext {
    HookContext {
        point: HookPoint::PreToolUse,
        tool_name: Some("shell".to_string()),
        tool_args: Some(serde_json::json!({ "command": command })),
        prompt: None,
    }
}

#[test]
fn hook_blocks_sensitive_path_commands() {
    let registry = registry_with(tool_config());
    let decision = registry.run(&shell_ctx("cat .env"));
    match decision {
        HookDecision::Block { reason } => assert!(reason.contains("sensitive path"), "{reason}"),
        HookDecision::Allow => panic!("cat .env must be blocked"),
    }
}

#[test]
fn hook_blocks_denied_hosts() {
    let registry = registry_with(tool_config());
    let decision = registry.run(&shell_ctx("curl http://169.254.169.254/latest/meta-data"));
    match decision {
        HookDecision::Block { reason } => assert!(reason.contains("denied"), "{reason}"),
        HookDecision::Allow => panic!("metadata host must be blocked"),
    }
}

#[test]
fn hook_blocks_bare_denied_host_tokens() {
    let registry = registry_with(tool_config());
    let decision = registry.run(&shell_ctx("nc 127.0.0.1 80"));
    match decision {
        HookDecision::Block { reason } => assert!(reason.contains("127.0.0.1"), "{reason}"),
        HookDecision::Allow => panic!("loopback must be blocked"),
    }
}

#[test]
fn hook_allows_normal_commands() {
    let registry = registry_with(tool_config());
    for command in [
        "ls -la",
        "sha256sum assets/logo.png",
        "rustc --version",
        "curl -sI https://doc.rust-lang.org",
        "git status",
    ] {
        let decision = registry.run(&shell_ctx(command));
        assert!(matches!(decision, HookDecision::Allow), "{command} must pass");
    }
}

#[test]
fn hook_only_gates_the_shell_tool() {
    let registry = registry_with(tool_config());
    let ctx = HookContext {
        point: HookPoint::PreToolUse,
        tool_name: Some("write_file".to_string()),
        tool_args: Some(serde_json::json!({ "path": ".env", "content": "x" })),
        prompt: None,
    };
    assert!(matches!(registry.run(&ctx), HookDecision::Allow), "write_file is gated elsewhere");
}
