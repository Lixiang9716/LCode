//! Tool system — the agent's interface to the outside world.
//!
//! Tools give the agent the ability to:
//! - Read and write files (write doubles as the in-place editor)
//! - Search the codebase
//! - Execute shell commands
//!
//! Each tool implements a standard interface for discovery (definition)
//! and execution.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::fmt;

pub mod fetch;
pub mod file;
pub mod search;
pub mod shell;

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Whether the tool executed successfully
    pub success: bool,
    /// Human-readable output
    pub output: String,
    /// Optional structured data
    pub data: Option<serde_json::Value>,
}

impl ToolResult {
    /// Create a successful result.
    pub fn ok(output: impl Into<String>) -> Self {
        Self { success: true, output: output.into(), data: None }
    }

    /// Create a successful result with structured data.
    pub fn ok_with_data(output: impl Into<String>, data: serde_json::Value) -> Self {
        Self { success: true, output: output.into(), data: Some(data) }
    }

    /// Create an error result.
    pub fn err(output: impl Into<String>) -> Self {
        Self { success: false, output: output.into(), data: None }
    }
}

impl fmt::Display for ToolResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.success {
            write!(f, "{}", self.output)
        } else {
            write!(f, "Error: {}", self.output)
        }
    }
}

/// A tool that can be called by the agent.
pub trait Tool: Send + Sync {
    /// Get the tool's name (used in tool calls).
    fn name(&self) -> &str;

    /// Get the tool's description (shown to the LLM).
    fn description(&self) -> &str;

    /// Get the JSON schema for the tool's parameters.
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with the given arguments.
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult>;
}

/// Registry of all available tools.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create a new tool registry with all built-in tools.
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let mut registry = Self { tools: Vec::new() };

        // Register built-in tools
        registry.register(Box::new(file::ReadFileTool::new(config)?));
        registry.register(Box::new(file::WriteFileTool::new(config)?));
        registry.register(Box::new(search::GrepTool::new(config)?));
        registry.register(Box::new(search::GlobTool::new(config)?));
        registry.register(Box::new(shell::ShellTool::new(config)?));

        Ok(registry)
    }

    /// Register a new tool.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        tracing::debug!(name = tool.name(), "Registered tool");
        self.tools.push(tool);
    }

    /// Get tool definitions for sending to the LLM.
    pub fn definitions(&self) -> Vec<crate::llm::ToolDefinition> {
        self.tools
            .iter()
            .map(|t| crate::llm::ToolDefinition {
                tool_type: "function".to_string(),
                function: crate::llm::FunctionDefinition {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.parameters(),
                },
                server: None,
            })
            .collect()
    }

    /// Execute a tool by name with the given arguments.
    pub fn execute(&self, name: &str, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", name))?;

        tracing::info!(tool = name, "Executing tool");
        tool.execute(args)
    }

    /// List all registered tool names.
    pub fn list_tools(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}
