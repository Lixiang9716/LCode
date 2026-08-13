//! MCP tools (learn-claude-code s19).
//!
//! The `connect_mcp` tool attaches external tool servers; tools are
//! namespaced `mcp__{server}__{tool}` to avoid collisions with built-ins,
//! and the tool pool is assembled per turn (built-ins + connected MCPs).
//!
//! Connection paths (G13):
//! - `mock://{name}` loads a built-in server (docs, deploy)
//! - `file://{path}` loads `{"tools": [...]}` from a local JSON file
//! - `command:{program} {args...}` spawns a real MCP server subprocess
//!   and speaks JSON-RPC 2.0 over its stdio (LSP-style Content-Length
//!   frames, the standard MCP stdio transport — see [`mcp_stdio`]).
//!   Example: `command:npx -y @modelcontextprotocol/server-filesystem /tmp`
//! - `http(s)://{url}` keeps the original custom REST protocol
//!   (`GET {url}/tools`, `POST {url}/call`) — a legacy LCode extension,
//!   not standard MCP HTTP transport; kept for compatibility and noted
//!   here so a future workstream can migrate it.

use crate::agent::mcp_stdio::StdioConnection;
use crate::tools::{Tool, ToolResult};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// An MCP server connection (simulated tools/list + tools/call).
#[derive(Debug, Clone)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    /// name -> (description, parameters)
    pub tools: HashMap<String, (String, serde_json::Value)>,
    /// Permissions annotation: readOnly or destructive.
    pub permissions: HashMap<String, String>,
    /// Live stdio subprocess for `command:` URLs (JSON-RPC over stdio);
    /// `None` for mock/file/http connections.
    pub(crate) stdio: Option<Arc<Mutex<StdioConnection>>>,
}

/// Manages connected MCP servers and their tools.
#[derive(Debug, Default)]
pub struct McpRegistry {
    servers: HashMap<String, McpServer>,
}

/// One tool of a `tools/list` response: `{"tools": [...]}`.
#[derive(Debug, Deserialize)]
struct ToolEntry {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: serde_json::Value,
    /// Optional `readOnly` / `destructive` permission annotation.
    #[serde(default)]
    permissions: String,
}

impl ToolEntry {
    fn new(
        name: &str,
        description: &str,
        parameters: serde_json::Value,
        permissions: &str,
    ) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
            permissions: permissions.to_string(),
        }
    }
}

/// A `tools/list` response body.
#[derive(Debug, Deserialize)]
struct ToolsFile {
    tools: Vec<ToolEntry>,
}

impl McpRegistry {
    /// Connect to a server and load its tool list. Names must match
    /// `[a-z0-9_-]` so they stay unambiguous inside `mcp__{server}__{tool}`.
    pub fn connect(&mut self, name: &str, url: &str) -> anyhow::Result<()> {
        validate_name(name)?;
        if self.servers.contains_key(name) {
            anyhow::bail!("MCP server '{}' already connected", name);
        }
        let mut server = McpServer {
            name: name.to_string(),
            url: url.to_string(),
            tools: HashMap::new(),
            permissions: HashMap::new(),
            stdio: None,
        };
        if let Some(command) = url.strip_prefix("command:") {
            // Real MCP stdio server (G13): spawn the subprocess and run
            // the JSON-RPC handshake (initialize → initialized →
            // tools/list). The connection stays open for the process
            // lifetime; every `tools/call` goes to the same subprocess.
            let mut conn = StdioConnection::spawn(command)?;
            for tool in conn.connect_and_list_tools()? {
                server.tools.insert(tool.name.clone(), (tool.description, tool.parameters));
                if !tool.permissions.is_empty() {
                    server.permissions.insert(tool.name, tool.permissions);
                }
            }
            server.stdio = Some(Arc::new(Mutex::new(conn)));
        } else {
            for entry in load_tools(url)? {
                let params = if entry.parameters.is_null() {
                    serde_json::json!({ "type": "object", "properties": {} })
                } else {
                    entry.parameters
                };
                server.tools.insert(entry.name.clone(), (entry.description, params));
                if !entry.permissions.is_empty() {
                    server.permissions.insert(entry.name, entry.permissions);
                }
            }
        }
        self.servers.insert(name.to_string(), server);
        Ok(())
    }

    /// List connected servers, one per line.
    pub fn list_servers(&self) -> String {
        let mut names: Vec<&String> = self.servers.keys().collect();
        names.sort();
        names
            .iter()
            .map(|n| {
                let server = &self.servers[*n];
                format!("- {} ({}, {} tools)", n, server.url, server.tools.len())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Call a tool on a server: `mcp__{server}__{tool}`.
    pub fn call(&self, namespaced_name: &str, args: &serde_json::Value) -> anyhow::Result<String> {
        let (server, tool) = parse_namespace(namespaced_name)?;
        let server_info = self
            .servers
            .get(server)
            .ok_or_else(|| anyhow::anyhow!("Unknown MCP server: {server}"))?;
        if !server_info.tools.contains_key(tool) {
            anyhow::bail!("Unknown MCP tool: {server}.{tool}");
        }
        if let Some(conn) = &server_info.stdio {
            // Real MCP stdio server: `tools/call` over the live JSON-RPC
            // connection (G13).
            let mut conn = conn.lock().unwrap();
            return conn.call_tool(tool, args);
        }
        if server_info.url.starts_with("http://") || server_info.url.starts_with("https://") {
            return http_call(&server_info.url, tool, args);
        }
        // Simulated execution.
        Ok(format!("{}.{} called with {}", server, tool, serde_json::to_string(args)?))
    }

    /// Tools available for the model (namespaced names + descriptions
    /// annotated with `(readOnly)` / `(destructive)` permissions).
    pub fn tool_definitions(&self) -> Vec<crate::llm::ToolDefinition> {
        let mut defs = Vec::new();
        let mut servers: Vec<&McpServer> = self.servers.values().collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        for server in servers {
            let mut names: Vec<&String> = server.tools.keys().collect();
            names.sort();
            for tool_name in names {
                defs.push(tool_definition(server, tool_name));
            }
        }
        defs
    }
}

// --- Helpers ------------------------------------------------------------

/// Build the model-facing definition for `mcp__{server}__{tool}`, with a
/// `(readOnly)` / `(destructive)` annotation from the permissions map.
fn tool_definition(server: &McpServer, tool_name: &str) -> crate::llm::ToolDefinition {
    let (description, parameters) = &server.tools[tool_name];
    let annotation = match server.permissions.get(tool_name).map(String::as_str) {
        Some("readOnly") => " (readOnly)",
        Some("destructive") => " (destructive)",
        _ => "",
    };
    crate::llm::ToolDefinition {
        tool_type: "function".to_string(),
        function: crate::llm::FunctionDefinition {
            name: format!("mcp__{}__{}", server.name, tool_name),
            description: format!("{description}{annotation}"),
            parameters: parameters.clone(),
        },
        server: None,
    }
}

/// Server names must be `[a-z0-9_-]` and non-empty.
fn validate_name(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !valid {
        anyhow::bail!("Invalid MCP server name '{}': use [a-z0-9_-]", name);
    }
    Ok(())
}

/// Split `mcp__{server}__{tool}` into (server, tool).
fn parse_namespace(namespaced_name: &str) -> anyhow::Result<(&str, &str)> {
    let rest = namespaced_name
        .strip_prefix("mcp__")
        .ok_or_else(|| anyhow::anyhow!("MCP tool names must be mcp__{{server}}__{{tool}}"))?;
    let (server, tool) = rest
        .split_once("__")
        .ok_or_else(|| anyhow::anyhow!("MCP tool names must be mcp__{{server}}__{{tool}}"))?;
    if server.is_empty() || tool.is_empty() {
        anyhow::bail!("MCP tool names must be mcp__{{server}}__{{tool}}");
    }
    Ok((server, tool))
}

/// Load the tool list for a URL: `mock://`, `file://`, or real HTTP.
fn load_tools(url: &str) -> anyhow::Result<Vec<ToolEntry>> {
    if let Some(name) = url.strip_prefix("mock://") {
        return mock_server(name);
    }
    if let Some(path) = url.strip_prefix("file://") {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read MCP tools file {path}: {e}"))?;
        return parse_tools(&text);
    }
    let text = http_get(format!("{}/tools", url.trim_end_matches('/')))?;
    parse_tools(&text)
}

/// Parse a `{"tools": [...]}` body into tool entries.
fn parse_tools(text: &str) -> anyhow::Result<Vec<ToolEntry>> {
    let file: ToolsFile =
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("invalid MCP tools list: {e}"))?;
    Ok(file.tools)
}

/// Built-in simulated servers (`mock://docs`, `mock://deploy`).
fn mock_server(name: &str) -> anyhow::Result<Vec<ToolEntry>> {
    let query = serde_json::json!({
        "type": "object",
        "properties": { "query": { "type": "string" } },
        "required": ["query"]
    });
    let service = serde_json::json!({
        "type": "object",
        "properties": { "service": { "type": "string" } },
        "required": ["service"]
    });
    let none = serde_json::json!({ "type": "object", "properties": {} });
    match name {
        "docs" => Ok(vec![
            ToolEntry::new("search", "Search documentation", query, "readOnly"),
            ToolEntry::new("get_version", "Get API version", none, "readOnly"),
        ]),
        "deploy" => Ok(vec![
            ToolEntry::new("trigger", "Trigger a deployment", service.clone(), "destructive"),
            ToolEntry::new("status", "Check deployment status", service, "readOnly"),
        ]),
        other => anyhow::bail!("Unknown mock MCP server '{}'", other),
    }
}

/// Synchronous HTTP GET. The `Tool` trait is synchronous and may run
/// inside a tokio runtime, so a dedicated OS thread runs a fresh
/// current-thread runtime (same pattern as `CompactTool`).
fn http_get(url: String) -> anyhow::Result<String> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build MCP http runtime");
        runtime.block_on(async move {
            let response = reqwest::Client::new()
                .get(&url)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await?;
            anyhow::ensure!(
                response.status().is_success(),
                "MCP tools/list failed: HTTP {}",
                response.status()
            );
            response.text().await.map_err(Into::into)
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("MCP http thread panicked"))?
}

/// Synchronous `POST {url}/call` for real HTTP servers.
fn http_call(url: &str, tool: &str, args: &serde_json::Value) -> anyhow::Result<String> {
    let url = format!("{}/call", url.trim_end_matches('/'));
    let body = serde_json::json!({ "name": tool, "args": args });
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build MCP http runtime");
        runtime.block_on(async move {
            let response = reqwest::Client::new()
                .post(&url)
                .timeout(std::time::Duration::from_secs(10))
                .json(&body)
                .send()
                .await?;
            anyhow::ensure!(
                response.status().is_success(),
                "MCP tools/call failed: HTTP {}",
                response.status()
            );
            response.text().await.map_err(Into::into)
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("MCP http thread panicked"))?
}

// --- Tools -------------------------------------------------------------

/// Tool: `connect_mcp`.
pub struct ConnectMcpTool {
    pub registry: Arc<Mutex<McpRegistry>>,
}

impl Tool for ConnectMcpTool {
    fn name(&self) -> &str {
        "connect_mcp"
    }
    fn description(&self) -> &str {
        "Connect an MCP (Model Context Protocol) tool server. Exposes \
         its tools as mcp__{server}__{tool}."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Server name ([a-z0-9_-])" },
                "url": { "type": "string", "description": "URL: mock://, file://, or http://" }
            },
            "required": ["name", "url"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("connect_mcp: missing required argument 'name'"))?;
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("connect_mcp: missing required argument 'url'"))?;
        let mut registry = self.registry.lock().unwrap();
        let tool_count = match registry.connect(name, url) {
            Ok(()) => registry.servers.get(name).map(|s| s.tools.len()).unwrap_or(0),
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        Ok(ToolResult::ok(format!("Connected to MCP server '{}' ({tool_count} tools)", name)))
    }
}

/// Register this module's tools with the registry.
///
/// The registry is created by the caller (session scope) so the executor
/// can share it for the dynamic per-turn tool pool.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    mcp_registry: std::sync::Arc<std::sync::Mutex<McpRegistry>>,
) {
    registry.register(Box::new(ConnectMcpTool { registry: mcp_registry }));
}
