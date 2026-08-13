//! LCode library crate — contains all core modules.
//!
//! The binary (main.rs) is a thin wrapper over this library,
//! which also enables integration testing in tests/.
//!
//! # Style rules
//!
//! - Functions must not exceed 50 lines (clippy::too_many_lines,
//!   threshold configured in clippy.toml).

#![warn(clippy::too_many_lines)]

pub mod agent;
pub mod app;
pub mod assets;
pub mod cli;
pub mod config;
pub mod llm;
pub mod repl;
pub mod tools;
pub mod update;
pub mod utils;
