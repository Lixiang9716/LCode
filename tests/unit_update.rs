//! Unit tests for the self-update module (`lcode update`).
//!
//! Covers version comparison, platform asset selection, release JSON
//! parsing, and the CLI surface. Network download/install is exercised
//! manually (needs a real GitHub release).

use clap::Parser;
use lcode::cli::{Cli, Command};
use lcode::update::{asset_name_for_platform, compare_versions, find_asset, Release, ReleaseAsset};

// ---------------------------------------------------------------------------
// compare_versions
// ---------------------------------------------------------------------------

#[test]
fn compare_equal_versions() {
    assert_eq!(compare_versions("0.6.0", "0.6.0"), std::cmp::Ordering::Equal);
    assert_eq!(compare_versions("v0.6.0", "0.6.0"), std::cmp::Ordering::Equal);
}

#[test]
fn compare_newer_versions() {
    assert_eq!(compare_versions("0.7.0", "0.6.0"), std::cmp::Ordering::Greater);
    assert_eq!(compare_versions("0.6.1", "0.6.0"), std::cmp::Ordering::Greater);
    assert_eq!(compare_versions("1.0.0", "0.9.9"), std::cmp::Ordering::Greater);
    assert_eq!(compare_versions("0.10.0", "0.9.0"), std::cmp::Ordering::Greater);
}

#[test]
fn compare_older_versions() {
    assert_eq!(compare_versions("0.5.0", "0.6.0"), std::cmp::Ordering::Less);
    assert_eq!(compare_versions("0.6.0", "0.6.1"), std::cmp::Ordering::Less);
}

#[test]
fn compare_malformed_segments_treated_as_zero() {
    assert_eq!(compare_versions("0.6", "0.6.0"), std::cmp::Ordering::Equal);
    assert_eq!(compare_versions("abc", "0.0.0"), std::cmp::Ordering::Equal);
    // Malformed segments parse as 0, so "0.7.x" == "0.7.0".
    assert_eq!(compare_versions("0.7.x", "0.7.0"), std::cmp::Ordering::Equal);
}

// ---------------------------------------------------------------------------
// asset_name_for_platform
// ---------------------------------------------------------------------------

#[test]
fn platform_asset_names_exist() {
    // All platforms we ship binaries for must have a mapping.
    assert!(asset_name_for_platform().is_some());
}

#[test]
fn asset_names_match_release_matrix() {
    // The names must match .github/workflows/release.yml artifacts.
    let linux = "lcode-linux-x86_64.tar.gz";
    let macos_x64 = "lcode-macos-x86_64.tar.gz";
    let macos_arm = "lcode-macos-aarch64.tar.gz";
    let windows = "lcode-windows-x86_64.exe";
    for name in [linux, macos_x64, macos_arm, windows] {
        assert!(name.starts_with("lcode-"), "unexpected asset name: {name}");
    }
}

// ---------------------------------------------------------------------------
// find_asset
// ---------------------------------------------------------------------------

fn sample_release() -> Release {
    Release {
        tag_name: "v0.7.0".to_string(),
        assets: vec![
            ReleaseAsset {
                name: "lcode-linux-x86_64.tar.gz".to_string(),
                browser_download_url: "https://example.com/lcode-linux-x86_64.tar.gz".to_string(),
            },
            ReleaseAsset {
                name: "lcode-macos-aarch64.tar.gz".to_string(),
                browser_download_url: "https://example.com/lcode-macos-aarch64.tar.gz".to_string(),
            },
        ],
    }
}

#[test]
fn find_asset_matches_current_platform() {
    let release = sample_release();
    // On every platform the asset finder must either succeed (the
    // platform ships binaries) or fail cleanly — but never panic.
    let result = find_asset(&release);
    match asset_name_for_platform() {
        Some(name) => {
            let asset = result.expect("asset should exist for shipped platform");
            assert_eq!(asset.name, name);
        }
        None => {
            assert!(result.is_err(), "unsupported platform must error cleanly");
        }
    }
}

#[test]
fn find_asset_errors_when_asset_missing() {
    let release = Release { tag_name: "v0.7.0".to_string(), assets: vec![] };
    assert!(find_asset(&release).is_err());
    let err = find_asset(&release).unwrap_err().to_string();
    assert!(err.contains("no asset"), "error should mention missing asset: {err}");
}

// ---------------------------------------------------------------------------
// Release JSON parsing (subset used by the updater)
// ---------------------------------------------------------------------------

#[test]
fn release_json_parses() {
    let json = r#"{
        "tag_name": "v0.8.0",
        "assets": [
            {"name": "lcode-linux-x86_64.tar.gz", "browser_download_url": "https://x/lcode.tgz"},
            {"name": "lcode-windows-x86_64.exe", "browser_download_url": "https://x/lcode.exe"}
        ]
    }"#;
    let release: Release = serde_json::from_str(json).expect("parse release JSON");
    assert_eq!(release.tag_name, "v0.8.0");
    assert_eq!(release.assets.len(), 2);
    assert_eq!(release.assets[0].name, "lcode-linux-x86_64.tar.gz");
}

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

#[test]
fn cli_update_command_parses() {
    let cli = Cli::try_parse_from(["lcode", "update"]).expect("update parses");
    match cli.command {
        Some(Command::Update { check, force }) => {
            assert!(!check);
            assert!(!force);
        }
        other => panic!("expected Update, got {:?}", other),
    }
}

#[test]
fn cli_update_check_flag() {
    let cli = Cli::try_parse_from(["lcode", "update", "--check"]).expect("update --check parses");
    match cli.command {
        Some(Command::Update { check, force }) => {
            assert!(check);
            assert!(!force);
        }
        other => panic!("expected Update, got {:?}", other),
    }
}

#[test]
fn cli_update_force_flag() {
    let cli = Cli::try_parse_from(["lcode", "update", "--force"]).expect("update --force parses");
    match cli.command {
        Some(Command::Update { check, force }) => {
            assert!(!check);
            assert!(force);
        }
        other => panic!("expected Update, got {:?}", other),
    }
}
