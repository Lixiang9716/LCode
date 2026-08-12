//! System prompt assembly (learn-claude-code s10).
//!
//! The system prompt is assembled at session start from sections:
//! base identity, workspace context, available tools, skill layer-1
//! descriptions, and memory index availability. Reassembled when the
//! session state changes (skills loaded, MCP servers connected).

/// A named system-prompt section.
pub struct PromptSection {
    pub title: &'static str,
    pub content: String,
}

/// Assemble the session system prompt from the given sections.
pub fn assemble(sections: &[PromptSection]) -> String {
    let mut out = String::new();
    for section in sections {
        if section.content.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("## {}\n{}\n\n", section.title, section.content.trim()));
    }
    out.trim_end().to_string()
}

/// Build the standard session sections.
///
/// - `base`: the configured system prompt (identity)
/// - `workspace`: current directory
/// - `tools`: names of the registered tools
/// - `skills`: layer-1 skill descriptions (empty when none)
/// - `context`: e.g. memory index presence
pub fn session_sections(
    base: &str,
    workspace: &std::path::Path,
    tool_names: &[String],
    skill_descriptions: &str,
    memory_index: Option<&str>,
) -> Vec<PromptSection> {
    let mut sections = Vec::new();

    if !base.trim().is_empty() {
        sections.push(PromptSection { title: "Role", content: base.to_string() });
    }

    sections.push(PromptSection {
        title: "Workspace",
        content: format!("Working directory: {}", workspace.display()),
    });

    if !tool_names.is_empty() {
        sections.push(PromptSection {
            title: "Tools",
            content: format!("Available tools: {}", tool_names.join(", ")),
        });
    }

    if !skill_descriptions.trim().is_empty() {
        sections.push(PromptSection { title: "Skills", content: skill_descriptions.to_string() });
    }

    if let Some(index) = memory_index {
        sections.push(PromptSection { title: "Memory", content: index.to_string() });
    }

    sections
}
