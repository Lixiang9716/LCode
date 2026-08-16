//! The built-in `assets` skill (resource-management conventions).
//!
//! The SKILL.md text is embedded in the binary and materialized into
//! `<workspace>/skills/assets/SKILL.md` when missing, so the skill
//! registry discovers it like any user skill — and the user can edit
//! the file afterwards (it is never overwritten). "Everything is a
//! file": the shipped skill is itself just a file.

/// The default assets skill document.
pub const ASSETS_SKILL: &str = include_str!("../../skills/assets/SKILL.md");
/// The multi-agent E2E testing playbook.
pub const E2E_SKILL: &str = include_str!("../../skills/e2e-battery/SKILL.md");

/// Every built-in skill shipped in the binary: (directory, content).
pub const BUILTIN_SKILLS: [(&str, &str); 2] =
    [("assets", ASSETS_SKILL), ("e2e-battery", E2E_SKILL)];

/// Ensure the built-in skills exist under `skills_dir`; each is written
/// only when missing so user edits survive. Failures are non-fatal (a
/// read-only workspace simply has no built-in skills).
pub fn ensure_assets_skill(skills_dir: &std::path::Path) {
    for (name, content) in BUILTIN_SKILLS {
        let target = skills_dir.join(name).join("SKILL.md");
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&target, content) {
            tracing::debug!(error = %e, skill = name, "could not materialize the built-in skill");
        }
    }
}
