//! File operation tools.
//!
//! Tools for reading, writing, and listing files and directories.
//! The find-and-replace editor lives in [`super::file_edit`].

use crate::config::Config;
use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;

/// Tool for reading file contents.
pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
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

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path. Returns the file contents with line numbers."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to read (relative to workspace root)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (0-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str =
            args["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;

        let full_path = self.workspace_root.join(path_str);

        if !full_path.exists() {
            return Ok(ToolResult::err(format!("File not found: {}", path_str)));
        }

        if !full_path.is_file() {
            return Ok(ToolResult::err(format!("Not a file: {}", path_str)));
        }

        let content = std::fs::read_to_string(&full_path)?;
        let lines: Vec<&str> = content.lines().collect();

        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().map(|n| n as usize).unwrap_or(lines.len());

        let start = offset.min(lines.len());
        let end = (start + limit).min(lines.len());

        let numbered: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect();

        let output = numbered.join("\n");
        let summary = format!(
            "Read {} lines ({} to {} of {}) from {}",
            end - start,
            start + 1,
            end,
            lines.len(),
            path_str
        );

        Ok(ToolResult::ok(format!("{}\n\n{}", summary, output)))
    }
}

/// Tool for writing file contents.
pub struct WriteFileTool {
    workspace_root: PathBuf,
}

impl WriteFileTool {
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

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to write (relative to workspace root)"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str =
            args["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' argument"))?;

        let full_path = self.workspace_root.join(path_str);

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&full_path, content)?;

        let size = content.len();
        let lines = content.lines().count();
        Ok(ToolResult::ok(format!("Wrote {} bytes ({} lines) to {}", size, lines, path_str)))
    }
}

/// Tool for listing directory contents.
pub struct ListDirTool {
    workspace_root: PathBuf,
}

impl ListDirTool {
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

impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the contents of a directory."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the directory to list (relative to workspace root, defaults to root)"
                }
            },
            "required": []
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str = args["path"].as_str().unwrap_or(".");
        let full_path = self.workspace_root.join(path_str);

        if !full_path.exists() {
            return Ok(ToolResult::err(format!("Directory not found: {}", path_str)));
        }

        if !full_path.is_dir() {
            return Ok(ToolResult::err(format!("Not a directory: {}", path_str)));
        }

        let entries: Vec<String> = std::fs::read_dir(&full_path)?
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let file_type = e.file_type().ok();
                let is_dir = file_type.map(|ft| ft.is_dir()).unwrap_or(false);
                if is_dir {
                    format!("{}/", name)
                } else {
                    name
                }
            })
            .collect();

        let output = entries.join("\n");
        Ok(ToolResult::ok(format!(
            "Contents of {} ({} items):\n{}",
            path_str,
            entries.len(),
            output
        )))
    }
}
