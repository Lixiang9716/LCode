//! Phase E tests: the built-in assets skill materializes into the
//! workspace skills dir when missing, never overwrites user edits, and
//! is discoverable through the normal SkillRegistry / load_skill flow.

use lcode::agent::ensure_assets_skill;
use lcode::agent::SkillRegistry;

#[test]
fn skill_materializes_when_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    ensure_assets_skill(&skills_dir);

    let path = skills_dir.join("assets").join("SKILL.md");
    assert!(path.is_file(), "built-in skill must be written");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("kind"), "sidecar conventions present");
    assert!(text.contains("sha256sum"), "integrity workflow present");
}

#[test]
fn skill_never_overwrites_user_edits() {
    let tmp = tempfile::TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    let target = skills_dir.join("assets").join("SKILL.md");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "---\nname: assets\ndescription: mine\n---\nuser version\n").unwrap();

    ensure_assets_skill(&skills_dir);

    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("user version"), "user edits survive: {text}");
    assert!(!text.contains("sidecar"), "built-in text must not clobber the user file");
}

#[test]
fn registry_discovers_the_builtin_skill() {
    let tmp = tempfile::TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    ensure_assets_skill(&skills_dir);

    let mut registry = SkillRegistry::default();
    registry.load_from(&skills_dir).expect("loads");

    // Layer 1: the one-line description is injected into the prompt.
    let descriptions = registry.descriptions();
    assert!(descriptions.contains("- assets:"), "{descriptions}");
    assert!(descriptions.contains("- e2e-battery:"), "{descriptions}");

    // Layer 2: load_skill pulls the full body.
    let content = registry.content("assets");
    assert!(content.contains("<skill name=\"assets\">"), "{content}");
    assert!(content.contains("sha256sum"), "full body loads: {content}");

    let e2e = registry.content("e2e-battery");
    assert!(e2e.contains("Multi-agent E2E"), "playbook loads: {e2e}");
}
