//! The built-in `assets` skill (resource-management conventions).
//!
//! The SKILL.md text is embedded in the binary and materialized into
//! `<workspace>/skills/assets/SKILL.md` when missing, so the skill
//! registry discovers it like any user skill — and the user can edit
//! the file afterwards (it is never overwritten). "Everything is a
//! file": the shipped skill is itself just a file.

/// The default assets skill document.
pub const ASSETS_SKILL: &str = include_str!("../../skills/assets/SKILL.md");

/// Ensure the built-in assets skill exists under `skills_dir`; writes
/// it only when missing so user edits survive. Failures are non-fatal
/// (a read-only workspace simply has no assets skill).
pub fn ensure_assets_skill(skills_dir: &std::path::Path) {
    let target = skills_dir.join("assets").join("SKILL.md");
    if target.exists() {
        return;
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&target, ASSETS_SKILL) {
        tracing::debug!(error = %e, "could not materialize the built-in assets skill");
    }
}
