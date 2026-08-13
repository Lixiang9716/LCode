//! Guardrail hooks for the shell tool.
//!
//! read_file/write_file carry the sensitive-path and host policies;
//! this module closes the gap for `shell`, which otherwise bypasses
//! every guardrail (`cat .env`, `curl http://169.254.169.254/...`).
//! A PreToolUse hook scans the command line before execution: tokens
//! referencing sensitive paths or denied hosts block the command.

use crate::agent::hooks::{HookContext, HookDecision, HookPoint, HookRegistry};
use crate::config::ToolsConfig;
use crate::tools::fetch::host_of;
use crate::tools::scrub;

/// Register the shell guardrail hook (PreToolUse). Runs alongside the
/// default hooks; the first `Block` decision wins, so this never
/// overrides an earlier block.
pub fn register(registry: &mut HookRegistry, tools: ToolsConfig) {
    registry.add(
        HookPoint::PreToolUse,
        Box::new(move |ctx: &HookContext| guardrail_decision(ctx, &tools)),
    );
}

/// The per-command policy: sensitive-path tokens and denied-host
/// tokens are refused with an explanatory reason.
fn guardrail_decision(ctx: &HookContext, tools: &ToolsConfig) -> HookDecision {
    if ctx.tool_name.as_deref() != Some("shell") {
        return HookDecision::Allow;
    }
    let command = ctx
        .tool_args
        .as_ref()
        .and_then(|args| args.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    for token in command.split_whitespace() {
        if scrub::is_sensitive_path(token, &tools.sensitive_paths) {
            return HookDecision::Block {
                reason: format!(
                    "Shell command blocked by guardrail: references sensitive path '{token}'"
                ),
            };
        }
        if let Some(reason) = denied_host_reason(token, tools) {
            return HookDecision::Block { reason };
        }
    }
    HookDecision::Allow
}

/// Host check for one command token: URL tokens are parsed down to
/// their host; bare tokens are compared directly against the policy
/// (exact or `*.suffix` wildcard, same matcher as fetch).
fn denied_host_reason(token: &str, tools: &ToolsConfig) -> Option<String> {
    let host = host_of(token).unwrap_or(token.to_string());
    let host_name = host.split(':').next().unwrap_or(&host);
    if tools.denied_hosts.iter().any(|e| crate::tools::fetch::host_matches(host_name, e)) {
        return Some(format!(
            "Shell command blocked by guardrail: host '{host_name}' is denied by tools.denied_hosts"
        ));
    }
    if !tools.allowed_hosts.is_empty()
        && !tools.allowed_hosts.iter().any(|e| crate::tools::fetch::host_matches(host_name, e))
    {
        return Some(format!(
            "Shell command blocked by guardrail: host '{host_name}' is not in tools.allowed_hosts"
        ));
    }
    None
}
