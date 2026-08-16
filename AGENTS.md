# AGENTS.md — LCode workspace guide

LCode is an event-driven Rust CLI coding agent (DeepSeek-centric). Read this
before changing anything; it encodes hard-won lessons that cost hours.

## Build / verify (always use these)

```bash
cargo nextest run                     # tests — NEVER `cargo test` (user requirement)
cargo clippy --all-targets            # must be 0 warnings (CI enforces -D warnings)
cargo fmt && cargo fmt --check
./scripts/check-style.sh              # file/indent gates, must print 🎉
scripts/e2e-battery.sh [out-dir]      # offline battery always; LCODE_E2E_API_KEY adds real-API tasks
make e2e                              # alias
```

Gate order that must ALL be green before commit: nextest → clippy → fmt →
check-style. CI runs the same plus MSRV (1.94) and nightly battery.

## Hard style gates (they will bite)

- **500 lines/file** (business code; enforced by check-style.sh)
- **50 lines/function** — clippy `too_many_lines` counts NON-BLANK NON-COMMENT
  body lines. Trimming comments does NOT fix it; move code into helpers.
- **5 indentation levels max** (check-style.sh counts braces)
- **No tests inside src/** — all tests live in tests/*.rs
- Keep under these when adding features; extract sibling modules
  (executor_hooks.rs, quality.rs, workspace.rs, run_entry.rs,
  anthropic_parse.rs, provider_build.rs, memory_store_llm.rs already exist
  for exactly this reason).

## Architecture boundaries

- Event-driven: `AgentEvent` broadcast bus + `AgentCommand` mpsc channel
  (src/agent/runtime.rs). The recorder auto-persists every event variant to
  `.transcripts/events_*.jsonl` — user messages are NOT events; if an
  injected message must be auditable, publish a dedicated event
  (see `WorkspaceContext` precedent).
- Provider layer (src/llm/): OpenAI-format and Anthropic-format wire
  builders are separate (openai.rs / anthropic.rs / anthropic_parse.rs).
  DeepSeek specifics: anthropic endpoint requires a thinking placeholder
  block per assistant message when thinking is on; effort knob is
  `output_config: {effort}` (NOT `reasoning`); cost math uses
  `cache_miss_tokens` as the input base; endpoint detection must be exact
  host match (`llm::is_deepseek_endpoint`), never substring.
- Tool surface is deliberately minimal: read_file / write_file (with
  `replace` + `url` modes) / grep / glob / shell. Filtering and permission
  policies live in src/tools/{scrub,fetch,guard,sandbox}.rs.
- Executor loop (src/agent/executor.rs + executor_hooks.rs + quality.rs):
  turn-start injections, budget gate, test-until-green, self-review,
  checkpoint seeding all plug in here.

## Conventions

- **Conventional commits** + feature branch + PR; release-please owns
  versions (never edit Cargo.toml version manually).
- Behavior-changing features default **off** (opt-in) so the E2E baseline
  never regresses; one-way config merge for bools (document which way).
- `#[doc(hidden)] pub` is the accepted escape hatch for test-only accessors.
- Config wiring has 4 points: settings.rs (+Default) → mod.rs merge+env →
  commands.rs list/get/set → README.
- Async-in-sync-tool trap: reqwest blocking client panics inside the async
  executor. Use the fetcher-thread bridge pattern (src/tools/fetch.rs).
- `std::env::set_current_dir` is process-global: tests that chdir must be
  `#[serial_test::serial]` and chdir BEFORE constructing tools/registry.

## Known gotchas

- **Performance tripwires are order-of-magnitude, not absolutes**: shared CI
  runners are ~2x slower than this machine. Calibrate against regression
  classes (the rejected secrets_scanner took 4-13s → 2s tripwire), not the
  dev box (see docs/perf-baseline.md scrub lesson).
- `cargo clean` wipes target/ including any report directory you created
  there — init reports AFTER the clean.
- nextest emits ANSI on CI TTYs: parse logs with NO_COLOR=1 or strip escapes.
- `cargo clean`-style builds race with parallel agents: cargo lock waits are
  normal; an ICE under contention usually resolves on retry.
- Python bulk edits vs rustfmt: after `cargo fmt`, string-matching edits
  fail silently — always `assert old in s` and re-read the file when an
  assert fires.

## Docs to read before sensitive changes

- `docs/perf-protocol.md` + `docs/perf-baseline.md` — reversible-engineering
  workflow: baseline → phases → thresholds → root-cause fixes, all verdicts
  recorded here. New big features must follow it (multi-agent E2E battery).
- `docs/deepseek-api-report.md` — measured API behavior (thinking replay,
  output_config, prefix beta, web_search shapes, cache semantics).
- `docs/assets-adherence.md` — seven-kind resource conventions (assets/ +
  sidecars) and the skill-adherence E2E results.
- `skills/assets/SKILL.md` — built-in skill text; materialized into
  workspaces, never overwritten on update.
