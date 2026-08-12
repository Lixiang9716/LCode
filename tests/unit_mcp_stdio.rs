//! Unit tests for the MCP stdio transport (G13): the Content-Length
//! frame parser, the command-line splitter, and a real end-to-end run
//! against a POSIX `sh` script that speaks minimal JSON-RPC 2.0 over
//! stdio (initialize → tools/list → tools/call).

use lcode::agent::{parse_frame, split_command, McpRegistry};

// ---------------------------------------------------------------------------
// parse_frame (pure buffer → frame parsing)
// ---------------------------------------------------------------------------

#[test]
fn parse_frame_extracts_complete_frame() {
    let body = br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
    let mut buffer = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    buffer.extend_from_slice(body);

    let (consumed, value) = parse_frame(&buffer).expect("complete frame");
    assert_eq!(consumed, buffer.len());
    assert_eq!(value["result"]["tools"].as_array().unwrap().len(), 0);
}

#[test]
fn parse_frame_consumes_only_one_frame() {
    let body = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    let mut buffer = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    buffer.extend_from_slice(body);
    // A second frame follows in the same buffer.
    let body2 = br#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
    buffer.extend_from_slice(&format!("Content-Length: {}\r\n\r\n", body2.len()).into_bytes());
    buffer.extend_from_slice(body2);

    let (consumed, value) = parse_frame(&buffer).expect("first frame");
    let expected = format!("Content-Length: {}\r\n\r\n", body.len()).len() + body.len();
    assert_eq!(consumed, expected, "consumed must stop at the first frame end");
    assert_eq!(value["id"], 1);
    // The remainder is the second frame.
    assert!(parse_frame(&buffer[consumed..]).is_some());
}

#[test]
fn parse_frame_needs_more_data() {
    let body = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    let mut buffer = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    buffer.extend_from_slice(&body[..body.len() - 3]); // truncated body
    assert_eq!(parse_frame(&buffer), None, "truncated frame must not parse");
}

#[test]
fn parse_frame_requires_content_length_header() {
    let buffer = b"\r\n\r\n{\"jsonrpc\":\"2.0\"}";
    assert_eq!(parse_frame(buffer), None, "missing Content-Length must not parse");
}

// ---------------------------------------------------------------------------
// split_command (program + args)
// ---------------------------------------------------------------------------

#[test]
fn split_command_splits_whitespace() {
    assert_eq!(
        split_command("npx -y @modelcontextprotocol/server-filesystem /tmp").unwrap(),
        vec!["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    );
    // Collapsed whitespace and leading/trailing spaces.
    assert_eq!(split_command("  sh   -c   'x' ").unwrap(), vec!["sh", "-c", "'x'"]);
}

#[test]
fn split_command_keeps_quoted_tokens() {
    assert_eq!(
        split_command(r#"my server --path "/tmp/my dir""#).unwrap(),
        vec!["my", "server", "--path", "/tmp/my dir"]
    );
}

#[test]
fn split_command_rejects_unterminated_quote_and_empty() {
    assert!(split_command(r#"sh -c "unclosed"#).is_err());
    assert!(split_command("   ").is_err());
    assert!(split_command("").is_err());
}

// ---------------------------------------------------------------------------
// End-to-end: a real stdio subprocess speaking minimal MCP JSON-RPC
// ---------------------------------------------------------------------------

/// A tiny MCP-compatible stdio server written in POSIX sh: reads
/// Content-Length-framed requests (headers by line, body by exact byte
/// count via `head -c`), answers `initialize`, `tools/list` and
/// `tools/call` with fixed results, and stays silent on
/// `notifications/initialized`.
const TEST_SERVER_SH: &str = r#"#!/bin/sh
# Minimal JSON-RPC 2.0 server over stdio for the G13 stdio tests.
send_response() {
  id="$1"
  result="$2"
  payload="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$result}"
  len=$(printf '%s' "$payload" | wc -c)
  printf 'Content-Length: %s\r\n\r\n%s' "$len" "$payload"
}
handle_request() {
  payload="$1"
  id=$(printf '%s' "$payload" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$payload" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  case "$method" in
    initialize)
      send_response "$id" '{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test-mcp","version":"1.0"}}' ;;
    tools/list)
      send_response "$id" '{"tools":[{"name":"hello","description":"Say hello","inputSchema":{"type":"object","properties":{"who":{"type":"string"}}},"readOnlyHint":true}]}' ;;
    tools/call)
      send_response "$id" '{"content":[{"type":"text","text":"hello from test mcp server"}],"isError":false}' ;;
    notifications/initialized)
      : ;;
  esac
}
read_frame() {
  content_length=""
  while IFS= read -r line || [ -n "$line" ]; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      "Content-Length:"*)
        content_length=$(printf '%s' "${line#Content-Length:}" | tr -d ' ') ;;
    esac
  done
  [ -n "$content_length" ] || return 1
  body=$(head -c "$content_length")
  handle_request "$body"
  return 0
}
while read_frame; do :; done
"#;

#[test]
fn test_connect_stdio_server_handshake_and_call() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("mcp_server.sh");
    std::fs::write(&script, TEST_SERVER_SH).unwrap();

    let mut registry = McpRegistry::default();
    let url = format!("command:sh {}", script.display());
    registry.connect("test", &url).expect("stdio handshake should succeed");

    // tools/list was discovered through the handshake.
    let listing = registry.list_servers();
    assert!(listing.contains("- test (") && listing.contains("1 tools"), "{listing}");

    let defs = registry.tool_definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].function.name, "mcp__test__hello");
    assert!(
        defs[0].function.description.ends_with("(readOnly)"),
        "readOnlyHint must annotate the definition: {}",
        defs[0].function.description
    );

    // tools/call round-trips through the same subprocess.
    let out = registry.call("mcp__test__hello", &serde_json::json!({ "who": "world" })).unwrap();
    assert_eq!(out, "hello from test mcp server");
}

#[test]
fn test_stdio_spawn_failure_is_a_connect_error() {
    let mut registry = McpRegistry::default();
    let err = registry.connect("ghost", "command:definitely-not-a-real-binary-xyz").unwrap_err();
    assert!(err.to_string().contains("Failed to spawn"), "error: {err}");
}

#[test]
fn test_mock_and_stdio_paths_coexist() {
    // mock:// stays on the simulated path, command: on the stdio path.
    let mut registry = McpRegistry::default();
    registry.connect("docs", "mock://docs").unwrap();
    let out = registry.call("mcp__docs__search", &serde_json::json!({ "query": "x" })).unwrap();
    assert!(out.contains("docs.search called with"), "mock path: {out}");
    assert_eq!(registry.list_servers().lines().count(), 1);
}
