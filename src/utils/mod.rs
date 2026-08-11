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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_display_message() {
        let err = LCodeError::Config("invalid provider".to_string());
        assert_eq!(err.to_string(), "Configuration error: invalid provider");
    }

    #[test]
    fn llm_api_error_display_message() {
        let err = LCodeError::LlmApi("rate limited".to_string());
        assert_eq!(err.to_string(), "LLM API error: rate limited");
    }

    #[test]
    fn tool_execution_error_display_message() {
        let err = LCodeError::ToolExecution("command failed".to_string());
        assert_eq!(err.to_string(), "Tool execution error: command failed");
    }

    #[test]
    fn io_error_display_message() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = LCodeError::Io(io_err);
        assert_eq!(err.to_string(), "I/O error: file not found");
    }

    #[test]
    fn io_error_via_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err: LCodeError = io_err.into();
        assert_eq!(err.to_string(), "I/O error: permission denied");
    }

    #[test]
    fn serde_error_via_from_conversion() {
        let serde_err = serde_json::from_str::<serde_json::Value>("{invalid").unwrap_err();
        let err: LCodeError = serde_err.into();
        assert!(err.to_string().starts_with("Serialization error: "));
    }

    #[test]
    fn agent_error_display_message() {
        let err = LCodeError::Agent("loop exceeded".to_string());
        assert_eq!(err.to_string(), "Agent error: loop exceeded");
    }

    #[test]
    fn result_alias_carries_lcode_error() {
        fn ok_fn() -> Result<i32> {
            Ok(42)
        }
        fn err_fn() -> Result<i32> {
            Err(LCodeError::Config("boom".to_string()))
        }
        assert_eq!(ok_fn().unwrap(), 42);
        assert!(matches!(err_fn(), Err(LCodeError::Config(msg)) if msg == "boom"));
    }
}
