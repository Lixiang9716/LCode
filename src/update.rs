//! Self-update: `lcode update` fetches the latest GitHub release and
//! replaces the running binary.
//!
//! Asset naming matches `.github/workflows/release.yml`:
//! - `lcode-linux-x86_64.tar.gz`, `lcode-linux-musl-x86_64.tar.gz`
//! - `lcode-macos-x86_64.tar.gz`, `lcode-macos-aarch64.tar.gz`
//! - `lcode-windows-x86_64.exe`

use serde::Deserialize;
use std::path::{Path, PathBuf};

const RELEASES_API: &str = "https://api.github.com/repos/Lixiang9716/LCode/releases/latest";

/// A GitHub release asset.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// The latest GitHub release payload (subset we need).
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

/// Compare two dotted versions ("0.6.0" vs "0.7.1").
/// Returns `Ordering`; malformed segments are treated as 0.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let a_parts: Vec<u32> =
        a.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let b_parts: Vec<u32> =
        b.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect();

    for i in 0..a_parts.len().max(b_parts.len()) {
        let av = a_parts.get(i).copied().unwrap_or(0);
        let bv = b_parts.get(i).copied().unwrap_or(0);
        if av != bv {
            return av.cmp(&bv);
        }
    }
    std::cmp::Ordering::Equal
}

/// Map the current platform to the release asset name it should download.
///
/// Asset names match `.github/workflows/release.yml` (5 original +
/// linux aarch64 gnu/musl since v0.7.0).
pub fn asset_name_for_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("lcode-linux-x86_64.tar.gz"),
        ("linux", "aarch64") => Some("lcode-linux-aarch64.tar.gz"),
        ("macos", "x86_64") => Some("lcode-macos-x86_64.tar.gz"),
        ("macos", "aarch64") => Some("lcode-macos-aarch64.tar.gz"),
        ("windows", "x86_64") => Some("lcode-windows-x86_64.exe"),
        _ => None,
    }
}

/// Find the release asset matching the current platform.
pub fn find_asset(release: &Release) -> anyhow::Result<ReleaseAsset> {
    let expected = asset_name_for_platform()
        .ok_or_else(|| anyhow::anyhow!("No prebuilt binary for this platform"))?;
    release
        .assets
        .iter()
        .find(|a| a.name == expected)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Release {} has no asset {}", release.tag_name, expected))
}

/// Fetch the latest release info from the GitHub API.
pub async fn fetch_latest_release() -> anyhow::Result<Release> {
    let client = reqwest::Client::new();
    let mut req = client.get(RELEASES_API).header("User-Agent", "lcode-updater");
    // Optional auth avoids rate limits (public repos work without it).
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.header("Authorization", format!("Bearer {}", token));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub API error ({}): {}", resp.status(), resp.text().await?);
    }
    Ok(resp.json().await?)
}

/// Download an asset to a temp file; returns the temp path.
async fn download_asset(asset: &ReleaseAsset) -> anyhow::Result<PathBuf> {
    let client = reqwest::Client::new();
    let resp = client
        .get(&asset.browser_download_url)
        .header("User-Agent", "lcode-updater")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Download failed ({}): {}", resp.status(), asset.name);
    }
    let bytes = resp.bytes().await?;

    let tmp = std::env::temp_dir().join(format!("lcode-download-{}", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    Ok(tmp)
}

/// Extract `lcode` (or `lcode.exe`) from a tar.gz archive into `dest_dir`.
fn extract_tarball(tarball: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    let file = std::fs::File::open(tarball)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    let bin_name = if cfg!(windows) { "lcode.exe" } else { "lcode" };

    let mut extracted = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.file_name().map(|f| f == bin_name).unwrap_or(false) {
            let out = dest_dir.join(bin_name);
            entry.unpack(&out)?;
            extracted = Some(out);
            break;
        }
    }
    extracted.ok_or_else(|| anyhow::anyhow!("Binary '{}' not found in archive", bin_name))
}

/// Replace the running executable with `new_bin` (write-temp + rename;
/// on Windows the old file must be removed first).
fn install_binary(new_bin: &Path) -> anyhow::Result<()> {
    let current = std::env::current_exe()?;
    if new_bin.canonicalize().ok() == current.canonicalize().ok() {
        anyhow::bail!("Cannot replace the running binary in place");
    }

    let backup = current.with_extension("old");
    // On Windows, replacing a running exe fails: remove the old file and
    // rename the new one into place (the running process keeps its handle).
    if cfg!(windows) {
        let _ = std::fs::remove_file(&current);
    } else {
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(&current, &backup)?;
    }
    std::fs::rename(new_bin, &current)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&current, perms)?;
    }
    Ok(())
}

/// The update entry point: check / download / install.
pub async fn run(check_only: bool, force: bool) -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    println!("🔍 Checking for updates (current v{})...", current);
    let release = fetch_latest_release().await?;
    let latest = release.tag_name.trim_start_matches('v');

    match compare_versions(latest, current) {
        std::cmp::Ordering::Less | std::cmp::Ordering::Equal if !force => {
            println!("✅ Already up to date (v{})", current);
            return Ok(());
        }
        _ => {}
    }

    if check_only {
        println!("📦 New version available: v{} (current v{})", latest, current);
        return Ok(());
    }

    println!("📦 Updating to v{}...", latest);
    let asset = find_asset(&release)?;
    println!("   Downloading {}...", asset.name);
    let tmp = download_asset(&asset).await?;

    let work_dir = std::env::temp_dir().join(format!("lcode-install-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir)?;

    let new_bin = if asset.name.ends_with(".tar.gz") {
        extract_tarball(&tmp, &work_dir)?
    } else {
        // Plain executable (Windows .exe)
        let out = work_dir.join("lcode.exe");
        std::fs::copy(&tmp, &out)?;
        out
    };

    install_binary(&new_bin)?;
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir_all(&work_dir);

    println!("🎉 Updated to v{}! Restart lcode to use the new version.", latest);
    Ok(())
}
