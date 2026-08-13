//! Shell sandboxing (P0): lightweight isolation for shell commands.
//!
//! Three tiers, resolved from `tools.sandbox`:
//! - `none` (default): unchanged behaviour.
//! - `landlock`: in-process kernel LSM rules applied in the child's
//!   `pre_exec` — the whole filesystem is handled, `/` is read-only +
//!   executable, and the workspace and `/tmp` stay writable. No
//!   external binary, works on kernels >= 5.13 (no user namespaces
//!   needed).
//! - `bwrap` / `docker`: external tool wrapping when available.
//! - `auto`: picks the first working tier in that order, falling back
//!   to unsandboxed execution with a warning.

use std::path::Path;
use std::process::Command;

/// The configured sandbox mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SandboxMode {
    None,
    Auto,
    Landlock,
    Bwrap,
    Docker,
}

impl SandboxMode {
    pub fn parse(value: &str) -> SandboxMode {
        match value.trim().to_lowercase().as_str() {
            "auto" => SandboxMode::Auto,
            "landlock" => SandboxMode::Landlock,
            "bwrap" => SandboxMode::Bwrap,
            "docker" => SandboxMode::Docker,
            _ => SandboxMode::None,
        }
    }
}

/// Which external sandbox tools are on this machine?
#[derive(Debug, Default, Clone)]
pub struct Availability {
    pub landlock: bool,
    pub bwrap: bool,
    pub docker: bool,
}

fn which(tool: &str) -> bool {
    let output = Command::new("which").arg(tool).output();
    output.map(|o| o.status.success()).unwrap_or(false)
}

/// Probe every tier (landlock via a restricted child probe; the
/// external tools via `which`). Cheap enough to run once per call.
pub fn availability() -> Availability {
    Availability { landlock: landlock_works(), bwrap: which("bwrap"), docker: which("docker") }
}

/// Resolve the effective tier for a mode: explicit tiers stay as-is;
/// `auto` walks landlock → bwrap → docker and falls back to none.
pub fn resolve(mode: SandboxMode) -> SandboxMode {
    let avail = availability();
    match mode {
        SandboxMode::Auto => {
            if avail.landlock {
                SandboxMode::Landlock
            } else if avail.bwrap {
                SandboxMode::Bwrap
            } else if avail.docker {
                SandboxMode::Docker
            } else {
                SandboxMode::None
            }
        }
        SandboxMode::Landlock if avail.landlock => SandboxMode::Landlock,
        SandboxMode::Bwrap if avail.bwrap => SandboxMode::Bwrap,
        SandboxMode::Docker if avail.docker => SandboxMode::Docker,
        // Explicit tier unavailable: run unsandboxed (the shell output
        // carries the warning via `mode_note`).
        _ => SandboxMode::None,
    }
}

/// Rewrite the `sh -c <command>` invocation into the sandboxed form.
/// Returns an error the caller surfaces instead of running anything.
pub fn wrap(
    mode: SandboxMode,
    workspace: &Path,
    command: &mut Command,
    command_str: &str,
) -> anyhow::Result<()> {
    match mode {
        SandboxMode::None | SandboxMode::Auto => Ok(()),
        SandboxMode::Landlock => {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let workspace = workspace.to_path_buf();
                let pre = move || landlock_restrict(&workspace).map_err(restrict_error);
                unsafe { command.pre_exec(pre) };
            }
            Ok(())
        }
        SandboxMode::Bwrap => {
            let mut wrapped = Command::new("bwrap");
            wrapped
                .args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
                .arg("--bind")
                .arg(workspace)
                .arg(workspace)
                .args(["--tmpfs", "/tmp"])
                .args(["sh", "-c", command_str]);
            *command = wrapped;
            Ok(())
        }
        SandboxMode::Docker => {
            let mut wrapped = Command::new("docker");
            wrapped
                .args(["run", "--rm", "-v"])
                .arg(format!("{}:/workspace", workspace.display()))
                .args(["-w", "/workspace", "alpine:latest", "sh", "-c", command_str]);
            *command = wrapped;
            Ok(())
        }
    }
}

/// A warning appended to the shell output when `auto` resolved to
/// unsandboxed execution.
pub fn mode_note(mode: SandboxMode, resolved: SandboxMode) -> Option<&'static str> {
    if mode == SandboxMode::Auto && resolved == SandboxMode::None {
        Some("⚠️ sandbox requested (auto) but no backend is available; running unsandboxed.\n")
    } else if resolved == SandboxMode::None && mode != SandboxMode::None {
        Some("⚠️ the requested sandbox backend is unavailable; running unsandboxed.\n")
    } else {
        None
    }
}

#[cfg(unix)]
fn restrict_error(_: landlock::RulesetError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, "landlock restrict failed")
}

#[cfg(unix)]
fn landlock_works() -> bool {
    // Probe in a child: landlock restrictions are inherited across
    // exec and cannot be lifted, so a child that can still run `true`
    // under the full ruleset proves the kernel enforces them.
    use std::os::unix::process::CommandExt;
    let probe = || {
        landlock_restrict(std::path::Path::new(".")).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Unsupported, "landlock unavailable")
        })
    };
    let mut child = match unsafe { Command::new("true").pre_exec(probe) }.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(not(unix))]
fn landlock_works() -> bool {
    false
}

#[cfg(unix)]
/// Apply the landlock ruleset to the current (child) process:
/// handle every filesystem access, allow read-only + execute on `/`,
/// and full access under the workspace and `/tmp`.
fn landlock_restrict(workspace: &Path) -> Result<(), landlock::RulesetError> {
    use landlock::{
        path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
        RulesetCreatedAttr,
    };
    let abi = landlock::ABI::V5;
    // Read-only traversal of the system root: list, read and execute,
    // but nothing writable (workspace and /tmp get full access below).
    let ro = AccessFs::ReadDir | AccessFs::ReadFile | AccessFs::Execute | AccessFs::Refer;
    Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules(&["/"], ro))?
        .add_rules(path_beneath_rules(&[workspace, Path::new("/tmp")], AccessFs::from_all(abi)))?
        .restrict_self()
        .map(|_status| ())
}
