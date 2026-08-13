//! Shell execution tool.
//!
//! Allows the agent to execute shell commands in a controlled environment.
//! Includes safety features: command allow/deny lists, timeout, and
//! working directory confinement.

use crate::config::Config;
use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;
use std::process::Command;

/// Tool for executing shell commands.
pub struct ShellTool {
    workspace_root: PathBuf,
    allowed_commands: Vec<String>,
    denied_commands: Vec<String>,
    timeout_secs: u64,
}

impl ShellTool {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            workspace_root: std::env::current_dir()?,
            allowed_commands: config.tools.allowed_commands.clone(),
            denied_commands: config.tools.denied_commands.clone(),
            timeout_secs: 120,
        })
    }

    /// Create a tool rooted at `root` with no allow/deny overrides.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn new_with_root(root: PathBuf) -> Self {
        Self {
            workspace_root: root,
            allowed_commands: vec![],
            denied_commands: vec![],
            timeout_secs: 120,
        }
    }

    /// Create a tool rooted at `root` with custom denied commands.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn with_denied(root: PathBuf, denied: Vec<String>) -> Self {
        Self {
            workspace_root: root,
            allowed_commands: vec![],
            denied_commands: denied,
            timeout_secs: 120,
        }
    }

    /// Check if a command is safe to execute.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn check_safety(&self, command: &str) -> anyhow::Result<()> {
        let lower = command.trim().to_lowercase();

        // Check denied patterns first (always enforced)
        for denied in &self.denied_commands {
            if lower.contains(&denied.to_lowercase()) {
                anyhow::bail!(
                    "Command blocked: matches denied pattern '{}'. Add it to tools.allowed_commands in config to override.",
                    denied
                );
            }
        }

        // Commands on the allowlist bypass the dangerous-pattern checks
        if self.allowed_commands.iter().any(|allowed| lower.starts_with(&allowed.to_lowercase())) {
            return Ok(());
        }

        // Check specifically dangerous patterns
        let dangerous = ["rm -rf /", "mkfs.", "dd if=", "> /dev/sda", "fork bomb", ":(){ :|:& };:"];

        for d in &dangerous {
            if lower.contains(d) {
                anyhow::bail!(
                    "Command blocked: appears to be destructive. If this is intentional, please run it manually."
                );
            }
        }

        Ok(())
    }
}

impl std::fmt::Debug for ShellTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellTool")
            .field("workspace_root", &self.workspace_root)
            .field("allowed_commands", &self.allowed_commands)
            .field("denied_commands", &self.denied_commands)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. \
         Commands run in the workspace root directory. \
         Use for: building, testing, installing dependencies, \
         running scripts, and version control operations."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120, max: 300)"
                }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let command_str = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;

        let timeout_secs = args["timeout"].as_u64().unwrap_or(self.timeout_secs).min(300);

        // Safety check
        self.check_safety(command_str)?;

        // Spawn with piped output drained on reader threads, poll for
        // completion, and kill on the deadline — `wait_with_output`
        // would block forever on a hung command (e.g. `find /` on a
        // slow mount), which this timeout actually enforces.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command_str)
            .current_dir(&self.workspace_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?;
        let stdout_reader = child.stdout.take().map(drain_in_thread);
        let stderr_reader = child.stderr.take().map(drain_in_thread);

        let status = match wait_with_timeout(&mut child, timeout_secs) {
            Ok(status) => status,
            Err(()) => {
                return Ok(ToolResult::err(format!(
                    "Command timed out after {}s (max 300)",
                    timeout_secs
                )));
            }
        };

        let stdout = read_drained(stdout_reader);
        let stderr = read_drained(stderr_reader);
        let exit_code = status.code().unwrap_or(-1);

        let mut result_output = String::new();

        if exit_code == 0 {
            result_output.push_str("✅ Command succeeded (exit code: 0)\n");
        } else {
            result_output.push_str(&format!("❌ Command failed (exit code: {})\n", exit_code));
        }

        if !stdout.is_empty() {
            // Truncate output if too long
            let truncated = truncate_output(&stdout, 10000);
            result_output.push_str(&format!("--- stdout ---\n{}\n", truncated));
        }

        if !stderr.is_empty() {
            let truncated = truncate_output(&stderr, 5000);
            result_output.push_str(&format!("--- stderr ---\n{}\n", truncated));
        }

        Ok(ToolResult::ok(result_output))
    }
}

/// Drain a piped-output reader thread (best effort).
fn read_drained(reader: Option<std::thread::JoinHandle<String>>) -> String {
    reader.map(|h| h.join().unwrap_or_default()).unwrap_or_default()
}

/// A reader thread pulling a child's piped output to EOF (prevents the
/// pipe buffer from deadlocking the child on large output).
fn drain_in_thread<R: std::io::Read + Send + 'static>(
    mut pipe: R,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        buf
    })
}

/// Poll a child until exit or the deadline; kills the child on timeout
/// and returns `Err(())` so the caller can report it.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout_secs: u64,
) -> std::result::Result<std::process::ExitStatus, ()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
}

/// Truncate long output with a note about truncation. Walks back to a
/// char boundary: slicing at `max_len` would panic mid-character.
fn truncate_output(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated, total {} bytes)\n", &s[..end], s.len())
}
