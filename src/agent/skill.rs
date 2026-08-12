//! Skill loading (learn-claude-code s05).
//!
//! Two-layer knowledge injection: layer 1 lists skill names + one-line
//! descriptions in the system prompt (cheap); layer 2 loads the full
//! SKILL.md body into the context only when the model calls
//! `load_skill` (expensive, on demand).

use crate::tools::{Tool, ToolResult};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A discovered skill (a directory containing SKILL.md).
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

impl Skill {
    /// Parse a `SKILL.md` file: YAML frontmatter (`name`/`description`)
    /// plus the body. Falls back to the parent directory name when the
    /// frontmatter is missing or malformed.
    fn from_file(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let (meta, _body) = parse_frontmatter(&text);
        let fallback = path.parent()?.file_name()?.to_string_lossy().into_owned();
        let name = meta
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(fallback);
        let description = meta
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        Some(Self { name, description, path: path.to_path_buf() })
    }
}

/// Split `---`-delimited YAML frontmatter from the body.
///
/// Returns the metadata map and the body; when the document has no
/// (parseable) frontmatter both degenerate to `Null` / the full text.
fn parse_frontmatter(text: &str) -> (serde_yaml::Value, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (serde_yaml::Value::Null, text);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (serde_yaml::Value::Null, text);
    };
    let yaml = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    let meta = serde_yaml::from_str(yaml).unwrap_or(serde_yaml::Value::Null);
    (meta, body)
}

/// Collapse whitespace (multi-line YAML descriptions) into one line.
fn flatten(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Discovers skills by scanning a directory for SKILL.md files and
/// parsing their YAML frontmatter.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Scan `skills_dir` recursively for `SKILL.md` files.
    ///
    /// Unreadable or malformed skill files are skipped; a missing
    /// directory leaves the registry empty (no error).
    pub fn load_from(&mut self, skills_dir: &Path) -> anyhow::Result<()> {
        if !skills_dir.is_dir() {
            return Ok(());
        }
        let mut skills = Vec::new();
        for entry in walkdir::WalkDir::new(skills_dir) {
            let entry = entry?;
            if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
                if let Some(skill) = Skill::from_file(entry.path()) {
                    skills.push(skill);
                }
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        self.skills = skills;
        Ok(())
    }

    /// Layer 1: one-line descriptions for the system prompt.
    pub fn descriptions(&self) -> String {
        if self.skills.is_empty() {
            return "(no skills available)".to_string();
        }
        self.skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, flatten(&s.description)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Layer 2: full SKILL.md body wrapped in `<skill>` tags.
    ///
    /// Unknown skills produce an error listing the available ones.
    pub fn content(&self, name: &str) -> String {
        match self.skills.iter().find(|s| s.name == name) {
            Some(skill) => {
                let text = std::fs::read_to_string(&skill.path).unwrap_or_default();
                let (_meta, body) = parse_frontmatter(&text);
                format!("<skill name=\"{}\">\n{}\n</skill>", name, body.trim())
            }
            None => {
                let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                let list =
                    if available.is_empty() { "(none)".to_string() } else { available.join(", ") };
                format!("Error: Unknown skill '{}'. Available: {}", name, list)
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Layer-1 injection (s07 / G9): append the registry's one-line
/// descriptions to a base system prompt as a `## Skills` section.
///
/// The combined prompt is what the session hands to the executor, so the
/// skill catalog is visible to the model from the first turn (cheap);
/// `load_skill` still pulls full SKILL.md bodies on demand (expensive).
/// Returns the base unchanged when no skills are available.
pub fn with_layer1(base: &str, registry: &SkillRegistry) -> String {
    let descriptions = registry.descriptions();
    if descriptions == "(no skills available)" {
        return base.to_string();
    }
    format!("{base}\n\n## Skills\n{descriptions}")
}

/// Tool: `load_skill` — pulls the full skill body into the context.
pub struct LoadSkillTool {
    pub registry: Arc<Mutex<SkillRegistry>>,
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
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name to load" }
            },
            "required": ["name"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name =
            args.get("name").and_then(|v| v.as_str()).map(str::trim).filter(|n| !n.is_empty());
        let Some(name) = name else {
            return Ok(ToolResult::err("load_skill requires a 'name' argument"));
        };
        let registry = self.registry.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if registry.is_empty() {
            return Ok(ToolResult::ok("no skills loaded"));
        }
        Ok(ToolResult::ok(registry.content(name)))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry, skills_dir: PathBuf) {
    let mut r = SkillRegistry::default();
    let _ = r.load_from(&skills_dir);
    registry.register(Box::new(LoadSkillTool { registry: Arc::new(Mutex::new(r)) }));
}
