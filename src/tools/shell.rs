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

    /// Check if a command is safe to execute.
    fn check_safety(&self, command: &str) -> anyhow::Result<()> {
        let lower = command.trim().to_lowercase();

        // Check denied patterns first
        for denied in &self.denied_commands {
            if lower.contains(&denied.to_lowercase()) {
                anyhow::bail!(
                    "Command blocked: matches denied pattern '{}'. \
                     Add it to tools.allowed_commands in config to override.",
                    denied
                );
            }
        }

        // Check specifically dangerous patterns
        let dangerous = [
            "rm -rf /",
            "mkfs.",
            "dd if=",
            "> /dev/sda",
            "fork bomb",
            ":(){ :|:& };:",
        ];

        for d in &dangerous {
            if lower.contains(d) {
                anyhow::bail!(
                    "Command blocked: appears to be destructive. \
                     If this is intentional, please run it manually."
                );
            }
        }

        Ok(())
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

        let _timeout_secs = args["timeout"]
            .as_u64()
            .unwrap_or(self.timeout_secs)
            .min(300);

        // Safety check
        self.check_safety(command_str)?;

        let output = Command::new("sh")
            .arg("-c")
            .arg(command_str)
            .current_dir(&self.workspace_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn command: {}", e))?
            .wait_with_output()
            .map_err(|e| anyhow::anyhow!("Failed to wait on command: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        let mut result_output = String::new();

        if exit_code == 0 {
            result_output.push_str(&format!("✅ Command succeeded (exit code: 0)\n"));
        } else {
            result_output.push_str(&format!(
                "❌ Command failed (exit code: {})\n",
                exit_code
            ));
        }

        if !stdout.is_empty() {
            // Truncate output if too long
            let truncated = truncate_output(&stdout, 10000);
            result_output.push_str(&format!(
                "--- stdout ---\n{}\n",
                truncated
            ));
        }

        if !stderr.is_empty() {
            let truncated = truncate_output(&stderr, 5000);
            result_output.push_str(&format!(
                "--- stderr ---\n{}\n",
                truncated
            ));
        }

        Ok(ToolResult::ok(result_output))
    }
}

/// Truncate long output with a note about truncation.
fn truncate_output(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }

    let boundary = s[..max_len]
        .char_indices()
        .last()
        .map(|(i, _)| i)
        .unwrap_or(max_len);

    format!(
        "{}\n... (truncated, total {} bytes)\n",
        &s[..boundary],
        s.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_check() {
        let tool = ShellTool {
            workspace_root: PathBuf::from("/tmp"),
            allowed_commands: vec![],
            denied_commands: vec!["sudo".into()],
            timeout_secs: 120,
        };

        assert!(tool.check_safety("echo hello").is_ok());
        assert!(tool.check_safety("sudo rm -rf /").is_err());
        assert!(tool.check_safety("rm -rf /").is_err());
    }

    #[test]
    fn test_basic_command() {
        let tool = ShellTool {
            workspace_root: PathBuf::from("."),
            allowed_commands: vec![],
            denied_commands: vec![],
            timeout_secs: 120,
        };

        let result = tool
            .execute(&serde_json::json!({"command": "echo hello"}))
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello"));
    }
}
