//! Asset registry checker — the enforcement half of the "everything is
//! a file" conventions.
//!
//! The `assets` skill defines the layout (payload + `<name>.meta.json`
//! sidecars); this module validates a workspace's `assets/` directory
//! read-only: sidecar schema per kind, file-kind payload presence and
//! sha256 integrity, orphan payloads/sidecars and leftover temp files.

use std::path::Path;

/// The seven resource kinds defined by the assets skill.
const KINDS: [&str; 7] = ["file", "url", "env", "secret", "tool", "service", "quota"];

/// Required sidecar fields per kind (beyond the common name/kind).
const KIND_REQUIRED: [&[&str]; 7] = [
    &["file.sha256", "file.size_bytes"], // file
    &["url.value"],                      // url
    &["env.var"],                        // env
    &["secret.var", "secret.location"],  // secret
    &["tool.command"],                   // tool
    &["service.host", "service.port"],   // service
    &["quota.provider"],                 // quota
];

/// A single validation finding.
#[derive(Debug, Clone)]
pub struct Issue {
    /// Asset name (or filename) the issue belongs to.
    pub name: String,
    /// Human-readable problem description.
    pub detail: String,
}

/// Result of an `assets check` run.
#[derive(Debug, Default)]
pub struct AssetCheckReport {
    pub metas: usize,
    pub payloads: usize,
    pub issues: Vec<Issue>,
}

impl AssetCheckReport {
    pub fn ok(&self) -> bool {
        self.issues.is_empty()
    }

    /// Human-readable one-line-per-issue report.
    pub fn render(&self) -> String {
        if self.ok() {
            return format!(
                "assets check: OK ({} resource(s), {} payload file(s))",
                self.metas, self.payloads
            );
        }
        let mut lines = vec![format!(
            "assets check: {} issue(s) ({} resource(s), {} payload file(s))",
            self.issues.len(),
            self.metas,
            self.payloads
        )];
        for issue in &self.issues {
            lines.push(format!("- {}: {}", issue.name, issue.detail));
        }
        lines.join("\n")
    }
}

/// Run the checker over `<workspace>/assets/`. A missing directory is a
/// clean empty registry, not an error.
pub fn check(workspace: &Path) -> anyhow::Result<AssetCheckReport> {
    let dir = workspace.join("assets");
    let mut report = AssetCheckReport::default();
    if !dir.is_dir() {
        return Ok(report);
    }
    let (payloads, metas) = scan_entries(&dir, &mut report)?;
    cross_check(&dir, &payloads, &metas, &mut report);
    Ok(report)
}

/// First pass: validate every entry, collecting payload / sidecar name
/// sets for the cross-check.
fn scan_entries(
    dir: &Path,
    report: &mut AssetCheckReport,
) -> anyhow::Result<(std::collections::HashSet<String>, std::collections::HashSet<String>)> {
    let mut payload_names = std::collections::HashSet::new();
    let mut meta_names = std::collections::HashSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            report.issues.push(Issue {
                name: file_name,
                detail: "nested directory in assets/ (flat layout only)".to_string(),
            });
            continue;
        }
        if file_name.starts_with(".tmp-") {
            report.issues.push(Issue {
                name: file_name,
                detail: "leftover temporary file from an interrupted fetch".to_string(),
            });
            continue;
        }
        if let Some(name) = file_name.strip_suffix(".meta.json") {
            report.metas += 1;
            meta_names.insert(name.to_string());
            check_sidecar(entry.path().as_path(), name, report)?;
        } else {
            report.payloads += 1;
            payload_names.insert(file_name);
        }
    }
    Ok((payload_names, meta_names))
}

/// Second pass: payloads without sidecars, and file-kind sidecars
/// without payloads (the other six kinds are sidecar-only by design).
fn cross_check(
    dir: &Path,
    payloads: &std::collections::HashSet<String>,
    metas: &std::collections::HashSet<String>,
    report: &mut AssetCheckReport,
) {
    for name in payloads {
        if !metas.contains(name) {
            report.issues.push(Issue {
                name: name.clone(),
                detail: "payload file without a sidecar (unregistered asset)".to_string(),
            });
        }
    }
    for name in metas {
        if payloads.contains(name) {
            continue;
        }
        let Ok(kind) = sidecar_kind(&dir.join(format!("{name}.meta.json"))) else {
            continue;
        };
        if kind == "file" {
            report.issues.push(Issue {
                name: name.clone(),
                detail: "file resource without its payload".to_string(),
            });
        }
    }
}

/// Validate one sidecar: JSON parses, kind is known, required fields
/// exist, and file-kind sha256 matches the payload.
fn check_sidecar(path: &Path, name: &str, report: &mut AssetCheckReport) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            report
                .issues
                .push(Issue { name: name.to_string(), detail: format!("invalid JSON: {e}") });
            return Ok(());
        }
    };
    let Some(obj) = value.as_object() else {
        report.issues.push(Issue {
            name: name.to_string(),
            detail: "sidecar must be a JSON object".to_string(),
        });
        return Ok(());
    };
    let kind = obj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if !KINDS.contains(&kind) {
        report.issues.push(Issue {
            name: name.to_string(),
            detail: format!("unknown kind '{kind}' (expected one of {})", KINDS.join("/")),
        });
        return Ok(());
    }
    let idx = KINDS.iter().position(|k| *k == kind).expect("kind found");
    for required in KIND_REQUIRED[idx] {
        if dot_lookup(&value, required).is_none() {
            report.issues.push(Issue {
                name: name.to_string(),
                detail: format!("missing required field '{required}'"),
            });
        }
    }
    if kind == "file" {
        verify_payload_sha(path.parent().expect("assets dir"), name, &value, report);
    }
    Ok(())
}

/// `a.b.c` lookup on a nested JSON value.
fn dot_lookup<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(value, |current, key| current.get(key))
}

/// Recompute the payload's sha256 and compare with the sidecar.
fn verify_payload_sha(
    dir: &Path,
    name: &str,
    sidecar: &serde_json::Value,
    report: &mut AssetCheckReport,
) {
    let payload_path = dir.join(name);
    let expected = sidecar.get("file").and_then(|f| f.get("sha256")).and_then(|v| v.as_str());
    let Some(expected) = expected else { return };
    let Ok(bytes) = std::fs::read(&payload_path) else {
        report.issues.push(Issue { name: name.to_string(), detail: "payload missing".to_string() });
        return;
    };
    use sha2::Digest;
    let actual = hex::encode(sha2::Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        report.issues.push(Issue {
            name: name.to_string(),
            detail: format!("sha256 mismatch (sidecar {expected}, payload {actual})"),
        });
    }
}

/// Kind of a sidecar file (unchecked read, used by the orphan cross-check).
fn sidecar_kind(path: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    Ok(value.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string())
}
