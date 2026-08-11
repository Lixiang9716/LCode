//! Skill loading (learn-claude-code s05).
//!
//! Two-layer knowledge injection: layer 1 lists skill names + one-line
//! descriptions in the system prompt (cheap); layer 2 loads the full
//! SKILL.md body into the context only when the model calls
//! `load_skill` (expensive, on demand).

use crate::tools::{Tool, ToolResult};
use std::path::{Path, PathBuf};

/// A discovered skill (a directory containing SKILL.md).
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Discovers skills by scanning a directory for SKILL.md files and
/// parsing their YAML frontmatter.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Scan `skills_dir` recursively for `SKILL.md` files.
    pub fn load_from(&mut self, skills_dir: &Path) -> anyhow::Result<()> {
        // TODO(s05): rglob SKILL.md, parse `---` frontmatter with
        // serde_yaml, fall back to directory name as skill name.
        let _ = skills_dir;
        Ok(())
    }

    /// Layer 1: one-line descriptions for the system prompt.
    pub fn descriptions(&self) -> String {
        // TODO(s05): "- name: description" lines.
        self.skills.iter().map(|s| format!("- {}: {}", s.name, s.description)).collect::<Vec<_>>().join("\n")
    }

    /// Layer 2: full SKILL.md body wrapped in `<skill>` tags.
    pub fn content(&self, name: &str) -> String {
        // TODO(s05): return `<skill name="...">body</skill>`; unknown
        // skills list the available ones in the error.
        format!("<skill name=\"{}\">(not loaded)</skill>", name)
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Tool: `load_skill` — pulls the full skill body into the context.
pub struct LoadSkillTool {
    pub registry: std::sync::Arc<std::sync::Mutex<SkillRegistry>>,
}

impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn description(&self) -> &str {
        "Load the full content of a skill by name. Use when a task \
         requires domain knowledge listed in the system prompt."
    }

    fn parameters(&self) -> serde_json::Value {
        // TODO(s05): { name: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        // TODO(s05): get name, return registry.content(name).
        Ok(ToolResult::err("load_skill not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, skills_dir: PathBuf) {
    let mut r = SkillRegistry::default();
    let _ = r.load_from(&skills_dir);
    registry.register(Box::new(LoadSkillTool {
        registry: std::sync::Arc::new(std::sync::Mutex::new(r)),
    }));
}
