//! Cross-session memory store (learn-claude-code s09).
//!
//! Persistent facts survive session restarts: the store owns `.memory/`
//! in the workspace (one markdown file per memory, `MEMORY.md` as the
//! index). The agent can write/list/read memories through four tools;
//! extraction ([`MemoryStore::extract`]) and consolidation
//! ([`MemoryStore::consolidate`]) are LLM-driven and async.
//!
//! Executor integration points — wired by the coordinator once the
//! executor refactor (first batch, `feat/consistency-batch1`) lands; do
//! **not** edit `executor.rs` here:
//!
//! 1. **Stop extraction** — when a turn ends with
//!    [`crate::llm::FinishReason::Stop`], call
//!    `store.extract(&conversation_text, provider.as_ref()).await` on
//!    the pre-compression conversation text (s09: `extract_memories` in
//!    the stop branch), then `store.consolidate(provider.as_ref()).await`
//!    so the index stays tidy across sessions.
//! 2. **Per-turn injection** — before building the context for each
//!    turn, call `store.index()` and append it to the memory section of
//!    the assembled system prompt (`prompt::session_sections` memory
//!    param). For tighter relevance, `store.relevant(query,
//!    provider.as_ref()).await` returns filenames whose content can be
//!    injected into the current user turn (s09: `build_system` +
//!    `load_memories`).
//!
//! LLM calls from tools follow the synchronous-tool-over-async pattern:
//! `tokio::task::block_in_place` + `Handle::block_on` (see
//! `subagent.rs`).

use std::path::{Path, PathBuf};

/// Directory holding memory files (relative to the workspace).
const MEMORY_DIR: &str = ".memory";
/// Index file name inside the memory directory.
const MEMORY_INDEX: &str = "MEMORY.md";
/// Consolidation kicks in when the memory file count reaches this.
pub const CONSOLIDATE_THRESHOLD: usize = 10;
/// Maximum memories injected per relevance query.
const MAX_RELEVANT: usize = 5;
/// Catalog size fed to the consolidation model.
pub(crate) const MAX_CONSOLIDATE_CHARS: usize = 16_000;
/// Query size fed to the relevance model.
pub(crate) const MAX_QUERY_CHARS: usize = 2000;

/// A single memory file with its parsed metadata.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub filename: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub body: String,
}

/// Persistent, filesystem-backed memory store rooted at `workspace/.memory/`.
///
/// The store is stateless (every operation reads/writes the directory),
/// so it is cheap to share via `Arc` and can be rebuilt at any time.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    pub(crate) dir: PathBuf,
    pub(crate) consolidate_threshold: usize,
    pub(crate) max_relevant: usize,
    pub(crate) max_extract_chars: usize,
    /// Lock extraction replies to JSON via prefix completion (P1-1).
    pub(crate) json_lock: bool,
}

impl MemoryStore {
    /// Create a store rooted at `workspace/.memory/`, creating the
    /// directory when missing. Tuning values come from the defaults.
    pub fn new(workspace: &Path) -> anyhow::Result<Self> {
        Self::with_config(workspace, &crate::config::MemoryConfig::default())
    }

    /// Create a store with user-tunable thresholds from `config`.
    pub fn with_config(
        workspace: &Path,
        config: &crate::config::MemoryConfig,
    ) -> anyhow::Result<Self> {
        let dir = workspace.join(MEMORY_DIR);
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            consolidate_threshold: config.consolidate_threshold,
            max_relevant: config.max_relevant,
            max_extract_chars: config.max_extract_chars,
            json_lock: config.json_lock,
        })
    }

    /// The `.memory/` directory backing this store.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write (or overwrite) a memory file, then rebuild the index.
    ///
    /// `content` may carry its own `---` YAML frontmatter (`name`,
    /// `description`, `tags`); otherwise metadata is derived from
    /// `file_name` (slug + first non-empty line as description).
    /// Returns the path of the written file.
    pub fn write(&self, file_name: &str, content: &str) -> anyhow::Result<PathBuf> {
        let (name, description, tags, body) = memory_parts(file_name, content);
        let path = self.dir.join(format!("{}.md", slugify(&name)));
        std::fs::write(&path, format_memory_file(&name, &description, &tags, &body))?;
        self.rebuild_index();
        Ok(path)
    }

    /// List all memory files (excluding the index), sorted by filename.
    pub fn list(&self) -> Vec<MemoryFile> {
        let mut files = Vec::new();
        let mut entries: Vec<_> = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries.flatten().collect(),
            Err(_) => return files,
        };
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let filename = entry.file_name().to_string_lossy().into_owned();
            if filename == MEMORY_INDEX || !filename.ends_with(".md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let (name, description, tags, body) = memory_parts(&filename, &text);
            files.push(MemoryFile { filename, name, description, tags, body });
        }
        files
    }

    /// Read a memory file by exact filename (`.md` optional), memory
    /// name, or slug.
    pub fn read(&self, name: &str) -> Option<String> {
        let exact = self.dir.join(name);
        if exact.is_file() {
            return std::fs::read_to_string(exact).ok();
        }
        let stemmed = self.dir.join(format!("{}.md", name.trim_end_matches(".md")));
        if stemmed.is_file() {
            return std::fs::read_to_string(stemmed).ok();
        }
        let slgged = self.dir.join(format!("{}.md", slugify(name)));
        if slgged.is_file() {
            return std::fs::read_to_string(slgged).ok();
        }
        None
    }

    /// Rebuild `MEMORY.md` from the current memory files (name +
    /// description list, one line per memory).
    pub fn rebuild_index(&self) {
        let lines: Vec<String> = self
            .list()
            .iter()
            .map(|f| format!("- [{}]({}) — {}", f.name, f.filename, f.description))
            .collect();
        let content =
            if lines.is_empty() { String::new() } else { format!("{}\n", lines.join("\n")) };
        let _ = std::fs::write(self.dir.join(MEMORY_INDEX), content);
    }

    /// Read the `MEMORY.md` index, rebuilding it first when missing.
    pub fn index(&self) -> String {
        let path = self.dir.join(MEMORY_INDEX);
        if !path.exists() {
            self.rebuild_index();
        }
        std::fs::read_to_string(path).unwrap_or_default().trim().to_string()
    }

    /// One-line `- name: description` catalog used as the model's
    /// dedupe aid during extraction.
    pub(crate) fn existing_catalog(&self) -> String {
        let lines: Vec<String> =
            self.list().iter().map(|f| format!("- {}: {}", f.name, f.description)).collect();
        if lines.is_empty() {
            "(none)".to_string()
        } else {
            lines.join("\n")
        }
    }

    /// Persist validated items returned by the model; returns the
    /// filenames written (empty when nothing was usable).
    pub(crate) fn write_items(&self, items: &[serde_json::Value]) -> Vec<String> {
        let mut written = Vec::new();
        for item in items {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
            let description = item.get("description").and_then(|v| v.as_str()).unwrap_or("").trim();
            let body = item.get("body").and_then(|v| v.as_str()).unwrap_or("").trim();
            if name.is_empty() || body.is_empty() {
                continue;
            }
            let tags = json_tags(item.get("tags"));
            let content = format_memory_file(name, description, &tags, body);
            if self.write(&format!("{name}.md"), &content).is_ok() {
                written.push(format!("{}.md", slugify(name)));
            }
        }
        written
    }
}

/// Map model-chosen indices to memory filenames (bounded by
/// [`MAX_RELEVANT`], out-of-range indices ignored).
pub(crate) fn select_indices(files: &[MemoryFile], indices: &[usize]) -> Vec<String> {
    let mut selected = Vec::new();
    for idx in indices {
        if selected.len() >= MAX_RELEVANT {
            break;
        }
        if let Some(f) = files.get(*idx) {
            selected.push(f.filename.clone());
        }
    }
    selected
}

/// Parse `name`/`description`/`tags` and the body from memory content,
/// deriving from `file_name` when no frontmatter is present.
fn memory_parts(file_name: &str, content: &str) -> (String, String, Vec<String>, String) {
    match parse_frontmatter(content) {
        Some((meta, body)) => {
            let meta = meta_json(&meta);
            let name = meta
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| stem(file_name));
            let description =
                meta.get("description").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            (name, description, json_tags(meta.get("tags")), body.trim().to_string())
        }
        None => {
            let description =
                content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            (stem(file_name), description, Vec::new(), content.trim().to_string())
        }
    }
}

/// Serialize a memory to the on-disk format: YAML frontmatter (quoted,
/// so arbitrary descriptions round-trip) plus the markdown body.
fn format_memory_file(name: &str, description: &str, tags: &[String], body: &str) -> String {
    let mut meta =
        format!("---\nname: {}\ndescription: {}\n", yaml_str(name), yaml_str(description));
    if !tags.is_empty() {
        let tags: Vec<String> = tags.iter().map(|t| yaml_str(t)).collect();
        meta.push_str(&format!("tags: [{}]\n", tags.join(", ")));
    }
    format!("{meta}---\n\n{body}\n")
}

/// Quote a string as a YAML double-quoted scalar.
fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " "))
}

/// `kebab-case` identifier from a memory name.
fn slugify(name: &str) -> String {
    let mut slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "memory".to_string()
    } else {
        slug
    }
}

fn stem(filename: &str) -> String {
    filename.strip_suffix(".md").unwrap_or(filename).to_string()
}

/// Split `---`-delimited YAML frontmatter from the body.
fn parse_frontmatter(text: &str) -> Option<(serde_yaml::Value, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let yaml = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    Some((serde_yaml::from_str(yaml).unwrap_or(serde_yaml::Value::Null), body))
}

/// Uniform JSON view of YAML frontmatter metadata.
fn meta_json(meta: &serde_yaml::Value) -> serde_json::Value {
    serde_json::to_value(meta).unwrap_or(serde_json::Value::Null)
}

/// Tags from a JSON array (or comma-separated string).
fn json_tags(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(arr)) => {
            arr.iter().filter_map(|t| t.as_str()).map(str::to_string).collect()
        }
        Some(serde_json::Value::String(s)) => {
            s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()
        }
        _ => Vec::new(),
    }
}

/// Extract the first JSON array from a model reply.
pub(crate) fn extract_json_array(text: &str) -> Vec<serde_json::Value> {
    let Some(start) = text.find('[') else {
        return Vec::new();
    };
    let Some(end) = text.rfind(']') else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    serde_json::from_str(&text[start..=end]).unwrap_or_default()
}

/// Trim a string to `max` chars at a character boundary.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}

/// The four memory tools (`write_memory`, `extract_memories`,
/// `list_memories`, `read_memory`) and their registration.
mod tools;

pub use tools::{register, ExtractMemoriesTool, ListMemoriesTool, ReadMemoryTool, WriteMemoryTool};
