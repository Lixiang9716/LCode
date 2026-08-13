# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0](https://github.com/Lixiang9716/LCode/compare/v0.8.0...v0.9.0) (2026-08-13)


### Features

* **agent:** persist every agent event as JSONL audit log ([5363a0f](https://github.com/Lixiang9716/LCode/commit/5363a0f6e0fe6b8845688f2865f06553f349061c))
* **agent:** publish each streamed delta as a TextDelta event ([5878aa0](https://github.com/Lixiang9716/LCode/commit/5878aa0a60bcf71be3cb4392f48e03b3ef36ba09))
* complete learn-claude-code consistency (G1-G14) ([f891b3f](https://github.com/Lixiang9716/LCode/commit/f891b3f1dcb94602b409dfc109b9e3c33d927e94))
* **config:** make every hardcoded runtime value user-tunable ([e173b04](https://github.com/Lixiang9716/LCode/commit/e173b049844c4683eba9f9a8404f926795ca4957))
* **config:** make every hardcoded runtime value user-tunable ([ef008fc](https://github.com/Lixiang9716/LCode/commit/ef008fc7f65d0fc4cad5adeeedf805089883d382))
* wire memory injection and lead inbox drain into executor ([e39c734](https://github.com/Lixiang9716/LCode/commit/e39c7346f535108d35eee6ea44e58bac63ad7b8f))


### Bug Fixes

* **agent:** blockable read_inbox and session-end teammate shutdown ([d514dbd](https://github.com/Lixiang9716/LCode/commit/d514dbd24f62ab28da486b328b9fb7b3c36a3711))
* **agent:** compaction summarizes instead of echoing instructions ([4a9bc7f](https://github.com/Lixiang9716/LCode/commit/4a9bc7fecfc4bb92fc715398431a24764d93f9d6))
* **agent:** publish the six events that had no publisher ([ffb6351](https://github.com/Lixiang9716/LCode/commit/ffb635163a61a72323007c0e38ad6d17f85edd40))
* **agent:** release event bus so lcode exits after tasks ([0192ed2](https://github.com/Lixiang9716/LCode/commit/0192ed2a21331df2221c2bbc02c80f20f526bdca))
* close final consistency gaps ([99b7810](https://github.com/Lixiang9716/LCode/commit/99b7810df3635358f387c1b955eab0497c38773b))
* **llm:** merge parallel tool results into one Anthropic user message ([c4b2b2a](https://github.com/Lixiang9716/LCode/commit/c4b2b2a7f2788a23f75940bedfc95988a99a6e58))
* **llm:** stream sentinels must not overwrite the finish reason ([e0ab75c](https://github.com/Lixiang9716/LCode/commit/e0ab75cc51fed27148c1e9cc5afc0784ea467590))
* **repl:** approval flag was inverted; retry intermittent wire 400 ([ab5a1bf](https://github.com/Lixiang9716/LCode/commit/ab5a1bff5980d7713be26380641365a8e4f5284d))

## [0.8.0](https://github.com/Lixiang9716/LCode/compare/v0.7.0...v0.8.0) (2026-08-12)


### Features

* **release:** add linux aarch64 binaries (gnu + musl) ([5eb7963](https://github.com/Lixiang9716/LCode/commit/5eb796377f26f97d815a86c5d03cc4f3b222d40a))
* **release:** add linux aarch64 binaries (gnu + musl) ([867c2bd](https://github.com/Lixiang9716/LCode/commit/867c2bda8b7015e254cd33be048178bd719c14bb))

## [0.7.0](https://github.com/Lixiang9716/LCode/compare/v0.6.0...v0.7.0) (2026-08-12)


### Features

* add lcode update (self-update from GitHub releases) ([8154c04](https://github.com/Lixiang9716/LCode/commit/8154c04907a1061269afc4dca3f869a3af2f8ae9))
* add lcode update command (self-update from GitHub releases) ([12890a4](https://github.com/Lixiang9716/LCode/commit/12890a465efda700c7f8171a8aa5f84e753d1b1e))

## [0.6.0](https://github.com/Lixiang9716/LCode/compare/v0.5.0...v0.6.0) (2026-08-12)


### Features

* **agent:** wire cron ticks and streaming consumption into executor ([2282f75](https://github.com/Lixiang9716/LCode/commit/2282f7570d7a549fcb6f5f9e69cb6aa6e6daafc9))
* **agent:** wire MCP tools into the dynamic per-turn tool pool (s19) ([b806385](https://github.com/Lixiang9716/LCode/commit/b8063852e6c4e9cf56f2d3f5fd4ec10195a0be71))
* wire cron ticks, streaming, lcode serve and session CLI into the loop ([2cb3812](https://github.com/Lixiang9716/LCode/commit/2cb3812e29f9d81cfcd2d1c87cfb72df4bcb230c))
* wire MCP tools into dynamic per-turn tool pool (s19) ([5ca4087](https://github.com/Lixiang9716/LCode/commit/5ca40872bc56d827390a2458ce7f20dd90f68ac0))

## [0.5.0](https://github.com/Lixiang9716/LCode/compare/v0.4.0...v0.5.0) (2026-08-11)


### Features

* close all 12 capability gaps (cron/mcp/hooks/retry/providers/streaming/session/web/vscode/token/fanout/shutdown) ([e3ddd03](https://github.com/Lixiang9716/LCode/commit/e3ddd0338592088478b07d0fe37503b382e5bf6e))

## [0.4.0](https://github.com/Lixiang9716/LCode/compare/v0.3.0...v0.4.0) (2026-08-11)


### Features

* **agent:** scaffold round-2 capabilities (cron/mcp/hooks/session/retry/streaming) ([a63cfd4](https://github.com/Lixiang9716/LCode/commit/a63cfd4de5533993a1c28041c22aab626abaaffc))

## [0.3.0](https://github.com/Lixiang9716/LCode/compare/v0.2.0...v0.3.0) (2026-08-11)


### Features

* port learn-claude-code capabilities (s03-s12) ([464331d](https://github.com/Lixiang9716/LCode/commit/464331dd031f5823491d9f88123fe546442f69e1))

## [0.2.0](https://github.com/Lixiang9716/LCode/compare/v0.1.0...v0.2.0) (2026-08-11)


### Features

* **agent:** scaffold session capabilities (learn-claude-code parity) ([201f6c8](https://github.com/Lixiang9716/LCode/commit/201f6c8a428bb9c2167996482baf6c07365054e9))

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
