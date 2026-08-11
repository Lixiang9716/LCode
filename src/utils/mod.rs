//! Error types and utility functions for LCode.

use thiserror::Error;

/// LCode-specific error types.
#[derive(Error, Debug)]
pub enum LCodeError {
    /// Configuration-related errors
    #[error("Configuration error: {0}")]
    Config(String),

    /// LLM API errors
    #[error("LLM API error: {0}")]
    LlmApi(String),

    /// Tool execution errors
    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    /// I/O errors
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization errors
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// HTTP request errors
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Agent loop errors
    #[error("Agent error: {0}")]
    Agent(String),
}

/// Convenience type alias for Results using LCodeError.
pub type Result<T> = std::result::Result<T, LCodeError>;
