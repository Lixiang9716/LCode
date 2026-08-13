//! Code search tools.
//!
//! Tools for searching codebases: grep (content search) and glob (filename search).

use crate::config::{Config, ToolsConfig};
use crate::tools::{Tool, ToolResult};
use std::path::PathBuf;
use std::process::Command;

/// Tool for searching file contents using grep-like functionality.
pub struct GrepTool {
    workspace_root: PathBuf,
    config: ToolsConfig,
}

impl GrepTool {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self { workspace_root: std::env::current_dir()?, config: config.tools.clone() })
    }

    /// Create a tool rooted at `root` instead of the current directory.
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

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a pattern in files. Returns matching lines with file paths and line numbers."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to workspace root)"
                },
                "include": {
                    "type": "string",
                    "description": "File pattern to include (e.g., '*.rs')"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 50)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' argument"))?;

        let search_path = args["path"]
            .as_str()
            .map(|p| self.workspace_root.join(p))
            .unwrap_or_else(|| self.workspace_root.clone());

        let max_results = args["max_results"].as_u64().unwrap_or(50) as usize;

        // Try using system grep or rg for speed
        let output = if let Ok(result) = run_rg(pattern, &search_path, max_results) {
            result
        } else {
            run_builtin_grep(pattern, &search_path, max_results)?
        };

        // Guardrails: drop matches inside sensitive paths, scrub detected
        // secrets from the remaining lines — the same context gate that
        // read_file applies.
        let (filtered, hidden) = crate::tools::guard::filter_sensitive_lines(&output, &self.config);
        if filtered.trim().is_empty() {
            if hidden > 0 {
                return Ok(ToolResult::ok(format!(
                    "No matches found for '{pattern}' ({hidden} sensitive matches hidden)"
                )));
            }
            return Ok(ToolResult::ok(format!("No matches found for '{pattern}'")));
        }

        let scrubbed = crate::tools::scrub::scrub_secrets(&filtered);
        let lines = scrubbed.lines().count();
        let note = if hidden > 0 {
            format!("\n({hidden} sensitive matches hidden)")
        } else {
            String::new()
        };
        Ok(ToolResult::ok(format!("Found {lines} matches for '{pattern}':\n{scrubbed}{note}")))
    }
}

/// Try using ripgrep for fast search.
fn run_rg(pattern: &str, path: &PathBuf, max_results: usize) -> std::result::Result<String, ()> {
    let output = Command::new("rg")
        .args(["--no-heading", "--line-number", "--color=never"])
        .arg("-m")
        .arg(max_results.to_string())
        .arg(pattern)
        .arg(path)
        .output()
        .map_err(|_| ())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(())
    }
}

/// Built-in grep fallback using Rust iterators.
fn run_builtin_grep(pattern: &str, path: &PathBuf, max_results: usize) -> anyhow::Result<String> {
    let regex = regex_lite::Regex::new(pattern)
        .map_err(|e| anyhow::anyhow!("Invalid regex pattern: {}", e))?;

    let mut results = Vec::new();
    let walker = walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file());

    'outer: for entry in walker {
        let file_path = entry.path();
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (line_num, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                let rel_path = file_path.strip_prefix(path).unwrap_or(file_path);
                results.push(format!("{}:{}: {}", rel_path.display(), line_num + 1, line));
                if results.len() >= max_results {
                    break 'outer;
                }
            }
        }
    }

    Ok(results.join("\n"))
}

/// Tool for finding files by glob pattern.
pub struct GlobTool {
    workspace_root: PathBuf,
    config: ToolsConfig,
}

impl GlobTool {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self { workspace_root: std::env::current_dir()?, config: config.tools.clone() })
    }

    /// Create a tool rooted at `root` instead of the current directory.
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

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern (e.g., '**/*.rs', 'src/**/mod.rs')."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match files against"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to workspace root)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let pattern_str = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' argument"))?;

        let search_path = args["path"]
            .as_str()
            .map(|p| self.workspace_root.join(p))
            .unwrap_or_else(|| self.workspace_root.clone());

        let pattern = glob::Pattern::new(pattern_str)
            .map_err(|e| anyhow::anyhow!("Invalid glob pattern: {}", e))?;

        let matches: Vec<String> = walkdir::WalkDir::new(&search_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let rel = e.path().strip_prefix(&search_path).unwrap_or(e.path());
                pattern.matches_path(rel)
            })
            .map(|e| {
                let rel = e.path().strip_prefix(&search_path).unwrap_or(e.path());
                if e.file_type().is_dir() {
                    format!("{}/", rel.display())
                } else {
                    rel.display().to_string()
                }
            })
            .collect();

        // Guardrails: sensitive paths are not revealed by listing.
        let (hidden_vec, visible): (Vec<String>, Vec<String>) = matches
            .into_iter()
            .partition(|m| crate::tools::scrub::is_sensitive_path(m, &self.config.sensitive_paths));
        let hidden = hidden_vec.len();
        if visible.is_empty() {
            if hidden > 0 {
                return Ok(ToolResult::ok(format!(
                    "No files matching '{pattern_str}' ({hidden} sensitive matches hidden)"
                )));
            }
            return Ok(ToolResult::ok(format!("No files matching '{pattern_str}'")));
        }

        let note = if hidden > 0 {
            format!("\n({hidden} sensitive matches hidden)")
        } else {
            String::new()
        };
        Ok(ToolResult::ok(format!(
            "Found {} files matching '{}':\n{}{}",
            visible.len(),
            pattern_str,
            visible.join("\n"),
            note
        )))
    }
}
