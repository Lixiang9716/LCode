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
    pub fn run(&self, context: &HookContext) -> HookDecision {
        // TODO(s20): iterate matching hooks; return first Block or Allow.
        let _ = context;
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
pub fn deny_tool(tool_name: &str, reason: &str) -> HookFn {
    // TODO(s20): return a closure that Blocks when tool_name matches.
    let _ = (tool_name, reason);
    Box::new(|_| HookDecision::Allow)
}

/// Register default hooks (e.g. deny dangerous tools) with the session.
pub fn register_default_hooks(registry: &mut HookRegistry) {
    // TODO(s20): e.g. PreToolUse deny for "rm -rf /" style destructive
    // commands even when auto-approve is on.
    let _ = registry;
}
