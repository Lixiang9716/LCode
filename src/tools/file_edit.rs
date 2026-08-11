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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_edit_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("edit.txt"), "hello world\nfoo bar\n").unwrap();

        let tool = EditFileTool { workspace_root: dir.path().to_path_buf() };
        let result = tool
            .execute(&serde_json::json!({
                "path": "edit.txt",
                "old_string": "hello world",
                "new_string": "hi there"
            }))
            .unwrap();
        assert!(result.success);

        let content = std::fs::read_to_string(dir.path().join("edit.txt")).unwrap();
        assert!(content.contains("hi there"));
        assert!(!content.contains("hello world"));
    }

    #[test]
    fn test_edit_file_old_string_not_found() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("edit.txt"), "hello world\n").unwrap();
        let tool = EditFileTool { workspace_root: dir.path().to_path_buf() };

        let result = tool
            .execute(&serde_json::json!({
                "path": "edit.txt",
                "old_string": "does not exist anywhere",
                "new_string": "replacement"
            }))
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("not found"));
    }

    #[test]
    fn test_edit_file_not_unique() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("dup.txt"), "abc\ndef\nabc\n").unwrap();
        let tool = EditFileTool { workspace_root: dir.path().to_path_buf() };

        let result = tool
            .execute(&serde_json::json!({
                "path": "dup.txt",
                "old_string": "abc",
                "new_string": "xyz"
            }))
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("must be unique"));

        // File must be left unchanged.
        let content = std::fs::read_to_string(dir.path().join("dup.txt")).unwrap();
        assert_eq!(content, "abc\ndef\nabc\n");
    }

    #[test]
    fn test_edit_file_multiline() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("multi.txt"), "fn foo() {\n    old_body\n}\n").unwrap();
        let tool = EditFileTool { workspace_root: dir.path().to_path_buf() };

        let result = tool
            .execute(&serde_json::json!({
                "path": "multi.txt",
                "old_string": "fn foo() {\n    old_body\n}",
                "new_string": "fn foo() {\n    new_body\n}"
            }))
            .unwrap();
        assert!(result.success);

        let content = std::fs::read_to_string(dir.path().join("multi.txt")).unwrap();
        assert!(content.contains("new_body"));
        assert!(!content.contains("old_body"));
    }
}
