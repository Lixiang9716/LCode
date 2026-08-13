//! Unit tests for skill loading (s05) — `lcode::agent::skill`.
//!
//! Exercises SKILL.md frontmatter parsing (with directory-name fallback),
//! recursive registry discovery, the two-layer `descriptions`/`content`
//! accessors, and the `load_skill` tool.

use lcode::agent::{LoadSkillTool, SkillRegistry};
use lcode::tools::Tool;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Write a skill directory containing a `SKILL.md` with frontmatter.
fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    let text = format!("---\nname: {}\ndescription: {}\n---\n\n{}", name, description, body);
    std::fs::write(skill_dir.join("SKILL.md"), text).unwrap();
}

fn load(dir: &Path) -> SkillRegistry {
    let mut registry = SkillRegistry::default();
    registry.load_from(dir).unwrap();
    registry
}

// --- Registry discovery ---

#[test]
fn test_load_from_discovers_skills() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "Process PDF files", "Step 1: parse.");
    write_skill(tmp.path(), "code-review", "Review code for bugs", "# Code Review");

    let registry = load(tmp.path());

    assert!(!registry.is_empty());
    let descriptions = registry.descriptions();
    assert!(descriptions.contains("- pdf: Process PDF files"));
    assert!(descriptions.contains("- code-review: Review code for bugs"));
    // One line per skill, sorted.
    assert_eq!(descriptions.lines().count(), 2);
    assert!(descriptions.lines().next().unwrap().starts_with("- code-review"));
}

#[test]
fn test_load_from_scans_nested_directories() {
    let tmp = TempDir::new().unwrap();
    write_skill(&tmp.path().join("nested/deep"), "nested-skill", "Found deep", "body");

    let registry = load(tmp.path());

    assert!(registry.descriptions().contains("- nested-skill: Found deep"));
}

#[test]
fn test_load_from_missing_dir_leaves_empty_registry() {
    let tmp = TempDir::new().unwrap();
    let registry = load(&tmp.path().join("does-not-exist"));

    assert!(registry.is_empty());
    assert_eq!(registry.descriptions(), "(no skills available)");
}

// --- Frontmatter parsing ---

#[test]
fn test_frontmatter_falls_back_to_directory_name() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("no-frontmatter");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "# Just a body, no frontmatter").unwrap();

    let registry = load(tmp.path());

    assert!(!registry.is_empty());
    assert!(registry.descriptions().contains("- no-frontmatter: "));
}

#[test]
fn test_malformed_frontmatter_falls_back_to_directory_name() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("bad-yaml");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: [unclosed\n---\nbody").unwrap();

    let registry = load(tmp.path());

    assert!(registry.descriptions().contains("- bad-yaml: "));
}

#[test]
fn test_multiline_descriptions_are_flattened() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent-builder");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: agent-builder\ndescription: |\n  Design agents.\n  Use when asked.\n---\nbody",
    )
    .unwrap();

    let registry = load(tmp.path());

    let descriptions = registry.descriptions();
    assert!(descriptions.contains("- agent-builder: Design agents. Use when asked."));
    assert_eq!(descriptions.lines().count(), 1);
}

// --- Layer 2: content ---

#[test]
fn test_content_wraps_body_in_skill_tags() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "d", "Step 1: parse.\n\nStep 2: extract.");

    let registry = load(tmp.path());

    let content = registry.content("pdf");
    // The body excludes the frontmatter and is wrapped in <skill> tags.
    assert_eq!(content, "<skill name=\"pdf\">\nStep 1: parse.\n\nStep 2: extract.\n</skill>");
    assert!(!content.contains("description:"));
}

#[test]
fn test_content_unknown_skill_lists_available() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "d", "body");
    write_skill(tmp.path(), "git", "d", "body");

    let registry = load(tmp.path());

    let out = registry.content("nope");
    assert!(out.contains("Error: Unknown skill 'nope'"));
    assert!(out.contains("Available: git, pdf"));
}

#[test]
fn test_content_unknown_skill_with_empty_registry() {
    let registry = SkillRegistry::default();

    assert!(registry.content("nope").contains("Available: (none)"));
}

// --- The load_skill tool ---

#[test]
fn test_load_skill_tool_metadata_and_parameters() {
    let tool =
        LoadSkillTool { registry: Arc::new(Mutex::new(SkillRegistry::default())), events: None };

    assert_eq!(tool.name(), "load_skill");
    assert!(tool.description().contains("skill"));

    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert_eq!(params["required"][0], "name");
    assert_eq!(params["properties"]["name"]["type"], "string");
}

#[test]
fn test_load_skill_tool_executes() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "Process PDF files", "Step 1: parse.");
    let tool = LoadSkillTool { registry: Arc::new(Mutex::new(load(tmp.path()))), events: None };

    let result = tool.execute(&serde_json::json!({ "name": "pdf" })).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "<skill name=\"pdf\">\nStep 1: parse.\n</skill>");
}

#[test]
fn test_load_skill_tool_unknown_skill_returns_error_text() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "d", "body");
    let tool = LoadSkillTool { registry: Arc::new(Mutex::new(load(tmp.path()))), events: None };

    let result = tool.execute(&serde_json::json!({ "name": "nope" })).unwrap();
    assert!(result.output.contains("Error: Unknown skill 'nope'"));
    assert!(result.output.contains("Available: pdf"));
}

#[test]
fn test_load_skill_tool_with_empty_registry_says_no_skills_loaded() {
    let tool =
        LoadSkillTool { registry: Arc::new(Mutex::new(SkillRegistry::default())), events: None };

    let result = tool.execute(&serde_json::json!({ "name": "pdf" })).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "no skills loaded");
}

#[test]
fn test_load_skill_tool_requires_name_argument() {
    let tool =
        LoadSkillTool { registry: Arc::new(Mutex::new(SkillRegistry::default())), events: None };

    let result = tool.execute(&serde_json::json!({})).unwrap();
    assert!(!result.success);
    assert!(result.output.contains("'name'"));

    // Whitespace-only names are rejected too.
    let result = tool.execute(&serde_json::json!({ "name": "  " })).unwrap();
    assert!(!result.success);
}
