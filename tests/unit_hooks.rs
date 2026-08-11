//! Unit tests for the lifecycle hook subsystem (s20): registry run
//! semantics, tool denial, and the default safety hooks.

use lcode::agent::{
    deny_tool, register_default_hooks, HookContext, HookDecision, HookPoint, HookRegistry,
};
use serde_json::json;

/// A bare context for the given hook point.
fn ctx(point: HookPoint) -> HookContext {
    HookContext { point, tool_name: None, tool_args: None, prompt: None }
}

/// A PreToolUse context for a `shell` call with the given command.
fn shell_ctx(command: &str) -> HookContext {
    HookContext {
        point: HookPoint::PreToolUse,
        tool_name: Some("shell".to_string()),
        tool_args: Some(json!({ "command": command })),
        prompt: None,
    }
}

/// A PreToolUse context for an arbitrary tool call.
fn tool_ctx(name: &str) -> HookContext {
    HookContext {
        point: HookPoint::PreToolUse,
        tool_name: Some(name.to_string()),
        tool_args: Some(json!({ "path": "/tmp/x" })),
        prompt: None,
    }
}

// ---------------------------------------------------------------------------
// HookRegistry::run
// ---------------------------------------------------------------------------

#[test]
fn test_run_with_no_hooks_allows() {
    let registry = HookRegistry::default();
    assert_eq!(registry.run(&ctx(HookPoint::PreToolUse)), HookDecision::Allow);
    assert_eq!(registry.run(&ctx(HookPoint::UserPromptSubmit)), HookDecision::Allow);
    assert!(registry.is_empty());
}

#[test]
fn test_run_first_block_wins() {
    let mut registry = HookRegistry::default();
    registry.add(HookPoint::PreToolUse, Box::new(|_| HookDecision::Allow));
    registry.add(
        HookPoint::PreToolUse,
        Box::new(|_| HookDecision::Block { reason: "first block".to_string() }),
    );
    registry.add(
        HookPoint::PreToolUse,
        Box::new(|_| HookDecision::Block { reason: "second block".to_string() }),
    );

    // The first Block among the matching hooks must win, not the last.
    assert_eq!(
        registry.run(&ctx(HookPoint::PreToolUse)),
        HookDecision::Block { reason: "first block".to_string() }
    );
}

#[test]
fn test_run_points_are_isolated() {
    let mut registry = HookRegistry::default();
    registry.add(
        HookPoint::PreToolUse,
        Box::new(|_| HookDecision::Block { reason: "blocked at pretool".to_string() }),
    );

    // The Block hook lives at PreToolUse, so other points are unaffected.
    assert_eq!(registry.run(&ctx(HookPoint::PostToolUse)), HookDecision::Allow);
    assert_eq!(registry.run(&ctx(HookPoint::Stop)), HookDecision::Allow);
    assert_eq!(registry.run(&ctx(HookPoint::UserPromptSubmit)), HookDecision::Allow);
    assert_eq!(registry.len(), 1);
}

// ---------------------------------------------------------------------------
// deny_tool
// ---------------------------------------------------------------------------

#[test]
fn test_deny_tool_blocks_matching_tool() {
    let hook = deny_tool("shell", "shell usage is denied");

    // Exact name match.
    assert_eq!(
        hook(&tool_ctx("shell")),
        HookDecision::Block { reason: "shell usage is denied".to_string() }
    );
    // Case-insensitive contains match.
    assert_eq!(
        hook(&tool_ctx("Shell")),
        HookDecision::Block { reason: "shell usage is denied".to_string() }
    );
    assert_eq!(
        hook(&tool_ctx("MY_SHELL_WRAPPER")),
        HookDecision::Block { reason: "shell usage is denied".to_string() }
    );
}

#[test]
fn test_deny_tool_allows_non_matching_tool() {
    let hook = deny_tool("shell", "shell usage is denied");

    // Different tool.
    assert_eq!(hook(&tool_ctx("write_file")), HookDecision::Allow);
    // Missing tool name.
    assert_eq!(hook(&ctx(HookPoint::PreToolUse)), HookDecision::Allow);
    // Denial applies at PreToolUse only.
    let post = HookContext {
        point: HookPoint::PostToolUse,
        tool_name: Some("shell".to_string()),
        tool_args: None,
        prompt: None,
    };
    assert_eq!(hook(&post), HookDecision::Allow);
}

// ---------------------------------------------------------------------------
// register_default_hooks
// ---------------------------------------------------------------------------

#[test]
fn test_default_hooks_block_dangerous_shell_commands() {
    let mut registry = HookRegistry::default();
    register_default_hooks(&mut registry);

    for command in [
        "rm -rf /",
        "rm -rf /* --no-preserve-root",
        "mkfs.ext4 /dev/sdb",
        "mkfs /dev/sda",
        "dd if=/dev/zero of=/dev/sda bs=1M count=1",
        "sudo apt-get install evil",
        "shutdown -h now",
        "reboot",
    ] {
        let decision = registry.run(&shell_ctx(command));
        let HookDecision::Block { reason } = decision else {
            panic!("expected Block for command {command:?}, got Allow");
        };
        assert!(!reason.is_empty(), "block reason for {command:?} must be shown to the model");
    }
}

#[test]
fn test_default_hooks_allow_safe_commands() {
    let mut registry = HookRegistry::default();
    register_default_hooks(&mut registry);

    for command in
        ["ls -la", "echo hello world", "git status", "rm -rf ./target", "cat src/main.rs"]
    {
        assert_eq!(
            registry.run(&shell_ctx(command)),
            HookDecision::Allow,
            "safe command {command:?} must be allowed"
        );
    }

    // Non-shell tools are never gated by the shell safety hook.
    assert_eq!(registry.run(&tool_ctx("write_file")), HookDecision::Allow);
    assert_eq!(registry.run(&tool_ctx("todo_write")), HookDecision::Allow);
}

#[test]
fn test_default_hooks_posttooluse_and_prompt_pass_through() {
    let mut registry = HookRegistry::default();
    register_default_hooks(&mut registry);

    // PostToolUse is a no-op observation point.
    let post = HookContext {
        point: HookPoint::PostToolUse,
        tool_name: Some("shell".to_string()),
        tool_args: Some(json!({ "command": "rm -rf /" })),
        prompt: None,
    };
    assert_eq!(registry.run(&post), HookDecision::Allow);

    // UserPromptSubmit passes through, including empty prompts.
    let prompt_ctx = |prompt: Option<String>| HookContext {
        point: HookPoint::UserPromptSubmit,
        tool_name: None,
        tool_args: None,
        prompt,
    };
    assert_eq!(registry.run(&prompt_ctx(Some("do something".to_string()))), HookDecision::Allow);
    assert_eq!(registry.run(&prompt_ctx(Some(String::new()))), HookDecision::Allow);
    assert_eq!(registry.run(&prompt_ctx(None)), HookDecision::Allow);
}
