//! LCode library crate — contains all core modules.
//!
//! The binary (main.rs) is a thin wrapper over this library,
//! which also enables integration testing in tests/.

pub mod agent;
pub mod app;
pub mod cli;
pub mod config;
pub mod llm;
pub mod repl;
pub mod tools;
pub mod utils;
