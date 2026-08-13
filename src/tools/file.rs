//! File operation tools.
//!
//! Tools for reading and writing files. `write_file` doubles as the
//! find-and-replace editor via its optional `replace` argument, so the
//! tool surface stays at a single write path.

use crate::config::Config;
use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;

/// In-place edit: replace the unique exact match of `old_string` with
/// `new_string` (the former edit_file semantics, folded into write_file
/// so the tool surface keeps a single write path).
fn apply_replace(
    full_path: &std::path::Path,
    path_str: &str,
    replace: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<ToolResult> {
    let old = replace
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'replace.old_string'"))?;
    let new = replace
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'replace.new_string'"))?;
    if !full_path.exists() {
        return Ok(ToolResult::err(format!("File not found: {}", path_str)));
    }
    let content = std::fs::read_to_string(full_path)?;
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
    std::fs::write(full_path, content.replacen(old, new, 1))?;
    Ok(ToolResult::ok(format!("Successfully edited {}", path_str)))
}

/// JSON schema for the write_file parameters (kept flat so the nested
/// replace object stays within the style indentation limit).
fn write_file_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "The path to the file to write (relative to workspace root)"
            },
            "content": {
                "type": "string",
                "description": "The content to write to the file (ignored when `replace` is set)"
            },
            "replace": {
                "type": "object",
                "description": "In-place edit: replace one exact string match",
                "properties": {
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find (must be unique in the file)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The text to replace it with"
                    }
                },
                "required": ["old_string", "new_string"]
            }
        },
        "required": ["path"]
    })
}

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
        "Write content to a file, or edit it in place via an optional \
         `replace` object: the old_string must match exactly once. \
         With `replace`, `content` is ignored."
    }

    fn parameters(&self) -> serde_json::Value {
        write_file_schema()
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str =
            args["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        let full_path = self.workspace_root.join(path_str);

        // In-place edit mode: unique exact match, replacen once. Mirrors
        // the former edit_file tool so one write path serves both.
        if let Some(replace) = args["replace"].as_object() {
            return apply_replace(&full_path, path_str, replace);
        }

        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' argument (or 'replace')"))?;

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
