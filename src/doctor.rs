//! `lcode doctor` — environment health check (P0).
//!
//! Read-only sections: version (local vs GitHub latest), toolchain,
//! config validity (key masked), assets registry, transcripts, disk
//! space, sandbox availability and the DeepSeek balance. Network
//! checks fail soft (skipped, never an error).

use crate::config::Config;

/// Run every check and print the report. Never fails the CLI: each
/// section reports its own status.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    println!("🔍 LCode doctor\n");
    println!("[version]");
    print_version().await;
    println!("\n[toolchain]");
    print_tool("git");
    print_tool("rustc");
    print_tool("cargo");
    println!("\n[config]");
    print_config(cfg);
    println!("\n[assets]");
    print_assets();
    println!("\n[transcripts]");
    print_transcripts();
    println!("\n[disk]");
    print_disk();
    println!("\n[sandbox]");
    print_sandbox();
    println!("\n[balance]");
    print_balance(cfg).await;
    Ok(())
}

async fn print_version() {
    let local = env!("CARGO_PKG_VERSION");
    println!("✅ lcode {local}");
    match crate::update::fetch_latest_release().await {
        Ok(release) => {
            if release.tag_name.trim_start_matches('v') == local {
                println!("✅ up to date ({})", release.tag_name);
            } else {
                println!("⚠️ newer release available: {} (local {local})", release.tag_name);
            }
        }
        Err(e) => println!("⚠️ GitHub release check skipped: {e}"),
    }
}

fn print_tool(tool: &str) {
    match std::process::Command::new(tool).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("✅ {tool} {version}");
        }
        _ => println!("❌ {tool} not found or not runnable"),
    }
}

fn print_config(cfg: &Config) {
    let key = secrecy::ExposeSecret::expose_secret(&cfg.llm.api_key);
    let masked = if key.is_empty() { "(unset)".to_string() } else { crate::config::mask_key(key) };
    println!(
        "✅ provider={} model={} api_key={} budget={}",
        cfg.llm.provider,
        cfg.llm.model,
        masked,
        cfg.llm.budget_total_usd.map(|b| format!("${b}")).unwrap_or_else(|| "none".to_string())
    );
    if !cfg.tools.allowed_dirs.is_empty() {
        println!("ℹ️  allowed_dirs: {:?}", cfg.tools.allowed_dirs);
    }
}

fn print_assets() {
    let workspace = std::env::current_dir().unwrap_or_default();
    match crate::assets::check(&workspace) {
        Ok(report) if report.ok() => println!("✅ {}", report.render()),
        Ok(report) => println!("❌ {}", report.render()),
        Err(e) => println!("⚠️ assets check failed: {e}"),
    }
}

fn print_transcripts() {
    let dir = std::env::current_dir().unwrap_or_default().join(".transcripts");
    if !dir.is_dir() {
        println!("ℹ️  no .transcripts/ directory yet");
        return;
    }
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|e| e == "jsonl") {
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    println!("✅ {files} event/transcript file(s), {:.1} MiB total", bytes as f64 / 1_048_576.0);
}

fn print_disk() {
    let Ok(output) = std::process::Command::new("df").args(["-P", "-k", "."]).output() else {
        println!("⚠️ df unavailable");
        return;
    };
    if !output.status.success() {
        println!("⚠️ df failed");
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(line) = text.lines().nth(1) else {
        println!("⚠️ could not parse df output");
        return;
    };
    let fields: Vec<&str> = line.split_whitespace().collect();
    match fields.get(3).and_then(|v| v.parse::<u64>().ok()) {
        Some(kb) => {
            println!("✅ {:.1} GiB free in the workspace filesystem", kb as f64 / 1_048_576.0)
        }
        None => println!("⚠️ could not parse df output"),
    }
}

fn print_sandbox() {
    let mode =
        crate::tools::sandbox::SandboxMode::parse(&crate::config::Config::default().tools.sandbox);
    let _ = mode;
    let avail = crate::tools::sandbox::availability();
    let backend = if avail.landlock {
        "landlock".to_string()
    } else if avail.bwrap {
        "bwrap".to_string()
    } else if avail.docker {
        "docker".to_string()
    } else {
        "none".to_string()
    };
    println!(
        "✅ sandbox backend: {backend} (landlock {}, bwrap {}, docker {})",
        avail.landlock, avail.bwrap, avail.docker
    );
}

async fn print_balance(cfg: &Config) {
    let key = secrecy::ExposeSecret::expose_secret(&cfg.llm.api_key).to_string();
    if key.is_empty() {
        println!("ℹ️  no API key configured; balance check skipped");
        return;
    }
    let base = match cfg.llm.provider.to_lowercase().as_str() {
        "deepseek" => Some("https://api.deepseek.com".to_string()),
        "openai_compatible" | "openai" => cfg.llm.api_base.clone(),
        _ => None,
    };
    let Some(base) = base else {
        println!("ℹ️  balance check only supports DeepSeek endpoints");
        return;
    };
    let base = base.trim_end_matches('/').replace("/v1", "");
    if !crate::llm::is_deepseek_endpoint(&base) {
        println!("ℹ️  balance check only supports DeepSeek endpoints");
        return;
    }
    let url = format!("{base}/user/balance");
    let auth = format!("Bearer {key}");
    match crate::tools::fetch::fetch_json_with_auth(&url, 15, 4096, Some(auth)) {
        Ok((bytes, _)) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => {
                let available = value["is_available"].as_bool().unwrap_or(false);
                let balances = value["balance_infos"].as_array().cloned().unwrap_or_default();
                if balances.is_empty() {
                    println!("✅ balance available: {available}");
                }
                for entry in balances {
                    let currency = entry["currency"].as_str().unwrap_or("?");
                    let total = entry["total_balance"].as_str().unwrap_or("?");
                    println!("✅ balance: {total} {currency}");
                }
            }
            Err(_) => println!("⚠️ balance response unparsable"),
        },
        Err(e) => println!("⚠️ balance check skipped: {e}"),
    }
}
