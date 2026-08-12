//! Memory tools: `write_memory`, `extract_memories`, `list_memories`,
//! `read_memory` (learn-claude-code s09).
//!
//! LLM-backed tools follow the synchronous-tool-over-async pattern:
//! `tokio::task::block_in_place` + `Handle::block_on` (see
//! `subagent.rs`).

use super::{format_memory_file, json_tags, MemoryStore};
use crate::tools::{Tool, ToolResult};
use std::path::Path;
use std::sync::Arc;

/// Tool: `write_memory` — store a fact/preference in `.memory/`.
pub struct WriteMemoryTool {
    pub store: Arc<MemoryStore>,
}

impl Tool for WriteMemoryTool {
    fn name(&self) -> &str {
        "write_memory"
    }

    fn description(&self) -> &str {
        "Store a fact, preference, or project detail in cross-session \
         memory (.memory/). Contents persist between sessions; use for \
         things the user will want remembered later."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Short kebab-case identifier, e.g. 'user-prefers-tabs'"
                },
                "description": {
                    "type": "string",
                    "description": "One-line summary shown in the memory index"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional tags"
                },
                "content": {
                    "type": "string",
                    "description": "Full markdown body of the memory"
                }
            },
            "required": ["name", "content"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let content = args["content"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let (Some(name), Some(content)) = (name, content) else {
            return Ok(ToolResult::err("write_memory requires 'name' and 'content' arguments"));
        };
        let description = args["description"].as_str().unwrap_or("").trim();
        let tags = json_tags(args.get("tags"));
        let body = format_memory_file(name, description, &tags, content);
        self.store.write(&format!("{name}.md"), &body)?;
        Ok(ToolResult::ok(format!("wrote memory '{name}' to .memory/")))
    }
}

/// Tool: `extract_memories` — mine a conversation excerpt for facts.
pub struct ExtractMemoriesTool {
    pub store: Arc<MemoryStore>,
    pub provider: Arc<dyn crate::llm::LlmProvider>,
}

impl Tool for ExtractMemoriesTool {
    fn name(&self) -> &str {
        "extract_memories"
    }

    fn description(&self) -> &str {
        "Extract user preferences, constraints, or project facts from a \
         conversation excerpt and store them in cross-session memory."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "conversation": {
                    "type": "string",
                    "description": "Recent conversation text to mine for memories"
                }
            },
            "required": ["conversation"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let conversation = args["conversation"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let Some(conversation) = conversation else {
            return Ok(ToolResult::err("extract_memories requires a 'conversation' argument"));
        };
        // The `Tool` trait is synchronous but extraction is async; block
        // on the current runtime handle (see `subagent.rs`).
        tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("extract_memories requires a tokio runtime context"))?;
        let store = self.store.clone();
        let provider = self.provider.clone();
        let count = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current()
                .block_on(store.extract(conversation, provider.as_ref()))
        })
        .map_err(|e| anyhow::anyhow!("memory extraction failed: {e}"))?;
        Ok(ToolResult::ok(format!("extracted {count} new memories")))
    }
}

/// Tool: `list_memories` — show the memory index.
pub struct ListMemoriesTool {
    pub store: Arc<MemoryStore>,
}

impl Tool for ListMemoriesTool {
    fn name(&self) -> &str {
        "list_memories"
    }

    fn description(&self) -> &str {
        "List stored memories (name + description) from the memory index."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let index = self.store.index();
        if index.is_empty() {
            Ok(ToolResult::ok("no memories stored"))
        } else {
            Ok(ToolResult::ok(index))
        }
    }
}

/// Tool: `read_memory` — fetch a memory's full content.
pub struct ReadMemoryTool {
    pub store: Arc<MemoryStore>,
}

impl Tool for ReadMemoryTool {
    fn name(&self) -> &str {
        "read_memory"
    }

    fn description(&self) -> &str {
        "Read the full content of a stored memory by name or filename."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Memory name or filename ('.md' optional)"
                }
            },
            "required": ["name"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let Some(name) = name else {
            return Ok(ToolResult::err("read_memory requires a 'name' argument"));
        };
        match self.store.read(name) {
            Some(content) => Ok(ToolResult::ok(content)),
            None => Ok(ToolResult::err(format!("no memory found for '{name}'"))),
        }
    }
}

/// Register this module's tools with the registry.
///
/// The provider backs `extract_memories` (and, once the executor wiring
/// lands, session-boundary extraction/consolidation).
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    workspace: &Path,
    provider: Arc<dyn crate::llm::LlmProvider>,
) -> anyhow::Result<()> {
    let store = Arc::new(MemoryStore::new(workspace)?);
    registry.register(Box::new(WriteMemoryTool { store: store.clone() }));
    registry.register(Box::new(ExtractMemoriesTool { store: store.clone(), provider }));
    registry.register(Box::new(ListMemoriesTool { store: store.clone() }));
    registry.register(Box::new(ReadMemoryTool { store }));
    Ok(())
}
