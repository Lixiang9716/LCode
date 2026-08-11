# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Automated releases via **release-please**: version bumps derived from
  Conventional Commits (`feat:` → minor, `fix:` → patch,
  `BREAKING CHANGE:` → major), with prebuilt binaries for 5 platforms
  (Linux glibc/musl, macOS x86_64/arm64, Windows x86_64) attached to each
  GitHub Release
- CI performance:
  - **cargo-nextest** parallel test runner (process-per-test isolation,
    flaky-test retries, per-test timeouts)
  - Coverage workflow installs **prebuilt tarpaulin** binaries instead of
    compiling from source (4m30s → 1m21s)
  - rust-cache tuning (cache-on-failure, per-job keys) and concurrency
    cancellation for stale runs
- Style enforcement rules (`scripts/check-style.sh`, enforced in CI):
  - Source files ≤ 500 lines
  - Functions ≤ 50 lines (`clippy::too_many_lines`)
  - Indentation ≤ 5 levels in business code
  - `src/` contains no test code — tests live in `tests/`
- Test-only constructors and `#[doc(hidden)]` API exposure for migrated
  unit tests (e.g. `new_with_root`, `parse_response`)

### Changed

- Raised MSRV from Rust 1.80 to **1.94** to support edition 2024
  dependencies (serial_test 4.x requires Rust 1.93.1+)
- Moved all 116 unit tests from `src/` into `tests/` (`unit_config.rs`,
  `unit_tools.rs`, `unit_llm.rs`, `unit_agent.rs`, `unit_cli_app.rs`);
  `src/` is now pure source code
- Split `config` module into `mod`/`settings`/`commands` and
  `tools/file.rs` into `file`/`file_edit` to satisfy the 500-line file
  limit
- Upgraded dependencies: rustyline 15→18, mockall 0.13→0.15,
  serial_test 3→4, rstest 0.22→0.26, toml 0.8→1.1, tiktoken-rs 0.6→0.12
- GitHub Actions upgraded to Node 24: release-please v4→v5,
  action-gh-release v2→v3, actions/checkout v4→v7
- Removed nightly-only rustfmt options (`imports_granularity`,
  `group_imports`, `reorder_impl_items`) that emit warnings on stable
  rustfmt

### Removed

- sccache compilation cache — its GitHub Actions backend is incompatible
  with the current cache API (Swatinem/rust-cache remains in place)

## [0.1.0] - 2026-08-11

### Added

- Initial release of LCode, a Rust-based CLI code agent for autonomous software development.
- Core agent architecture with planner, executor, and memory components:
  - **Planner**: task planning and decomposition
  - **Executor**: agent loop execution
  - **Memory**: conversation memory management
- Multi-provider LLM support:
  - **OpenAI** (GPT-4, GPT-4o) and OpenAI-compatible providers
  - **Anthropic** (Claude 3.5/4)
- Tool system:
  - **File tools**: `read_file`, `write_file`, `edit_file`, `list_dir`
  - **Search tools**: `grep` (content search), `glob` (file pattern matching)
  - **Shell tool**: shell command execution
- Interactive REPL with history, auto-complete, and slash commands
- Single-shot mode: `lcode run "task description"`
- Configuration management:
  - TOML config files (global + per-project)
  - Environment variable overrides (`LCODE_` prefix)
  - CLI argument overrides
- Safety features:
  - Per-tool user approval by default
  - Dangerous command filtering (`rm -rf /`, `sudo`, `mkfs`)
  - Shell command timeout protection (120 seconds)
  - Configurable command allow/deny lists

[Unreleased]: https://github.com/Lixiang9716/LCode/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Lixiang9716/LCode/releases/tag/v0.1.0
