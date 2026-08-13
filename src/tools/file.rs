//! File operation tools.
//!
//! Tools for reading and writing files. `write_file` doubles as the
//! find-and-replace editor via its optional `replace` argument, so the
//! tool surface stays at a single write path. Both tools accept
//! http(s) URLs (read_file.path / write_file.url) with the host policy,
//! size cap and timeouts enforced by [`super::fetch`].

use crate::config::{Config, ToolsConfig};
use crate::tools::{Tool, ToolResult};
use std::path::{Path, PathBuf};

/// In-place edit: replace the unique exact match of `old_string` with
/// `new_string` (the former edit_file semantics, folded into write_file
/// so the tool surface keeps a single write path).
fn apply_replace(
    full_path: &Path,
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
                "description": "The content to write (ignored when `replace` or `url` is set)"
            },
            "url": {
                "type": "string",
                "description": "Fetch this http(s) URL and write it to `path` (requires tools.enable_web)"
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

/// Enforce `tools.allowed_dirs`: the target must live under the
/// workspace root (default) or one of the listed directories. Resolves
/// `..` and symlinks by canonicalizing the deepest existing ancestor,
/// then re-joining the not-yet-created tail (writes may create files).
fn check_path_allowed(root: &Path, allowed: &[String], target: &Path) -> anyhow::Result<()> {
    let abs = if target.is_absolute() { target.to_path_buf() } else { root.join(target) };
    let mut existing = abs.as_path();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        tail.push(existing.file_name().unwrap_or_default().to_os_string());
        existing = existing.parent().ok_or_else(|| {
            anyhow::anyhow!("path has no existing ancestor: {}", target.display())
        })?;
    }
    let mut resolved = existing.canonicalize()?;
    for seg in tail.iter().rev() {
        resolved = resolved.join(seg);
    }
    let roots: Vec<PathBuf> = if allowed.is_empty() {
        vec![root.canonicalize().unwrap_or_else(|_| root.to_path_buf())]
    } else {
        allowed.iter().map(|d| root.join(PathBuf::from(d))).collect()
    };
    let inside = roots.iter().any(|r| {
        let r = r.canonicalize().unwrap_or_else(|_| r.clone());
        resolved.starts_with(&r)
    });
    if !inside {
        anyhow::bail!("path outside allowed directories: {}", target.display());
    }
    Ok(())
}

/// Atomic write: temp file next to the target, then rename over it.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Content entering the LLM context: binary data is refused, secrets
/// are scrubbed (when enabled). URL fetches pass through the same gate.
fn text_for_context(bytes: &[u8], config: &ToolsConfig) -> anyhow::Result<String> {
    if crate::tools::scrub::looks_binary(bytes) {
        anyhow::bail!("binary content (not text); use bash to handle it");
    }
    let text = String::from_utf8(bytes.to_vec()).expect("non-binary check passed");
    if config.scrub_secrets {
        Ok(crate::tools::scrub::scrub_secrets(&text))
    } else {
        Ok(text)
    }
}

/// Number the lines of `text` honouring offset/limit. Returns the
/// numbered output plus (start, end, total) for the summary line.
fn numbered_lines(text: &str, args: &serde_json::Value) -> (String, usize, usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().map(|n| n as usize).unwrap_or(lines.len());
    let start = offset.min(lines.len());
    let end = (start + limit).min(lines.len());
    let output = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");
    (output, start, end, lines.len())
}

/// Summary line for a read: bytes/kind plus the line range.
fn read_summary(path_str: &str, start: usize, end: usize, total: usize, prefix: &str) -> String {
    format!(
        "{}: Read {} lines ({} to {} of {}) from {}",
        prefix,
        end - start,
        start + 1,
        end,
        total,
        path_str
    )
}

/// Tool for reading file contents (local paths or http(s) URLs).
pub struct ReadFileTool {
    workspace_root: PathBuf,
    config: ToolsConfig,
}

impl ReadFileTool {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self { workspace_root: std::env::current_dir()?, config: config.tools.clone() })
    }

    /// Create a tool rooted at `root` with default tool settings.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn new_with_root(root: PathBuf) -> Self {
        Self { workspace_root: root, config: ToolsConfig::default() }
    }

    /// Create a tool rooted at `root` with explicit tool settings.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn new_with_root_and_config(root: PathBuf, config: ToolsConfig) -> Self {
        Self { workspace_root: root, config }
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path (or fetch an \
         http(s) URL). Returns the file contents with line numbers."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to the file to read (relative to workspace root), or an http(s) URL"
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

        if crate::tools::fetch::is_http_url(path_str) {
            let (bytes, content_type) = crate::tools::fetch::fetch_url(path_str, &self.config)?;
            let text = match text_for_context(&bytes, &self.config) {
                Ok(text) => text,
                Err(e) => return Ok(ToolResult::err(e.to_string())),
            };
            let (output, start, end, total) = numbered_lines(&text, args);
            let kind = content_type.as_deref().unwrap_or("unknown content type");
            let prefix = format!("Fetched {} bytes ({})", bytes.len(), kind);
            let summary = read_summary(path_str, start, end, total, &prefix);
            return Ok(ToolResult::ok(format!("{}\n\n{}", summary, output)));
        }

        if crate::tools::scrub::is_sensitive_path(path_str, &self.config.sensitive_paths) {
            return Ok(ToolResult::err(format!("refusing to read sensitive path: {}", path_str)));
        }

        let full_path = self.workspace_root.join(path_str);
        check_path_allowed(&self.workspace_root, &self.config.allowed_dirs, &full_path)?;

        if !full_path.exists() {
            return Ok(ToolResult::err(format!("File not found: {}", path_str)));
        }
        if !full_path.is_file() {
            return Ok(ToolResult::err(format!("Not a file: {}", path_str)));
        }

        let bytes = std::fs::read(&full_path)?;
        let content = match text_for_context(&bytes, &self.config) {
            Ok(content) => content,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let (output, start, end, total) = numbered_lines(&content, args);
        let summary = read_summary(path_str, start, end, total, "");
        Ok(ToolResult::ok(format!("{}\n\n{}", summary.trim(), output)))
    }
}

/// Tool for writing file contents (full write, in-place replace, or
/// URL fetch to path).
pub struct WriteFileTool {
    workspace_root: PathBuf,
    config: ToolsConfig,
}

impl WriteFileTool {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self { workspace_root: std::env::current_dir()?, config: config.tools.clone() })
    }

    /// Create a tool rooted at `root` with default tool settings.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn new_with_root(root: PathBuf) -> Self {
        Self { workspace_root: root, config: ToolsConfig::default() }
    }

    /// Create a tool rooted at `root` with explicit tool settings.
    /// Hidden: only used by tests in tests/.
    #[doc(hidden)]
    pub fn new_with_root_and_config(root: PathBuf, config: ToolsConfig) -> Self {
        Self { workspace_root: root, config }
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file, edit it in place via an optional \
         `replace` object (unique exact match), or fetch an http(s) URL \
         into the file via the optional `url` argument."
    }

    fn parameters(&self) -> serde_json::Value {
        write_file_schema()
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str =
            args["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        let full_path = self.workspace_root.join(path_str);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        check_path_allowed(&self.workspace_root, &self.config.allowed_dirs, &full_path)?;

        // In-place edit mode: unique exact match, replacen once.
        if let Some(replace) = args["replace"].as_object() {
            return apply_replace(&full_path, path_str, replace);
        }

        // URL fetch mode: fetch then write atomically (temp + rename).
        if let Some(url) = args["url"].as_str() {
            let (bytes, _content_type) = crate::tools::fetch::fetch_url(url, &self.config)?;
            write_atomic(&full_path, &bytes)?;
            return Ok(ToolResult::ok(format!(
                "Fetched {} bytes from {} to {}",
                bytes.len(),
                url,
                path_str
            )));
        }

        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' argument (or 'replace'/'url')"))?;
        std::fs::write(&full_path, content)?;

        let size = content.len();
        let lines = content.lines().count();
        Ok(ToolResult::ok(format!("Wrote {} bytes ({} lines) to {}", size, lines, path_str)))
    }
}
