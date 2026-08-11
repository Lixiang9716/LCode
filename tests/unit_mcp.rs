//! Unit tests for the MCP module (learn-claude-code s19): name
//! validation, mock/file tool loading, `mcp__{server}__{tool}` namespace
//! parsing, permission annotations on tool definitions, and the
//! `connect_mcp` tool.

use lcode::agent::{ConnectMcpTool, McpRegistry};
use lcode::tools::Tool;
use std::sync::{Arc, Mutex};

// --- connect: name validation -------------------------------------------

#[test]
fn test_connect_validates_names() {
    let mut registry = McpRegistry::default();
    for bad in ["My Server", "docs.foo", "中文", "", "a b"] {
        let err = registry.connect(bad, "mock://docs").unwrap_err();
        assert!(err.to_string().contains("Invalid MCP server name"), "{bad}: {err}");
    }
    for good in ["docs", "my_server", "a-b1", "x"] {
        registry.connect(good, "mock://docs").unwrap();
    }
    assert!(registry.list_servers().contains("- my_server ("));
}

// --- connect: tool loading ----------------------------------------------

#[test]
fn test_connect_mock_server_loads_tools() {
    let mut registry = McpRegistry::default();
    registry.connect("docs", "mock://docs").unwrap();
    registry.connect("deploy", "mock://deploy").unwrap();

    let listing = registry.list_servers();
    assert!(listing.contains("- docs (mock://docs, 2 tools)"));
    assert!(listing.contains("- deploy (mock://deploy, 2 tools)"));

    // Duplicate connects are rejected.
    assert!(registry.connect("docs", "mock://docs").is_err());
    // Unknown mock servers are rejected.
    assert!(registry.connect("db", "mock://db").is_err());
}

#[test]
fn test_connect_file_server_loads_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tools.json");
    std::fs::write(
        &path,
        r#"{
            "tools": [
                { "name": "list_issues", "description": "List issues",
                  "parameters": { "type": "object", "properties": {} },
                  "permissions": "readOnly" },
                { "name": "close_issue", "description": "Close an issue",
                  "parameters": { "type": "object", "properties": {} },
                  "permissions": "destructive" }
            ]
        }"#,
    )
    .unwrap();
    let mut registry = McpRegistry::default();
    let url = format!("file://{}", path.display());
    registry.connect("gh", &url).unwrap();
    assert!(registry.list_servers().contains(&format!("- gh ({url}, 2 tools)")));

    // Missing file.
    let missing = tmp.path().join("nope.json");
    assert!(registry.connect("bad", &format!("file://{}", missing.display())).is_err());
    // Malformed JSON.
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "not json").unwrap();
    assert!(registry.connect("bad2", &format!("file://{}", bad.display())).is_err());
}

// --- call: namespace parsing --------------------------------------------

#[test]
fn test_call_parses_namespace() {
    let mut registry = McpRegistry::default();
    registry.connect("docs", "mock://docs").unwrap();

    let out = registry.call("mcp__docs__search", &serde_json::json!({ "query": "cron" })).unwrap();
    assert_eq!(out, r#"docs.search called with {"query":"cron"}"#);

    let out = registry.call("mcp__docs__get_version", &serde_json::json!({})).unwrap();
    assert_eq!(out, "docs.get_version called with {}");

    // Unknown server.
    let err = registry.call("mcp__nope__search", &serde_json::json!({})).unwrap_err();
    assert!(err.to_string().contains("nope"));
    // Unknown tool.
    let err = registry.call("mcp__docs__nope", &serde_json::json!({})).unwrap_err();
    assert!(err.to_string().contains("docs.nope"));
    // Malformed namespaced names.
    for bad in ["docs.search", "mcp__docs", "mcp__docs__", "__docs__x", "plain", "mcp____tool"] {
        assert!(registry.call(bad, &serde_json::json!({})).is_err(), "{bad} must be rejected");
    }
}

// --- tool definitions ----------------------------------------------------

#[test]
fn test_tool_definitions_names_and_permissions() {
    let mut registry = McpRegistry::default();
    registry.connect("docs", "mock://docs").unwrap();
    registry.connect("deploy", "mock://deploy").unwrap();

    let defs = registry.tool_definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "mcp__deploy__status",
            "mcp__deploy__trigger",
            "mcp__docs__get_version",
            "mcp__docs__search"
        ]
    );

    let by_name = |name: &str| defs.iter().find(|d| d.function.name == name).unwrap();
    assert!(by_name("mcp__docs__search").function.description.ends_with("(readOnly)"));
    assert!(by_name("mcp__docs__get_version").function.description.ends_with("(readOnly)"));
    assert!(by_name("mcp__deploy__trigger").function.description.ends_with("(destructive)"));
    assert!(by_name("mcp__deploy__status").function.description.ends_with("(readOnly)"));
    assert!(by_name("mcp__docs__search").function.description.contains("Search documentation"));
    assert_eq!(by_name("mcp__docs__search").function.parameters["required"][0], "query");
}

#[test]
fn test_tool_without_permission_has_no_annotation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tools.json");
    std::fs::write(&path, r#"{"tools": [{"name": "hello", "description": "Say hello"}]}"#).unwrap();
    let mut registry = McpRegistry::default();
    registry.connect("greet", &format!("file://{}", path.display())).unwrap();

    let defs = registry.tool_definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].function.name, "mcp__greet__hello");
    assert_eq!(defs[0].function.description, "Say hello", "no annotation without permissions");
    assert_eq!(
        defs[0].function.parameters,
        serde_json::json!({ "type": "object", "properties": {} }),
        "missing parameters default to an empty schema"
    );
}

// --- connect_mcp tool ----------------------------------------------------

#[test]
fn test_connect_mcp_tool() {
    let tool = ConnectMcpTool { registry: Arc::new(Mutex::new(McpRegistry::default())) };

    let result =
        tool.execute(&serde_json::json!({ "name": "docs", "url": "mock://docs" })).unwrap();
    assert!(result.success, "output: {}", result.output);
    assert!(result.output.contains("Connected to MCP server 'docs'"));
    assert!(result.output.contains("2 tools"));

    // Invalid name -> tool error result.
    let result =
        tool.execute(&serde_json::json!({ "name": "Bad Name", "url": "mock://docs" })).unwrap();
    assert!(!result.success);

    // Duplicate connect -> tool error result.
    let result =
        tool.execute(&serde_json::json!({ "name": "docs", "url": "mock://docs" })).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("already connected"));

    // Missing required arguments are hard errors.
    let err = tool.execute(&serde_json::json!({ "name": "docs" })).unwrap_err();
    assert!(err.to_string().contains("url"));
}
