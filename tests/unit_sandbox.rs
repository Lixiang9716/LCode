//! P0 sandbox tests: mode parsing, resolution, wrapper shapes, and the
//! landlock enforcement behaviour when the kernel supports it.

use lcode::tools::sandbox::{self, SandboxMode};
use lcode::tools::shell::ShellTool;

#[test]
fn mode_parsing() {
    assert_eq!(SandboxMode::parse("none"), SandboxMode::None);
    assert_eq!(SandboxMode::parse("auto"), SandboxMode::Auto);
    assert_eq!(SandboxMode::parse("landlock"), SandboxMode::Landlock);
    assert_eq!(SandboxMode::parse("BWRAP"), SandboxMode::Bwrap);
    assert_eq!(SandboxMode::parse("docker"), SandboxMode::Docker);
    assert_eq!(SandboxMode::parse("banana"), SandboxMode::None);
}

#[test]
fn resolution_rules() {
    assert_eq!(sandbox::resolve(SandboxMode::None), SandboxMode::None);
    let avail = sandbox::availability();
    if avail.landlock {
        assert_eq!(sandbox::resolve(SandboxMode::Auto), SandboxMode::Landlock);
        assert_eq!(sandbox::resolve(SandboxMode::Landlock), SandboxMode::Landlock);
    } else {
        assert_ne!(sandbox::resolve(SandboxMode::Auto), SandboxMode::Landlock);
    }
    // An explicitly unavailable backend degrades to none with a note.
    if !avail.docker {
        assert_eq!(sandbox::resolve(SandboxMode::Docker), SandboxMode::None);
    }
}

#[test]
fn bwrap_wrapper_shape() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut command = std::process::Command::new("sh");
    sandbox::wrap(SandboxMode::Bwrap, dir.path(), &mut command, "echo hi").unwrap();
    let program = command.get_program().to_string_lossy().into_owned();
    assert_eq!(program, "bwrap");
    let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(args.contains(&"--ro-bind".to_string()), "{args:?}");
    assert!(args.contains(&"echo hi".to_string()), "{args:?}");
}

#[test]
fn docker_wrapper_shape() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut command = std::process::Command::new("sh");
    sandbox::wrap(SandboxMode::Docker, dir.path(), &mut command, "echo hi").unwrap();
    assert_eq!(command.get_program().to_string_lossy(), "docker");
    let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(args.iter().any(|a| a.contains("alpine:latest")), "{args:?}");
    assert!(args.contains(&"echo hi".to_string()), "{args:?}");
}

#[test]
fn mode_notes() {
    assert_eq!(sandbox::mode_note(SandboxMode::None, SandboxMode::None), None);
    let note = sandbox::mode_note(SandboxMode::Auto, SandboxMode::None);
    assert!(note.is_some_and(|n| n.contains("unsandboxed")));
}

#[test]
fn landlock_probe_answers() {
    // Must simply return a bool without panicking, whatever the kernel.
    let _ = sandbox::availability();
}

#[test]
fn landlock_denies_writes_outside_workspace_when_enforced() {
    if !sandbox::availability().landlock {
        return; // kernel without enforcement: nothing to assert
    }
    let dir = tempfile::TempDir::new().unwrap();
    let tool = ShellTool::new_with_root(dir.path().to_path_buf());
    // The shell tool uses SandboxMode::None by default; landlock is
    // exercised through the raw wrapper here instead.
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg("touch /etc/lcode-sandbox-probe 2>/dev/null && echo WROTE || echo DENIED")
        .current_dir(dir.path());
    sandbox::wrap(
        SandboxMode::Landlock,
        dir.path(),
        &mut command,
        "touch /etc/lcode-sandbox-probe 2>/dev/null && echo WROTE || echo DENIED",
    )
    .unwrap();
    let output = command.output().expect("probe runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("DENIED"), "landlock must deny writes under /etc, got: {stdout}");
    let _ = tool; // the plain tool itself stays unsandboxed (default none)
}
