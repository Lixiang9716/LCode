//! Workspace-context injection (P1): the model sees repository state
//! without spending turns on `git status` itself.
//!
//! Kept in a separate file so `executor_hooks.rs` stays under the
//! 500-line style limit.

use crate::agent::executor::Executor;
use crate::agent::ConversationMemory;

/// Workspace-context injection (P1): when `agent.workspace_aware` is on
/// and the workspace is a git repository, the current branch, porcelain
/// status and diff stat go into the conversation as one block — the
/// model sees repository state without spending turns on `git status`.
/// Cheap (three git subprocesses) and fail-silent (not a git repo or
/// git missing = no block).
pub(crate) fn inject_workspace_context(executor: &Executor, memory: &mut ConversationMemory) {
    let enabled = executor.tuning.as_ref().map(|t| t.workspace_aware).unwrap_or(false);
    if !enabled {
        return;
    }
    let Some(block) = workspace_context() else {
        return;
    };
    // User messages are not persisted by the recorder, so the audit
    // trail gets an explicit event for the injected context.
    executor.runtime.publish(crate::agent::AgentEvent::WorkspaceContext {
        branch: branch_of(&block).unwrap_or_default(),
    });
    memory.add_user(block);
}

/// Extract the branch name from a rendered context block.
fn branch_of(block: &str) -> Option<String> {
    let line = block.lines().find(|l| l.starts_with("git branch: "))?;
    Some(line.trim_start_matches("git branch: ").to_string())
}

/// Build the workspace-context block, or `None` when unavailable.
fn workspace_context() -> Option<String> {
    let branch = git_output(&["branch", "--show-current"])?.trim().to_string();
    if branch.is_empty() {
        return None; // not in a git work tree (or detached HEAD)
    }
    let status = git_output(&["status", "--porcelain"])?;
    let diff_stat = git_output(&["diff", "--stat", "HEAD"])?;
    let status = truncate_chars(status.trim(), 2000);
    let diff_stat = truncate_chars(diff_stat.trim(), 2000);
    Some(format!(
        "<workspace-context>\ngit branch: {branch}\nstatus (porcelain):\n{status}\n\nchanges vs HEAD (diff --stat):\n{diff_stat}\n</workspace-context>"
    ))
}

/// One git invocation; `None` when git is missing or the call fails.
fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Character-boundary-safe truncation.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}
