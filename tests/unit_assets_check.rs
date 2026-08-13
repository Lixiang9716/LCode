//! Tests for `lcode assets check`: sidecar schema validation per kind,
//! sha256 integrity, orphan detection, and the CLI wiring.

use lcode::assets::{check, AssetCheckReport};
use sha2::Digest;

fn sha_of(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

fn write_sidecar(dir: &std::path::Path, name: &str, value: serde_json::Value) {
    std::fs::write(dir.join("assets").join(format!("{name}.meta.json")), value.to_string())
        .unwrap();
}

fn setup(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("assets")).unwrap();
}

fn run(dir: &std::path::Path) -> AssetCheckReport {
    check(dir).expect("check runs")
}

#[test]
fn empty_registry_is_clean() {
    let dir = tempfile::TempDir::new().unwrap();
    let report = run(dir.path());
    assert!(report.ok(), "{}", report.render());
}

#[test]
fn valid_file_and_url_resources_pass() {
    let dir = tempfile::TempDir::new().unwrap();
    setup(dir.path());
    let payload = b"hello asset";
    std::fs::write(dir.path().join("assets").join("logo.txt"), payload).unwrap();
    write_sidecar(
        dir.path(),
        "logo.txt",
        serde_json::json!({
            "name": "logo.txt", "kind": "file",
            "file": { "sha256": sha_of(payload), "size_bytes": payload.len() }
        }),
    );
    write_sidecar(
        dir.path(),
        "rust-docs",
        serde_json::json!({
            "name": "rust-docs", "kind": "url",
            "url": { "value": "https://doc.rust-lang.org", "last_status": 200 }
        }),
    );

    let report = run(dir.path());
    assert!(report.ok(), "{}", report.render());
    assert_eq!(report.metas, 2);
    assert_eq!(report.payloads, 1);
}

#[test]
fn sha256_mismatch_is_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    setup(dir.path());
    let payload = b"actual content";
    std::fs::write(dir.path().join("assets").join("data.bin"), payload).unwrap();
    write_sidecar(
        dir.path(),
        "data.bin",
        serde_json::json!({
            "name": "data.bin", "kind": "file",
            "file": { "sha256": sha_of(b"different"), "size_bytes": 1 }
        }),
    );

    let report = run(dir.path());
    assert!(!report.ok());
    assert!(report.render().contains("sha256 mismatch"), "{}", report.render());
}

#[test]
fn unknown_kind_and_missing_fields_are_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    setup(dir.path());
    write_sidecar(dir.path(), "bad-kind", serde_json::json!({ "name": "bad-kind", "kind": "wat" }));
    write_sidecar(dir.path(), "no-url", serde_json::json!({ "name": "no-url", "kind": "url" }));

    let report = run(dir.path());
    assert!(!report.ok());
    assert!(report.render().contains("unknown kind 'wat'"), "{}", report.render());
    assert!(report.render().contains("url.value"), "{}", report.render());
}

#[test]
fn orphan_payload_and_sidecar_are_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    setup(dir.path());
    std::fs::write(dir.path().join("assets").join("stray.txt"), "x").unwrap();
    write_sidecar(
        dir.path(),
        "gone.bin",
        serde_json::json!({
            "name": "gone.bin", "kind": "file",
            "file": { "sha256": sha_of(b"x"), "size_bytes": 1 }
        }),
    );

    let report = run(dir.path());
    assert!(!report.ok());
    assert!(report.render().contains("without a sidecar"), "{}", report.render());
    assert!(report.render().contains("without its payload"), "{}", report.render());
}

#[test]
fn leftover_tmp_files_are_reported() {
    let dir = tempfile::TempDir::new().unwrap();
    setup(dir.path());
    std::fs::write(dir.path().join("assets").join(".tmp-123"), "x").unwrap();

    let report = run(dir.path());
    assert!(!report.ok());
    assert!(report.render().contains("temporary file"), "{}", report.render());
}

#[test]
fn env_secret_tool_kinds_are_sidecar_only() {
    let dir = tempfile::TempDir::new().unwrap();
    setup(dir.path());
    // Sidecar-only kinds must not be flagged as orphans.
    write_sidecar(
        dir.path(),
        "RUST_LOG",
        serde_json::json!({ "name": "RUST_LOG", "kind": "env", "env": { "var": "RUST_LOG" } }),
    );
    let report = run(dir.path());
    assert!(report.ok(), "{}", report.render());
}

#[test]
fn cli_parses_assets_check() {
    use clap::Parser;
    let cli = lcode::cli::Cli::try_parse_from(["lcode", "assets", "check"]);
    assert!(cli.is_ok(), "{cli:?}");
}
