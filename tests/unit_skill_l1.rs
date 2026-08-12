//! Unit tests for skill layer-1 injection (s07 / G9) — one-line skill
//! descriptions are folded into the base system prompt, so the
//! executor's assembled prompt carries the skill catalog from the first
//! turn (while `load_skill` still pulls full bodies on demand).

use lcode::agent::{with_layer1, ConversationMemory, SkillRegistry};
use lcode::config::Config;
use lcode::llm::Role;
use std::path::Path;
use tempfile::TempDir;

fn write_skill(dir: &Path, name: &str, description: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\nbody"),
    )
    .unwrap();
}

fn registry_in(dir: &Path) -> SkillRegistry {
    let mut registry = SkillRegistry::default();
    registry.load_from(dir).unwrap();
    registry
}

#[test]
fn test_with_layer1_appends_skill_catalog() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "Process PDF files");
    write_skill(tmp.path(), "code-review", "Review code for bugs");
    let base = "You are LCode.";

    let prompt = with_layer1(base, &registry_in(tmp.path()));

    assert!(prompt.starts_with("You are LCode."));
    assert!(prompt.contains("\n\n## Skills\n"));
    assert!(prompt.contains("- code-review: Review code for bugs"));
    assert!(prompt.contains("- pdf: Process PDF files"));
}

#[test]
fn test_with_layer1_unchanged_without_skills() {
    let registry = SkillRegistry::default();
    let base = "You are LCode.";

    assert_eq!(with_layer1(base, &registry), base);
}

#[test]
fn test_with_layer1_missing_dir_unchanged() {
    let tmp = TempDir::new().unwrap();
    let registry = registry_in(&tmp.path().join("does-not-exist"));

    assert_eq!(with_layer1("base", &registry), "base");
}

#[test]
fn test_layer1_prompt_flows_into_conversation_memory() {
    // End-to-end effect: the combined prompt is what the session hands
    // to the executor as its base system prompt.
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "pdf", "Process PDF files");
    let prompt = with_layer1("You are LCode.", &registry_in(tmp.path()));

    let memory = ConversationMemory::new(prompt);
    let first = &memory.get_context()[0];

    assert_eq!(first.role, Role::System);
    assert!(first.content.contains("## Skills"));
    assert!(first.content.contains("- pdf: Process PDF files"));
}

#[test]
fn test_agent_config_skills_dir_defaults_and_parses() {
    let default = Config::default();
    assert!(default.agent.skills_dir.is_none());

    let cfg: Config = toml::from_str("[agent]\nskills_dir = \"custom-skills\"\n").unwrap();
    assert_eq!(cfg.agent.skills_dir.as_deref(), Some(Path::new("custom-skills")));

    let cfg2: Config = toml::from_str("[agent]\nsystem_prompt = \"hi\"\n").unwrap();
    assert!(cfg2.agent.skills_dir.is_none());
}
