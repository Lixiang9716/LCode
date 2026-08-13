//! Unit tests for the cross-session memory store (s09 / G3) —
//! `lcode::agent::memory_store`.
//!
//! Exercises file write/list/read with frontmatter round-tripping, the
//! `MEMORY.md` index, LLM-driven extraction / consolidation / relevance
//! (via `MockLlmProvider`), and the four memory tools end to end.

use lcode::agent::{
    ExtractMemoriesTool, ListMemoriesTool, MemoryStore, ReadMemoryTool, WriteMemoryTool,
    CONSOLIDATE_THRESHOLD,
};
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{ChatMessage, FinishReason, LlmResponse, ToolDefinition, Usage};
use lcode::tools::Tool;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

fn store_in(dir: &Path) -> MemoryStore {
    MemoryStore::new(dir).unwrap()
}

fn llm_reply(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls: None,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

/// A provider answering every chat call with a single canned reply.
fn provider_replying(content: &str) -> MockLlmProvider {
    let content = content.to_string();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(1).returning(
        move |_messages: &[ChatMessage], _tools: &[ToolDefinition]| Ok(llm_reply(&content)),
    );
    mock
}

// --- File write / list / read ---

#[test]
fn test_write_creates_frontmatter_file() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());

    let path = store.write("note", "run tests before pushing").unwrap();

    assert_eq!(path, tmp.path().join(".memory").join("note.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\n"));
    assert!(text.contains("name: \"note\""));
    assert!(text.contains("description: \"run tests before pushing\""));
    assert!(text.ends_with("---\n\nrun tests before pushing\n"));
}

#[test]
fn test_write_uses_content_frontmatter() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    let content =
        "---\nname: Prefers Tabs\ndescription: The user prefers tabs\n---\n\nTabs over spaces.";

    let path = store.write("ignored-name.md", content).unwrap();

    assert_eq!(path.file_name().unwrap(), "prefers-tabs.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("name: \"Prefers Tabs\""));
    assert!(text.contains("description: \"The user prefers tabs\""));
}

#[test]
fn test_list_returns_metadata_sorted() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("beta", "second memory").unwrap();
    store
        .write(
            "alpha",
            "---\nname: Alpha\ndescription: first memory\ntags: [core, \"user-pref\"]\n---\n\nbody text",
        )
        .unwrap();

    let files = store.list();

    assert_eq!(files.len(), 2);
    assert_eq!(files[0].filename, "alpha.md");
    assert_eq!(files[0].name, "Alpha");
    assert_eq!(files[0].description, "first memory");
    assert_eq!(files[0].tags, vec!["core", "user-pref"]);
    assert_eq!(files[0].body, "body text");
    assert_eq!(files[1].filename, "beta.md");
    assert_eq!(files[1].name, "beta");
    assert_eq!(files[1].description, "second memory");
}

#[test]
fn test_read_by_filename_name_and_slug() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("note", "run tests before pushing").unwrap();

    assert!(store.read("note.md").unwrap().contains("run tests"));
    assert!(store.read("note").unwrap().contains("run tests"));
    assert!(store.read("missing").is_none());
}

// --- MEMORY.md index ---

#[test]
fn test_index_lists_name_and_description() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("note", "run tests before pushing").unwrap();
    store
        .write("style", "---\nname: Style\ndescription: keep lines under 100\n---\n\nbody")
        .unwrap();

    let index = store.index();

    assert!(index.contains("- [note](note.md) — run tests before pushing"));
    assert!(index.contains("- [Style](style.md) — keep lines under 100"));
}

#[test]
fn test_index_rebuilds_when_missing() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("note", "something to remember").unwrap();
    std::fs::remove_file(tmp.path().join(".memory").join("MEMORY.md")).unwrap();

    let index = store.index();

    assert!(index.contains("something to remember"));
}

// --- LLM-driven extraction ---

#[tokio::test]
async fn test_extract_persists_model_facts() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    let reply = "[{\"name\": \"prefers-tabs\", \"description\": \"User prefers tabs\", \
                  \"tags\": [\"editor\"], \"body\": \"Always use tabs for indentation.\"}]";
    let provider = provider_replying(reply);

    let count = store.extract("user: please use tabs\nassistant: got it", &provider).await.unwrap();

    assert_eq!(count, 1);
    let files = store.list();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "prefers-tabs");
    assert_eq!(files[0].tags, vec!["editor"]);
    assert!(store.index().contains("- [prefers-tabs](prefers-tabs.md) — User prefers tabs"));
}

#[tokio::test]
async fn test_extract_skips_empty_dialogue() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    // No chat expectation: any LLM call would fail the mock.
    let mock = MockLlmProvider::new();

    let count = store.extract("   ", &mock).await.unwrap();

    assert_eq!(count, 0);
    assert!(store.list().is_empty());
}

#[tokio::test]
async fn test_extract_ignores_unusable_reply() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    let provider = provider_replying("no memories here");

    let count = store.extract("some dialogue", &provider).await.unwrap();

    assert_eq!(count, 0);
    assert!(store.list().is_empty());
}

// --- Consolidation ---

#[tokio::test]
async fn test_consolidate_below_threshold_is_noop() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    for i in 0..CONSOLIDATE_THRESHOLD - 1 {
        store.write(&format!("m{i}"), "body").unwrap();
    }
    // No chat expectation: below the threshold the LLM is never called.
    let mock = MockLlmProvider::new();

    let count = store.consolidate(&mock).await.unwrap();

    assert_eq!(count, CONSOLIDATE_THRESHOLD - 1);
    assert_eq!(store.list().len(), CONSOLIDATE_THRESHOLD - 1);
}

#[tokio::test]
async fn test_consolidate_merges_above_threshold() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    for i in 0..=CONSOLIDATE_THRESHOLD {
        store.write(&format!("old-memory-{i}"), "stale detail").unwrap();
    }
    let reply = "[{\"name\": \"merged\", \"description\": \"Merged memory\", \
                  \"body\": \"unified facts\"}, \
                  {\"name\": \"kept\", \"description\": \"Kept memory\", \"body\": \"valid\"}]";
    let provider = provider_replying(reply);

    let count = store.consolidate(&provider).await.unwrap();

    assert_eq!(count, 2);
    let files = store.list();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f.filename != "old-memory-0.md"));
    assert!(store.index().contains("- [merged](merged.md) — Merged memory"));
}

#[tokio::test]
async fn test_consolidate_never_wipes_on_empty_reply() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    for i in 0..=CONSOLIDATE_THRESHOLD {
        store.write(&format!("m{i}"), "body").unwrap();
    }
    let provider = provider_replying("[]");

    let count = store.consolidate(&provider).await.unwrap();

    assert_eq!(count, CONSOLIDATE_THRESHOLD + 1);
    assert_eq!(store.list().len(), CONSOLIDATE_THRESHOLD + 1);
}

// --- Relevance ---

#[tokio::test]
async fn test_relevant_selects_by_llm() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("a", "first").unwrap();
    store.write("b", "second").unwrap();
    store.write("c", "third").unwrap();
    let provider = provider_replying("[0, 2]");

    let selected = store.relevant("which memories fit?", &provider).await.unwrap();

    assert_eq!(selected, vec!["a.md", "c.md"]);
}

#[tokio::test]
async fn test_relevant_falls_back_to_keywords() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store
        .write(
            "tab-preference",
            "---\nname: Tab Preference\ndescription: user prefers tabs\n---\n\nbody",
        )
        .unwrap();
    store.write("port", "---\nname: Port\ndescription: server runs on 8080\n---\n\nbody").unwrap();
    let provider = provider_replying("I could not decide.");

    let selected = store.relevant("please remember the tabs preference", &provider).await.unwrap();

    assert_eq!(selected, vec!["tab-preference.md"]);
}

#[tokio::test]
async fn test_relevant_empty_store_returns_nothing() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    let mock = MockLlmProvider::new();

    let selected = store.relevant("anything", &mock).await.unwrap();

    assert!(selected.is_empty());
}

// --- Memory tools ---

#[test]
fn test_memory_tools_metadata() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(store_in(tmp.path()));
    let write = WriteMemoryTool { store: store.clone() };
    assert_eq!(write.name(), "write_memory");
    assert_eq!(write.parameters()["required"][0], "name");
    let list = ListMemoriesTool { store: store.clone() };
    assert_eq!(list.name(), "list_memories");
    let read = ReadMemoryTool { store: store.clone() };
    assert_eq!(read.name(), "read_memory");
    assert_eq!(read.parameters()["required"][0], "name");
    let extract = ExtractMemoriesTool { store, provider: Arc::new(MockLlmProvider::new()) };
    assert_eq!(extract.name(), "extract_memories");
}

#[test]
fn test_write_memory_tool() {
    let tmp = TempDir::new().unwrap();
    let tool = WriteMemoryTool { store: Arc::new(store_in(tmp.path())) };

    let result = tool
        .execute(&serde_json::json!({
            "name": "user-pref",
            "description": "Prefers tabs",
            "tags": ["editor"],
            "content": "Always use tabs."
        }))
        .unwrap();

    assert!(result.success);
    assert!(result.output.contains("wrote memory 'user-pref'"));
    let path = tmp.path().join(".memory").join("user-pref.md");
    assert!(path.exists());
    let index = store_in(tmp.path()).index();
    assert!(index.contains("- [user-pref](user-pref.md) — Prefers tabs"));
}

#[test]
fn test_write_memory_tool_requires_args() {
    let tmp = TempDir::new().unwrap();
    let tool = WriteMemoryTool { store: Arc::new(store_in(tmp.path())) };

    let result = tool.execute(&serde_json::json!({})).unwrap();

    assert!(!result.success);
    assert!(result.output.contains("'name' and 'content'"));
}

#[test]
fn test_list_memories_tool() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("note", "remember this").unwrap();
    let tool = ListMemoriesTool { store: Arc::new(store) };

    let result = tool.execute(&serde_json::json!({})).unwrap();

    assert!(result.success);
    assert!(result.output.contains("- [note](note.md) — remember this"));
}

#[test]
fn test_list_memories_tool_empty_store() {
    let tmp = TempDir::new().unwrap();
    let tool = ListMemoriesTool { store: Arc::new(store_in(tmp.path())) };

    let result = tool.execute(&serde_json::json!({})).unwrap();

    assert!(result.success);
    assert_eq!(result.output, "no memories stored");
}

#[test]
fn test_read_memory_tool() {
    let tmp = TempDir::new().unwrap();
    let store = store_in(tmp.path());
    store.write("note", "remember this").unwrap();
    let tool = ReadMemoryTool { store: Arc::new(store) };

    let ok = tool.execute(&serde_json::json!({ "name": "note" })).unwrap();
    assert!(ok.success);
    assert!(ok.output.contains("remember this"));

    let missing = tool.execute(&serde_json::json!({ "name": "nope" })).unwrap();
    assert!(!missing.success);
    assert!(missing.output.contains("no memory found"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_extract_memories_tool_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(store_in(tmp.path()));
    let provider: Arc<dyn lcode::llm::LlmProvider> = Arc::new(provider_replying(
        "[{\"name\": \"port\", \"description\": \"Server port\", \"body\": \"Runs on 8080\"}]",
    ));
    let tool = ExtractMemoriesTool { store: store.clone(), provider };

    let result =
        tool.execute(&serde_json::json!({ "conversation": "user: the port is 8080" })).unwrap();

    assert!(result.success);
    assert!(result.output.contains("extracted 1 new memories"));
    assert_eq!(store.list().len(), 1);
}

#[test]
fn test_extract_memories_tool_requires_runtime() {
    let tmp = TempDir::new().unwrap();
    // No chat expectation: the tool must fail before touching the LLM.
    let provider: Arc<dyn lcode::llm::LlmProvider> = Arc::new(MockLlmProvider::new());
    let tool = ExtractMemoriesTool { store: Arc::new(store_in(tmp.path())), provider };

    // Outside a tokio runtime the tool fails cleanly instead of hanging.
    let result = tool.execute(&serde_json::json!({ "conversation": "hi" }));

    assert!(result.is_err());
}
