# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 (2026-08-11)


### Bug Fixes

* raise MSRV to 1.94 for edition 2024 dependencies ([b41b348](https://github.com/Lixiang9716/LCode/commit/b41b348e1b63b84367ff315685421764dc049b33))
* resolve clippy warnings and formatting issues ([313d2a3](https://github.com/Lixiang9716/LCode/commit/313d2a3ca6c915e69012d103d0b2cc755b2f53e1))

## [Unreleased]

### Changed

- Raised MSRV from Rust 1.80 to **1.94** to support edition 2024 dependencies
  (serial_test 4.x requires Rust 1.93.1+)

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
