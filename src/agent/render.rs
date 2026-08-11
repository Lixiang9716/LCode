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
        while let Ok(event) = events.recv().await {
            render_event(&event);

            if let AgentEvent::ToolCallRequested { id, requires_approval: true, .. } = &event {
                let approved = read_approval_line().await;
                let command = if approved {
                    AgentCommand::ApproveToolCall { id: id.clone() }
                } else {
                    AgentCommand::RejectToolCall { id: id.clone() }
                };
                if commands.send(command).await.is_err() {
                    break;
                }
            }
        }
    })
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
