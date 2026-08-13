//! Phase C tests: sensitive-path protection, secret scrubbing (self-made
//! patterns + entropy gate), binary detection, and the scrub latency
//! bound (protocol P5: 10MB through the scrub path < 200ms).

use lcode::config::ToolsConfig;
use lcode::tools::file::ReadFileTool;
use lcode::tools::scrub;
use lcode::tools::Tool;

fn read_tool(dir: &std::path::Path, config: ToolsConfig) -> ReadFileTool {
    ReadFileTool::new_with_root_and_config(dir.to_path_buf(), config)
}

// --- sensitive path protection ---

#[test]
fn sensitive_paths_are_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".env"), "TOKEN=abc").unwrap();
    std::fs::write(dir.path().join(".lcode.toml"), "[llm]").unwrap();
    std::fs::create_dir(dir.path().join(".ssh")).unwrap();
    std::fs::write(dir.path().join(".ssh/id_rsa"), "key").unwrap();
    let tool = read_tool(dir.path(), ToolsConfig::default());

    for path in [".env", ".lcode.toml", ".ssh/id_rsa"] {
        let result = tool.execute(&serde_json::json!({ "path": path })).unwrap();
        assert!(!result.success, "{path} must be refused");
        assert!(result.output.contains("sensitive path"), "{path}: {}", result.output);
    }
}

#[test]
fn normal_files_still_read_with_default_sensitive_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.md"), "hello\n").unwrap();
    let tool = read_tool(dir.path(), ToolsConfig::default());
    let result = tool.execute(&serde_json::json!({ "path": "notes.md" })).unwrap();
    assert!(result.success, "{}", result.output);
}

// --- scrubbing ---

#[test]
fn high_signal_tokens_are_redacted() {
    let text = "key=sk-abcdefghijklmnop123456\naws=AKIAIOSFODNN7EXAMPLE\ngithub=ghp_abcdefghijklmnopqrstuvwxyz1234";
    let scrubbed = scrub::scrub_secrets(text);
    assert!(!scrubbed.contains("sk-abcdefghijklmnop123456"), "{scrubbed}");
    assert!(!scrubbed.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(!scrubbed.contains("ghp_abcdefghijklmnopqrstuvwxyz1234"));
    assert!(scrubbed.contains("[REDACTED]"));
}

#[test]
fn private_key_block_is_collapsed() {
    let text = "pub\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc\nsecret-body\n-----END RSA PRIVATE KEY-----\nafter\n";
    let scrubbed = scrub::scrub_secrets(text);
    assert!(scrubbed.contains("[REDACTED PRIVATE KEY BLOCK]"));
    assert!(!scrubbed.contains("MIIabc"));
    assert!(scrubbed.contains("pub"));
    assert!(scrubbed.contains("after"));
}

#[test]
fn generic_assignment_redacts_only_high_entropy_values() {
    let random = "password: \"f8X2!kP9#qL7v\"";
    let plain = "password: \"hello world\"";
    let scrubbed = scrub::scrub_secrets(&format!("{random}\n{plain}\n"));
    assert!(scrubbed.contains("password: \"[REDACTED]\""));
    assert!(scrubbed.contains("password: \"hello world\""), "low entropy stays: {scrubbed}");
}

#[test]
fn cjk_text_passes_scrub_unchanged() {
    let text = "注释：这个文件包含中文说明，密钥是 sk-demo1234567890abcdef 请勿泄露。\n";
    let scrubbed = scrub::scrub_secrets(text);
    assert!(scrubbed.contains("中文说明"));
    assert!(!scrubbed.contains("sk-demo1234567890abcdef"));
}

#[test]
fn scrub_disabled_config_leaves_content_alone() {
    let dir = tempfile::TempDir::new().unwrap();
    let secret = "token sk-abcdefghijklmnop123456 here";
    std::fs::write(dir.path().join("f.txt"), secret).unwrap();
    let config = ToolsConfig { scrub_secrets: false, ..ToolsConfig::default() };
    let tool = read_tool(dir.path(), config);
    let result = tool.execute(&serde_json::json!({ "path": "f.txt" })).unwrap();
    assert!(result.output.contains("sk-abcdefghijklmnop123456"), "scrub off keeps content");
}

// --- binary detection ---

#[test]
fn binary_content_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 3, 255, 0, 9]).unwrap();
    let tool = read_tool(dir.path(), ToolsConfig::default());
    let result = tool.execute(&serde_json::json!({ "path": "bin.dat" })).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("binary"), "{}", result.output);
}

#[test]
fn utf8_text_with_high_bytes_is_not_binary() {
    // Valid UTF-8 Chinese must pass looks_binary.
    let text = "中文内容测试\n".as_bytes().to_vec();
    assert!(!scrub::looks_binary(&text));
}

// --- scrub latency bound (protocol P5) ---

#[test]
fn scrub_10mb_text_under_200ms() {
    let chunk = "line with some words and a token=abc123 value\n".repeat(20_000);
    let big = chunk.repeat(12); // ~11MB
    let start = std::time::Instant::now();
    let _scrubbed = scrub::scrub_secrets(&big);
    let elapsed = start.elapsed();
    assert!(elapsed.as_millis() < 200, "scrubbing 10MB took {:?} (bound: 200ms)", elapsed);
}

// --- config defaults ---

#[test]
fn tools_config_sensitive_defaults() {
    let config = ToolsConfig::default();
    assert!(config.scrub_secrets);
    for pattern in [".env", ".env.*", ".lcode.toml", "*.pem", "id_rsa*", ".ssh/*"] {
        assert!(config.sensitive_paths.iter().any(|p| p == pattern), "missing: {pattern}");
    }
}
