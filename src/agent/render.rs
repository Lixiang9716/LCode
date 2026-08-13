//! Default stdout renderer for agent events.
//!
//! The renderer consumes the event stream and turns it into terminal
//! output, handling approval prompts through stdin and sending decisions
//! back on the command channel. This is the default observer used by
//! single-shot tasks; the REPL can plug in its own renderer.

use crate::agent::event::{AgentCommand, AgentEvent};
use tokio::sync::{broadcast, mpsc};

/// Render a single agent event to stdout.
pub fn render_event(event: &AgentEvent) {
    match event {
        AgentEvent::SessionStarted { task } => {
            println!("\n🤖 LCode Agent starting...\n");
            println!("Task: {}\n", task);
        }
        AgentEvent::TurnStarted { .. } => {}
        AgentEvent::TextGenerated { content } => println!("\n{}", content),
        // Streaming deltas print inline (typewriter); the accumulated
        // text is suppressed by the executor so nothing is printed twice.
        AgentEvent::TextDelta { content } => {
            use std::io::Write;
            print!("{content}");
            let _ = std::io::stdout().flush();
        }
        AgentEvent::ToolCallRequested { name, arguments, requires_approval, .. } => {
            let args = serde_json::to_string_pretty(arguments).unwrap_or_default();
            println!("\n🔧 Tool call: {}({})", name, args);
            if *requires_approval {
                print!("   Execute? [y/N] ");
            }
        }
        AgentEvent::ToolCallExecuted { output, .. } => {
            println!("   ✅ Result: {}", truncate(output, 500));
        }
        AgentEvent::ToolCallFailed { error, .. } => println!("   ❌ {}", error),
        AgentEvent::ToolCallDeclined { .. } => println!("   ⏭️  Skipped (user declined)."),
        AgentEvent::TurnFinished { .. } => {}
        AgentEvent::TaskFinished { turns, .. } => {
            println!("\n✅ Task completed in {} turns.", turns);
        }
        AgentEvent::TaskAborted { reason } => println!("\n⚠️  {}", reason),
        AgentEvent::Error { message } => println!("\n❌ {}", message),

        // --- Session capabilities (learn-claude-code parity) ---
        event => render_capability_event(event),
    }
}

/// Render the todo list snapshot.
fn render_todos(items: &[crate::agent::TodoItem]) {
    println!("\n📋 Todos ({}):", items.len());
    for item in items {
        let mark = match item.status {
            crate::agent::TodoStatus::Pending => "[ ]",
            crate::agent::TodoStatus::InProgress => "[>]",
            crate::agent::TodoStatus::Completed => "[x]",
        };
        println!("   {} #{}: {}", mark, item.id, item.text);
    }
}

/// Render the session-capability events (todo/skill/compact/subagent/
/// background/task/team/worktree) added for learn-claude-code parity.
fn render_capability_event(event: &AgentEvent) {
    match event {
        AgentEvent::TodoUpdated { items } => render_todos(items),
        AgentEvent::TodoNag { turns_since_update } => {
            println!(
                "\n⏰ Reminder: update your todos ({} turns without update).",
                turns_since_update
            );
        }
        AgentEvent::SkillLoaded { name } => println!("\n📖 Skill loaded: {}", name),
        AgentEvent::ContextCompacted { summary, transcript_path } => {
            println!(
                "\n🗜️  Context compacted: {} (transcript: {})",
                truncate(summary, 200),
                transcript_path
            );
        }
        AgentEvent::SubagentSpawned { prompt, .. } => {
            println!("\n🧵 Subagent spawned: {}", truncate(prompt, 100));
        }
        AgentEvent::SubagentCompleted { summary, .. } => {
            println!("\n🧵 Subagent finished: {}", truncate(summary, 200));
        }
        AgentEvent::BackgroundTaskStarted { id, command } => {
            println!("\n🔄 Background started [{}]: {}", id, truncate(command, 80));
        }
        AgentEvent::BackgroundTaskCompleted { id, status, output } => {
            println!("\n🔄 Background [{}] {}: {}", id, status, truncate(output, 200));
        }
        AgentEvent::TaskCreated { id, title } => {
            println!("\n📌 Task #{} created: {}", id, title);
        }
        AgentEvent::TaskUpdated { id, status } => {
            println!("\n📌 Task #{} → {}", id, status);
        }
        AgentEvent::TeamMessageSent { from, to, msg_type } => {
            println!("\n💬 [{}] {} → {} (team)", from, to, msg_type);
        }
        AgentEvent::TeammateStateChanged { name, state } => {
            println!("\n👥 Teammate {} → {}", name, state);
        }
        AgentEvent::WorktreeCreated { name, task_id } => {
            println!("\n🌿 Worktree created: {} (task #{})", name, task_id);
        }
        AgentEvent::WorktreeRemoved { name } => {
            println!("\n🌿 Worktree removed: {}", name);
        }
        _ => {}
    }
}

/// Spawn a renderer task that consumes events, prints them, and answers
/// approval prompts by reading stdin and sending decisions back on the
/// command channel.
pub fn spawn_renderer(
    mut events: broadcast::Receiver<AgentEvent>,
    commands: mpsc::Sender<AgentCommand>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Streaming deltas print inline; the first non-delta event after
        // a run of deltas closes the line (the typewriter's newline).
        let mut in_delta = false;
        loop {
            let keep_going = match events.recv().await {
                Ok(event) => {
                    let is_delta = matches!(event, AgentEvent::TextDelta { .. });
                    in_delta = newline_after_deltas(in_delta, is_delta);
                    render_event(&event);
                    // Approval prompts read stdin and answer on the
                    // command channel; a broken channel ends the loop.
                    answer_approval(&event, &commands).await
                }
                // A lagged consumer only misses the skipped events; keep
                // rendering instead of ending the stream.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    tracing::warn!("renderer lagged; some events were skipped");
                    true
                }
                Err(broadcast::error::RecvError::Closed) => false,
            };
            if !keep_going {
                break;
            }
        }
    })
}

/// Close the typewriter line when a run of streamed deltas ends.
fn newline_after_deltas(in_delta: bool, is_delta: bool) -> bool {
    if in_delta && !is_delta {
        println!();
    }
    is_delta
}

/// Answer an approval prompt (when the event requests one) and return
/// false when the command channel is gone.
async fn answer_approval(event: &AgentEvent, commands: &mpsc::Sender<AgentCommand>) -> bool {
    let AgentEvent::ToolCallRequested { id, requires_approval: true, .. } = event else {
        return true;
    };
    let approved = read_approval_line().await;
    let command = if approved {
        AgentCommand::ApproveToolCall { id: id.clone() }
    } else {
        AgentCommand::RejectToolCall { id: id.clone() }
    };
    commands.send(command).await.is_ok()
}

/// Read a single approval line from stdin (y/yes → approve).
async fn read_approval_line() -> bool {
    tokio::task::spawn_blocking(|| {
        use std::io::Write;
        let mut input = String::new();
        std::io::stdout().flush().ok();
        std::io::stdin().read_line(&mut input).ok();
        let input = input.trim().to_lowercase();
        input == "y" || input == "yes"
    })
    .await
    .unwrap_or(false)
}

/// Truncate a string to max_len characters, adding "..." if truncated.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let boundary = s[..max_len].char_indices().last().map(|(i, _)| i).unwrap_or(max_len);
    &s[..boundary]
}
