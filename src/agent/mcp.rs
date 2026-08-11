//! MCP tools (learn-claude-code s19).
//!
//! The `connect_mcp` tool attaches external tool servers; tools are
//! namespaced `mcp__{server}__{tool}` to avoid collisions with built-ins,
//! and the tool pool is assembled per turn (built-ins + connected MCPs).

use crate::tools::{Tool, ToolResult};
use std::collections::HashMap;

/// An MCP server connection (simulated tools/list + tools/call).
#[derive(Debug, Clone)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    /// name -> (description, parameters)
    pub tools: HashMap<String, (String, serde_json::Value)>,
    /// Permissions annotation: readOnly or destructive.
    pub permissions: HashMap<String, String>,
}

/// Manages connected MCP servers and their tools.
#[derive(Debug, Default)]
pub struct McpRegistry {
    servers: HashMap<String, McpServer>,
}

impl McpRegistry {
    /// Connect to a server and load its tool list.
    pub fn connect(&mut self, name: &str, url: &str) -> anyhow::Result<()> {
        // TODO(s19): tools/list over HTTP (or a simulated local server);
        // store tools + permissions; name must match [a-z0-9_-].
        let _ = (name, url);
        Ok(())
    }

    /// List connected servers.
    pub fn list_servers(&self) -> String {
        self.servers
            .iter()
            .map(|(n, s)| format!("- {} ({}, {} tools)", n, s.url, s.tools.len()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Call a tool on a server: `mcp__{server}__{tool}`.
    pub fn call(&self, namespaced_name: &str, args: &serde_json::Value) -> anyhow::Result<String> {
        // TODO(s19): parse namespace, tools/call, return output string.
        let _ = (namespaced_name, args);
        anyhow::bail!("mcp.call not implemented yet")
    }

    /// Tools available for the model (namespaced names + descriptions).
    pub fn tool_definitions(&self) -> Vec<crate::llm::ToolDefinition> {
        // TODO(s19): for each server/tool build a ToolDefinition with
        // name `mcp__{server}__{tool}` and a permission annotation in
        // the description.
        Vec::new()
    }
}

/// Tool: `connect_mcp`.
pub struct ConnectMcpTool {
    pub registry: std::sync::Mutex<McpRegistry>,
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
        // TODO(s19): { name: string, url: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }
    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::err("connect_mcp not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry) {
    registry.register(Box::new(ConnectMcpTool {
        registry: std::sync::Mutex::new(McpRegistry::default()),
    }));
}
