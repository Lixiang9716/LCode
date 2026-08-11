//! Unit tests for exact token counting (`src/agent/memory.rs`).
//!
//! `token_count` / `exact_tokens` use the real cl100k_base BPE tokenizer
//! (tiktoken); `approximate_tokens` keeps the fast `chars / 4` heuristic.

use lcode::agent::{exact_tokens, ConversationMemory};

#[test]
fn hello_world_counts_two_tokens() {
    let mut memory = ConversationMemory::new(String::new());
    memory.add_user("Hello world");
    // cl100k_base splits "Hello world" into ["Hello", " world"] = 2.
    assert_eq!(memory.token_count(), 2);
}

#[test]
fn empty_text_is_zero_tokens() {
    let memory = ConversationMemory::new(String::new());
    assert_eq!(memory.token_count(), 0);
    assert_eq!(exact_tokens(""), 0);
}

#[test]
fn token_count_includes_system_prompt() {
    let mut memory = ConversationMemory::new("You are LCode, a coding agent.".to_string());
    memory.add_user("Fix the bug in src/main.rs");
    let system_only = exact_tokens("You are LCode, a coding agent.");
    let message_only = exact_tokens("Fix the bug in src/main.rs");
    assert_eq!(memory.token_count(), system_only + message_only);
}

#[test]
fn approximate_and_exact_agree_in_magnitude() {
    // The chars/4 heuristic should stay within the same order of
    // magnitude as the real tokenizer for English prose.
    let mut memory = ConversationMemory::new(String::new());
    let paragraph = "The quick brown fox jumps over the lazy dog. ".repeat(100);
    memory.add_user(paragraph.clone());

    let exact = memory.token_count();
    let approximate = memory.approximate_tokens();

    assert!(exact > 0, "exact count must be positive");
    assert!(approximate >= exact / 3, "approximate ({approximate}) far below exact ({exact})");
    assert!(approximate <= exact * 3, "approximate ({approximate}) far above exact ({exact})");
}

#[test]
fn exact_tokens_handles_code_and_symbols() {
    // Code-heavy text is more token-dense than prose but must still be
    // counted without panicking and stay positive.
    let code = "fn main() { println!(\"hi\"); } let x = 42; // comment";
    let count = exact_tokens(code);
    assert!(count >= 10, "expected a reasonable token count, got {count}");
    assert!(count <= code.len(), "tokens can never exceed input length");
}
