//! URL fetching for the file tools (read_file / write_file URL mode).
//!
//! Single entry point [`fetch_url`] with every gate in one place:
//! scheme whitelist, host policy (deny first, then allow), timeouts,
//! and a streamed size cap. The blocking reqwest client keeps the
//! synchronous `Tool` trait free of runtime-thread gymnastics.

use crate::config::ToolsConfig;
use std::time::Duration;

/// Does `path` look like an http(s) URL?
pub fn is_http_url(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

/// Extract `host[:port]` from an http(s) URL; `None` when the URL does
/// not parse to a network location. Shared with the shell guardrails.
pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url.trim_start_matches("https://").trim_start_matches("http://");
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Does `host` match a policy entry (exact or `*.suffix` wildcard)?
/// Shared with the shell guardrails.
pub(crate) fn host_matches(host: &str, entry: &str) -> bool {
    if let Some(suffix) = entry.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        host.eq_ignore_ascii_case(entry)
    }
}

/// A dedicated fetcher thread owns the reqwest client and a
/// current-thread tokio runtime. Building or dropping a runtime from
/// inside an async context (the executor runs tools synchronously
/// within its async loop) panics with "Cannot drop a runtime in a
/// context where blocking is not allowed"; owning both on a plain OS
/// thread keeps every build/use/drop in a sync context, and the
/// channel bridge keeps the `Tool` trait synchronous. The plain async
/// client is used (no `blocking` feature — it roughly doubled the
/// cold-build cost).
type FetchJob =
    (String, u64, usize, std::sync::mpsc::SyncSender<anyhow::Result<(Vec<u8>, Option<String>)>>);
static FETCHER: std::sync::OnceLock<std::sync::mpsc::SyncSender<FetchJob>> =
    std::sync::OnceLock::new();

fn fetcher() -> &'static std::sync::mpsc::SyncSender<FetchJob> {
    FETCHER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<FetchJob>(8);
        std::thread::Builder::new()
            .name("lcode-fetcher".to_string())
            .spawn(move || fetcher_loop(rx))
            .expect("fetcher thread spawns");
        tx
    })
}

/// The fetcher thread body: one client, one runtime, jobs from the
/// channel until the session drops the sender.
fn fetcher_loop(rx: std::sync::mpsc::Receiver<FetchJob>) {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("fetch client builds");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fetcher runtime builds");
    for (url, timeout_secs, max_bytes, reply) in rx.iter() {
        let result = runtime.block_on(fetch_on_thread(&client, &url, timeout_secs, max_bytes));
        let _ = reply.send(result);
    }
}

/// The actual download, running on the fetcher thread (sync context).
async fn fetch_on_thread(
    client: &reqwest::Client,
    url: &str,
    timeout_secs: u64,
    max_bytes: usize,
) -> anyhow::Result<(Vec<u8>, Option<String>)> {
    let mut response = client.get(url).timeout(Duration::from_secs(timeout_secs)).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("fetch failed with HTTP status {}", response.status());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    // Streamed cap: stop as soon as the configured limit is exceeded —
    // a Content-Length hint short-circuits the download entirely.
    if let Some(len) = response.content_length() {
        if len as usize > max_bytes {
            anyhow::bail!("fetch exceeds tools.max_fetch_bytes ({} > {})", len, max_bytes);
        }
    }
    let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
    loop {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        bytes.extend_from_slice(&chunk);
        if bytes.len() > max_bytes {
            anyhow::bail!("fetch exceeds tools.max_fetch_bytes ({} bytes limit)", max_bytes);
        }
    }
    Ok((bytes, content_type))
}
/// Host policy check: deny list first, then allow list (empty = allow
/// all). Port suffixes are ignored for matching.
fn host_allowed(host_and_port: &str, cfg: &ToolsConfig) -> anyhow::Result<()> {
    let host = host_and_port.split(':').next().unwrap_or(host_and_port);
    if cfg.denied_hosts.iter().any(|e| host_matches(host, e)) {
        anyhow::bail!("host '{host}' is denied by tools.denied_hosts");
    }
    if !cfg.allowed_hosts.is_empty() && !cfg.allowed_hosts.iter().any(|e| host_matches(host, e)) {
        anyhow::bail!("host '{host}' is not in tools.allowed_hosts");
    }
    Ok(())
}

/// Fetch a URL with every gate applied. Returns the body bytes and the
/// response's Content-Type (when present).
pub fn fetch_url(url: &str, cfg: &ToolsConfig) -> anyhow::Result<(Vec<u8>, Option<String>)> {
    if !is_http_url(url) {
        anyhow::bail!("unsupported URL scheme (only http/https): {url}");
    }
    if !cfg.enable_web {
        anyhow::bail!("web access is disabled (tools.enable_web = false)");
    }
    let host = host_of(url).ok_or_else(|| anyhow::anyhow!("invalid URL: {url}"))?;
    host_allowed(&host, cfg)?;

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let job = (url.to_string(), cfg.fetch_timeout_secs, cfg.max_fetch_bytes, tx);
    fetcher().send(job)?;
    rx.recv().map_err(|_| anyhow::anyhow!("fetcher thread terminated"))?
}
