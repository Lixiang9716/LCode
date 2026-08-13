//! Unit tests for the tools module (`lcode::tools`).
//!
//! Migrated verbatim from `src/tools/` so that `src/` contains no test code
//! (see scripts/check-style.sh).

use std::path::PathBuf;

use tempfile::TempDir;

use lcode::config::Config;
use lcode::tools::file::{ReadFileTool, WriteFileTool};
use lcode::tools::search::{GlobTool, GrepTool};
use lcode::tools::shell::ShellTool;
use lcode::tools::{Tool, ToolRegistry, ToolResult};

// --- ToolRegistry / ToolResult ---

/// A minimal custom tool used to test registry registration.
struct TestTool;

impl Tool for TestTool {
    fn name(&self) -> &str {
        "test_tool"
    }

    fn description(&self) -> &str {
        "A test-only tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok("test tool ran"))
    }
}

const BUILTIN_TOOLS: [&str; 5] = ["read_file", "write_file", "grep", "glob", "shell"];

#[test]
fn test_registry_registers_builtin_tools() {
    let registry = ToolRegistry::new(&Config::default()).unwrap();

    assert_eq!(registry.definitions().len(), BUILTIN_TOOLS.len());
    let tools = registry.list_tools();
    for name in BUILTIN_TOOLS {
        assert!(tools.contains(&name), "missing built-in tool: {}", name);
    }
}

#[test]
fn test_registry_register_custom_tool() {
    let mut registry = ToolRegistry::new(&Config::default()).unwrap();
    let before = registry.definitions().len();

    registry.register(Box::new(TestTool));

    assert_eq!(registry.definitions().len(), before + 1);
    assert!(registry.list_tools().contains(&"test_tool"));
    // Definitions carry the tool's name/description/parameters.
    let def = registry
        .definitions()
        .into_iter()
        .find(|d| d.function.name == "test_tool")
        .expect("custom tool definition should be present");
    assert_eq!(def.function.description, "A test-only tool");
    assert_eq!(def.tool_type, "function");
}

#[test]
fn test_registry_execute_unknown_tool() {
    let registry = ToolRegistry::new(&Config::default()).unwrap();

    let err = registry.execute("no_such_tool", &serde_json::json!({})).unwrap_err();
    assert!(err.to_string().contains("Unknown tool"));
    assert!(err.to_string().contains("no_such_tool"));
}

#[test]
fn test_registry_execute_known_tool() {
    let registry = ToolRegistry::new(&Config::default()).unwrap();

    let result = registry.execute("grep", &serde_json::json!({"pattern": "x"})).unwrap();
    assert!(result.success);
}

#[test]
fn test_tool_result_display_ok() {
    let ok = ToolResult::ok("all good");
    let text = ok.to_string();
    assert_eq!(text, "all good");
    assert!(!text.starts_with("Error:"));
}

#[test]
fn test_tool_result_display_err() {
    let err = ToolResult::err("bad input");
    let text = err.to_string();
    assert_eq!(text, "Error: bad input");
    assert!(!err.success);
}

// --- file.rs: ReadFileTool / WriteFileTool ---

#[test]
fn test_read_file() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.txt");
    std::fs::write(&file_path, "line 1\nline 2\nline 3\n").unwrap();

    let tool = ReadFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"path": "test.txt"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("line 1"));
    assert!(result.output.contains("line 3"));
}

#[test]
fn test_write_and_read_file() {
    let dir = TempDir::new().unwrap();

    let writer = WriteFileTool::new_with_root(dir.path().to_path_buf());
    let result = writer
        .execute(&serde_json::json!({
            "path": "output.txt",
            "content": "Hello, world!"
        }))
        .unwrap();
    assert!(result.success);

    let reader = ReadFileTool::new_with_root(dir.path().to_path_buf());
    let result = reader.execute(&serde_json::json!({"path": "output.txt"})).unwrap();
    assert!(result.output.contains("Hello, world!"));
}

#[test]
fn test_read_file_offset_beyond_lines() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.txt"), "line 1\nline 2\nline 3\nline 4\nline 5\n")
        .unwrap();

    let tool = ReadFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"path": "test.txt", "offset": 100})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("Read 0 lines"));
}

#[test]
fn test_read_file_limit_truncates() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.txt"), "line 1\nline 2\nline 3\nline 4\nline 5\n")
        .unwrap();

    let tool = ReadFileTool::new_with_root(dir.path().to_path_buf());

    let result =
        tool.execute(&serde_json::json!({"path": "test.txt", "offset": 1, "limit": 2})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("line 2"));
    assert!(result.output.contains("line 3"));
    assert!(!result.output.contains("line 4"));
}

#[test]
fn test_read_file_not_found() {
    let dir = TempDir::new().unwrap();
    let tool = ReadFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"path": "missing.txt"})).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("File not found"));
}

#[test]
fn test_read_file_is_directory() {
    let dir = TempDir::new().unwrap();
    let tool = ReadFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"path": "."})).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("Not a file"));
}

#[test]
fn test_write_file_creates_nested_dirs() {
    let dir = TempDir::new().unwrap();
    let tool = WriteFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool
        .execute(&serde_json::json!({
            "path": "a/b/c/deep.txt",
            "content": "deep content"
        }))
        .unwrap();
    assert!(result.success);
    assert!(dir.path().join("a/b/c/deep.txt").is_file());
    let written = std::fs::read_to_string(dir.path().join("a/b/c/deep.txt")).unwrap();
    assert_eq!(written, "deep content");
}

#[test]
fn test_write_file_empty_content() {
    let dir = TempDir::new().unwrap();
    let tool = WriteFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"path": "empty.txt", "content": ""})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("Wrote 0 bytes (0 lines)"));
    assert_eq!(std::fs::read_to_string(dir.path().join("empty.txt")).unwrap(), "");
}

#[test]
fn test_write_file_replace_unique_match() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "alpha
beta
gamma
",
    )
    .unwrap();
    let tool = WriteFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool
        .execute(&serde_json::json!({
            "path": "notes.txt",
            "replace": { "old_string": "beta", "new_string": "bravo" }
        }))
        .unwrap();
    assert!(result.success, "{}", result.output);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "alpha
bravo
gamma
"
    );
}

#[test]
fn test_write_file_replace_zero_matches() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "alpha
",
    )
    .unwrap();
    let tool = WriteFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool
        .execute(&serde_json::json!({
            "path": "notes.txt",
            "replace": { "old_string": "missing", "new_string": "x" }
        }))
        .unwrap();
    assert!(!result.success);
    assert!(result.output.contains("not found"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "alpha
"
    );
}

#[test]
fn test_write_file_replace_multiple_matches() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "x
x
",
    )
    .unwrap();
    let tool = WriteFileTool::new_with_root(dir.path().to_path_buf());

    let result = tool
        .execute(&serde_json::json!({
            "path": "notes.txt",
            "replace": { "old_string": "x", "new_string": "y" }
        }))
        .unwrap();
    assert!(!result.success);
    assert!(result.output.contains("must be unique"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "x
x
"
    );
}

// --- search.rs: GrepTool / GlobTool ---

/// Create a tempdir with a few files: two Rust files containing "needle"
/// (one nested), and one text file without it.
fn setup_search_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("nested")).unwrap();
    std::fs::write(dir.path().join("alpha.rs"), "fn alpha() {\n    let needle = 42;\n}\n").unwrap();
    std::fs::write(
        dir.path().join("nested/beta.rs"),
        "fn beta() {\n    println!(\"needle here\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("gamma.txt"), "nothing interesting in this text file\n")
        .unwrap();
    dir
}

#[test]
fn test_grep_finds_matches() {
    let dir = setup_search_dir();
    let tool = GrepTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"pattern": "needle"})).unwrap();
    assert!(result.success);
    // Both matching files with their line number (file:line) must appear.
    assert!(result.output.contains("alpha.rs:2"));
    assert!(result.output.contains("beta.rs:2"));
    // Line content must be included.
    assert!(result.output.contains("let needle = 42"));
    assert!(result.output.contains("needle here"));
    // The non-matching file must not appear.
    assert!(!result.output.contains("gamma.txt"));
}

#[test]
fn test_grep_no_matches() {
    let dir = setup_search_dir();
    let tool = GrepTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"pattern": "zzz_nonexistent_pattern"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("No matches found"));
}

#[test]
fn test_grep_invalid_regex() {
    let dir = setup_search_dir();
    let tool = GrepTool::new_with_root(dir.path().to_path_buf());

    // Unbalanced paren is an invalid regex: rg fails and the builtin
    // fallback reports the invalid pattern.
    let result = tool.execute(&serde_json::json!({"pattern": "("}));
    assert!(result.is_err());
}

#[test]
fn test_glob_recursive_rs_files() {
    let dir = setup_search_dir();
    let tool = GlobTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"pattern": "**/*.rs"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("alpha.rs"));
    assert!(result.output.contains("beta.rs"));
    assert!(result.output.contains("nested/beta.rs"));
    assert!(!result.output.contains("gamma.txt"));
}

#[test]
fn test_glob_no_matches() {
    let dir = setup_search_dir();
    let tool = GlobTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"pattern": "*.py"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("No files matching"));
}

#[test]
fn test_glob_invalid_pattern() {
    let dir = setup_search_dir();
    let tool = GlobTool::new_with_root(dir.path().to_path_buf());

    let result = tool.execute(&serde_json::json!({"pattern": "["}));
    assert!(result.is_err());
}

// --- shell.rs: ShellTool ---

#[test]
fn test_safety_check() {
    let tool = ShellTool::with_denied(PathBuf::from("/tmp"), vec!["sudo".into()]);

    assert!(tool.check_safety("echo hello").is_ok());
    assert!(tool.check_safety("sudo rm -rf /").is_err());
    assert!(tool.check_safety("rm -rf /").is_err());
}

#[test]
fn test_basic_command() {
    let tool = ShellTool::new_with_root(PathBuf::from("."));

    let result = tool.execute(&serde_json::json!({"command": "echo hello"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("hello"));
}

#[test]
fn test_safety_check_dangerous_patterns() {
    let tool = ShellTool::new_with_root(PathBuf::from("/tmp"));

    assert!(tool.check_safety("rm -rf /").is_err());
    assert!(tool.check_safety("RM -RF /").is_err(), "case-insensitive");
    assert!(tool.check_safety("dd if=/dev/zero of=/dev/sda").is_err());
    assert!(tool.check_safety("mkfs.ext4 /dev/sdb1").is_err());
    assert!(tool.check_safety("echo data > /dev/sda").is_err());
    assert!(tool.check_safety(":(){ :|:& };:").is_err());
    assert!(tool.check_safety("echo safe command").is_ok());
    assert!(tool.check_safety("mkdir -p /tmp/foo").is_ok());
}

#[test]
fn test_safety_check_denied_commands() {
    let tool =
        ShellTool::with_denied(PathBuf::from("/tmp"), vec!["git push".into(), "curl".into()]);

    assert!(tool.check_safety("git push origin main").is_err());
    assert!(tool.check_safety("CURL -s https://example.com").is_err());
    assert!(tool.check_safety("git status").is_ok());
    assert!(tool.check_safety("echo hello").is_ok());
}

#[test]
fn test_command_with_stderr() {
    let tool = ShellTool::new_with_root(PathBuf::from("."));

    let result =
        tool.execute(&serde_json::json!({"command": "echo warning >&2; echo out"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("Command succeeded"));
    assert!(result.output.contains("--- stdout ---"));
    assert!(result.output.contains("out"));
    assert!(result.output.contains("--- stderr ---"));
    assert!(result.output.contains("warning"));
}

#[test]
fn test_command_failure_exit_code() {
    let tool = ShellTool::new_with_root(PathBuf::from("."));

    let result = tool.execute(&serde_json::json!({"command": "echo oops >&2; exit 3"})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("Command failed"));
    assert!(result.output.contains("exit code: 3"));
    assert!(result.output.contains("oops"));
}

#[test]
fn test_execute_with_timeout_param() {
    let tool = ShellTool::new_with_root(PathBuf::from("."));

    // timeout field must be parsed without error (and clamped to max 300).
    let result = tool.execute(&serde_json::json!({"command": "echo hi", "timeout": 5})).unwrap();
    assert!(result.success);
    assert!(result.output.contains("hi"));

    let result = tool.execute(&serde_json::json!({"command": "echo hi", "timeout": 9999})).unwrap();
    assert!(result.success);
}
