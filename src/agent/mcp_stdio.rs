//! MCP stdio transport (learn-claude-code s19, real protocol).
//!
//! `command:` URLs spawn a local MCP server subprocess and speak JSON-RPC
//! 2.0 over its stdin/stdout using the MCP stdio framing (LSP-style
//! `Content-Length` headers), so real MCP servers work end to end:
//!
//! ```text
//! connect_mcp("fs", "command:npx -y @modelcontextprotocol/server-filesystem /tmp")
//! ```
//!
//! Handshake per the MCP spec: `initialize` → `notifications/initialized`
//! → `tools/list` → `tools/call` per invocation. All I/O is synchronous
//! because tools run on synchronous threads (`Tool::execute`); the
//! connection is kept alive for the process lifetime and serialized
//! behind a mutex.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Protocol version advertised during `initialize` (2024-11-05 — the
/// current stable MCP revision).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// A running MCP stdio subprocess speaking JSON-RPC over
/// Content-Length-framed stdio.
#[derive(Debug)]
pub(crate) struct StdioConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioConnection {
    /// Spawn `command` (program + arguments) with piped stdin/stdout.
    /// Fails if the binary cannot be spawned.
    pub(crate) fn spawn(command: &str) -> anyhow::Result<Self> {
        let mut parts = split_command(command)?;
        let program = parts.remove(0);
        let mut child = Command::new(&program)
            .args(&parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn MCP stdio server '{program}': {e}"))?;
        Ok(Self {
            stdin: child.stdin.take().expect("stdin is piped"),
            stdout: BufReader::new(child.stdout.take().expect("stdout is piped")),
            child,
            next_id: 1,
        })
    }

    /// Send a JSON-RPC request and wait for the matching response.
    ///
    /// Responses addressed to other ids (stray notifications or late
    /// answers) are skipped. A server that never answers hangs the call;
    /// the test harness bounds this via process-level timeouts.
    pub(crate) fn request(&mut self, method: &str, params: &Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let body = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        write_frame(&mut self.stdin, &body)?;
        loop {
            let frame = read_frame(&mut self.stdout)?;
            let msg: Value = serde_json::from_slice(&frame)?;
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                anyhow::bail!("MCP {method} error: {err}");
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub(crate) fn notify(&mut self, method: &str, params: &Value) -> anyhow::Result<()> {
        let body = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))?;
        write_frame(&mut self.stdin, &body)
    }

    /// MCP handshake: `initialize` → `notifications/initialized` →
    /// `tools/list`. Returns the discovered tools.
    pub(crate) fn connect_and_list_tools(&mut self) -> anyhow::Result<Vec<McpToolInfo>> {
        // The result carries the negotiated protocol version and server
        // capabilities; not needed by the client beyond compatibility.
        let _negotiated = self.request(
            "initialize",
            &serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "lcode", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        self.notify("notifications/initialized", &serde_json::json!({}))?;
        let result = self.request("tools/list", &serde_json::json!({}))?;
        parse_tools_list(&result)
    }

    /// Invoke `tools/call`; concatenates the text content parts into the
    /// returned string. An `isError` result surfaces as an error.
    pub(crate) fn call_tool(&mut self, name: &str, args: &Value) -> anyhow::Result<String> {
        let result =
            self.request("tools/call", &serde_json::json!({ "name": name, "arguments": args }))?;
        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            anyhow::bail!("MCP tool {name} failed: {}", text_content(&result));
        }
        Ok(text_content(&result))
    }
}

impl Drop for StdioConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One tool from a `tools/list` result.
#[derive(Debug, Clone)]
pub(crate) struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// `"readOnly"` / `"destructive"` from the annotation hints, or "".
    pub permissions: String,
}

/// Parse the `result` of a `tools/list` response into tool infos.
fn parse_tools_list(result: &Value) -> anyhow::Result<Vec<McpToolInfo>> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP tools/list: missing 'tools' array"))?;
    let mut infos = Vec::with_capacity(tools.len());
    for tool in tools {
        let parameters = tool
            .get("inputSchema")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
        let permissions = if tool.get("destructiveHint").and_then(Value::as_bool).unwrap_or(false) {
            "destructive"
        } else if tool.get("readOnlyHint").and_then(Value::as_bool).unwrap_or(false) {
            "readOnly"
        } else {
            ""
        };
        infos.push(McpToolInfo {
            name: tool["name"].as_str().unwrap_or_default().to_string(),
            description: tool["description"].as_str().unwrap_or_default().to_string(),
            parameters,
            permissions: permissions.to_string(),
        });
    }
    Ok(infos)
}

/// Concatenate the `text` parts of a `tools/call` result's `content`.
fn text_content(result: &Value) -> String {
    let Some(parts) = result.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let mut text = String::new();
    for part in parts {
        if part["type"] != "text" {
            continue;
        }
        if let Some(part_text) = part["text"].as_str() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part_text);
        }
    }
    text
}

/// Write one Content-Length-framed message to the server's stdin.
fn write_frame(stdin: &mut ChildStdin, body: &[u8]) -> anyhow::Result<()> {
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(body)?;
    stdin.flush()?;
    Ok(())
}

/// Read one Content-Length-framed message from the server's stdout.
fn read_frame(reader: &mut impl BufRead) -> anyhow::Result<Vec<u8>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            anyhow::bail!("MCP stdio: EOF while reading frame headers");
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse().map_err(|_| {
                anyhow::anyhow!("MCP stdio: invalid Content-Length header: {rest}")
            })?);
        }
    }
    let len = content_length
        .ok_or_else(|| anyhow::anyhow!("MCP stdio: frame is missing the Content-Length header"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// Parse a complete Content-Length frame from the front of `buffer`,
/// returning `(consumed_bytes, parsed_body)` — or `None` when the buffer
/// does not yet hold a complete frame. Pure function, unit-tested without
/// a subprocess.
#[doc(hidden)]
pub fn parse_frame(buffer: &[u8]) -> Option<(usize, Value)> {
    let header_end = buffer.windows(b"\r\n\r\n".len()).position(|w| w == b"\r\n\r\n")?;
    let headers = std::str::from_utf8(&buffer[..header_end]).ok()?;
    let content_length = headers.lines().find_map(|line| {
        line.strip_prefix("Content-Length:").and_then(|rest| rest.trim().parse::<usize>().ok())
    })?;
    let frame_len = header_end + 4 + content_length;
    let body = buffer.get(header_end + 4..frame_len)?;
    let value = serde_json::from_slice(body).ok()?;
    Some((frame_len, value))
}

/// Split a command line into program + arguments.
///
/// Whitespace separates tokens; double quotes group tokens so paths with
/// spaces work (`command:my mcp server --path` passes `server --path` as
/// one argument when quoted). Unquoted quotes are an error.
#[doc(hidden)]
pub fn split_command(command: &str) -> anyhow::Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in command.trim().chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if in_quotes {
        anyhow::bail!("unterminated quote in MCP command: {command}");
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        anyhow::bail!("empty MCP command");
    }
    Ok(parts)
}
