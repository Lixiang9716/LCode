//! Find-and-replace file editing tool.

use crate::config::Config;
use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;

/// Tool for editing files with find-and-replace.
pub struct EditFileTool {
    workspace_root: PathBuf,
}

impl EditFileTool {
    pub fn new(_config: &Config) -> anyhow::Result<Self> {
        Ok(Self { workspace_root: std::env::current_dir()? })
    }

    /// Create a tool rooted at `root` instead of the current directory.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn new_with_root(root: PathBuf) -> Self {
        Self { workspace_root: root }
    }
}

impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string match with new content. \
         The old_string must match exactly (including whitespace) and be unique in the file."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to edit (relative to workspace root)"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The new text to replace it with"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str =
            args["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        let old = args["old_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'old_string' argument"))?;
        let new = args["new_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'new_string' argument"))?;

        let full_path = self.workspace_root.join(path_str);

        if !full_path.exists() {
            return Ok(ToolResult::err(format!("File not found: {}", path_str)));
        }

        let content = std::fs::read_to_string(&full_path)?;

        let count = content.matches(old).count();
        if count == 0 {
            return Ok(ToolResult::err("old_string not found in file"));
        }
        if count > 1 {
            return Ok(ToolResult::err(format!(
                "old_string found {} times in file — must be unique. \
                 Use a larger string with more surrounding context.",
                count
            )));
        }

        let new_content = content.replacen(old, new, 1);
        std::fs::write(&full_path, new_content)?;

        Ok(ToolResult::ok(format!("Successfully edited {}", path_str)))
    }
}
