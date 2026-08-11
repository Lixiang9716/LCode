//! Lifecycle hooks (learn-claude-code s20).
//!
//! The harness exposes four hook points so policies can observe and gate
//! the agent without touching the loop: UserPromptSubmit, PreToolUse,
//! PostToolUse, and Stop. Permission checks are implemented as
//! PreToolUse hooks.

/// The four hook points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookPoint {
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

/// Decision a PreToolUse hook returns.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    /// Allow the operation.
    Allow,
    /// Block the operation with a reason (shown to the model).
    Block { reason: String },
}

/// Context passed to a hook.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub point: HookPoint,
    /// Tool name (PreToolUse / PostToolUse).
    pub tool_name: Option<String>,
    /// Tool arguments (PreToolUse).
    pub tool_args: Option<serde_json::Value>,
    /// User prompt (UserPromptSubmit).
    pub prompt: Option<String>,
}

/// A hook callback.
pub type HookFn = Box<dyn Fn(&HookContext) -> HookDecision + Send + Sync>;

/// Registry of hooks per hook point.
#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<(HookPoint, HookFn)>,
}

impl HookRegistry {
    /// Register a hook for a hook point.
    pub fn add(&mut self, point: HookPoint, hook: HookFn) {
        self.hooks.push((point, hook));
    }

    /// Run all hooks for a point; the first `Block` wins.
    ///
    /// Hooks registered for other points are skipped, and every matching
    /// hook sees the context until one of them blocks; if none does, the
    /// operation is allowed.
    pub fn run(&self, context: &HookContext) -> HookDecision {
        for (point, hook) in &self.hooks {
            if *point != context.point {
                continue;
            }
            match hook(context) {
                decision @ HookDecision::Block { .. } => return decision,
                HookDecision::Allow => {}
            }
        }
        HookDecision::Allow
    }

    /// Number of registered hooks.
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

/// Convenience: a permission hook that blocks a tool by name.
///
/// The returned closure blocks (during `PreToolUse`) any tool whose name
/// contains `tool_name`, compared case-insensitively — e.g.
/// `deny_tool("mcp__deploy", ...)` also blocks `mcp__deploy_prod`.
pub fn deny_tool(tool_name: &str, reason: &str) -> HookFn {
    let needle = tool_name.to_lowercase();
    let reason = reason.to_string();
    Box::new(move |ctx: &HookContext| {
        let matches = ctx.point == HookPoint::PreToolUse
            && ctx.tool_name.as_deref().is_some_and(|name| name.to_lowercase().contains(&needle));
        if matches {
            HookDecision::Block { reason: reason.clone() }
        } else {
            HookDecision::Allow
        }
    })
}

/// Shell command fragments that are always denied, even when auto-approve
/// is enabled (mirrors the s20 `DENY_LIST`). Matching is case-insensitive
/// substring, so e.g. `mkfs` also covers `mkfs.ext4 /dev/sdb`.
const DENIED_SHELL_PATTERNS: &[&str] = &[
    "rm -rf /", // root-wipe (also matches `rm -rf /*`)
    "mkfs",     // filesystem creation
    "dd if=",   // raw device writes
    "sudo",     // privilege escalation
    "shutdown", // system shutdown / reboot family
    "reboot",
];

/// Denial reason for a shell command containing a denied pattern.
fn denied_shell_reason(command: &str) -> Option<String> {
    let lower = command.to_lowercase();
    for pattern in DENIED_SHELL_PATTERNS.iter().copied() {
        if lower.contains(pattern) {
            return Some(format!(
                "Shell command blocked by safety hook: contains denied pattern '{pattern}'"
            ));
        }
    }
    None
}

/// Register default hooks (e.g. deny dangerous tools) with the session.
///
/// - `PreToolUse`: block `shell` commands containing destructive
///   fragments even under auto-approve (the s20 permission layer).
/// - `PostToolUse`: no-op placeholder — the observation point where
///   policies can react to tool output without blocking execution.
/// - `UserPromptSubmit`: pass-through for now (an empty-prompt
///   rejection would live here).
pub fn register_default_hooks(registry: &mut HookRegistry) {
    // PreToolUse: deny destructive shell commands even when auto-approve
    // is on. This runs before dispatch, without touching each individual
    // tool handler.
    registry.add(
        HookPoint::PreToolUse,
        Box::new(|ctx: &HookContext| {
            if ctx.tool_name.as_deref() != Some("shell") {
                return HookDecision::Allow;
            }
            let command = ctx
                .tool_args
                .as_ref()
                .and_then(|args| args.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match denied_shell_reason(command) {
                Some(reason) => HookDecision::Block { reason },
                None => HookDecision::Allow,
            }
        }),
    );

    // PostToolUse: no-op placeholder (observation point for e.g.
    // large-output warnings, audit logging).
    registry.add(HookPoint::PostToolUse, Box::new(|_| HookDecision::Allow));

    // UserPromptSubmit: pass-through.
    registry.add(HookPoint::UserPromptSubmit, Box::new(|_| HookDecision::Allow));
}
