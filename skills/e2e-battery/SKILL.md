---
name: e2e-battery
description: Multi-agent end-to-end testing playbook — launch parallel agents per dimension, score objectively, adjudicate findings and root-cause fixes
---

# Multi-agent E2E battery (启动多个 agent 做端到端测试并分析问题)

Use this when a feature changes agent behavior and must be verified
end-to-end with the real API. The pattern has caught real defects in
every round it ran (landlock /dev, shell timeout dead code, doubled
`$`, test cwd races, ANSI parsing).

## 1. Protocol first (协议先行)

Fix everything before launching: exact task prompts (verbatim), the
`.lcode.toml` per task, collected fields (success / turns / tokens /
wall time / artifacts), and the thresholds. Prefer the existing
protocol at `docs/perf-protocol.md`; extend it for feature-specific
dimensions instead of inventing new ones.

## 2. Launch parallel agents (并行各司一维)

One agent per dimension, all self-contained (repo path, branch/commit,
API key via env only, never written to a file):

- **E2E feature-on**: the new capability exercised with the real API;
  objective scoring points (file fields, exit codes, conversation
  evidence).
- **E2E feature-off control**: default config, proves opt-in means zero
  behavior change (baseline turns unchanged, no injected blocks).
- **Budget/state continuity** (when the feature carries state): seeded
  state must persist honestly — e.g. a resumed run cannot re-bill from
  zero.
- **Regression agent**: nextest + clippy + fmt + style + the offline
  battery (`scripts/e2e-battery.sh`).
- **Performance agent** (when deps/build change): cold build + binary
  size vs the baseline in `docs/perf-baseline.md`.

## 3. Evidence beats assertion (证据优先于断言)

Transcripts do not contain user messages. For proof of injected
context or request shapes, use one of:

- `strace` on the lcode process (git injections, execve)
- a local forwarding proxy pointed at `127.0.0.1` via
  `LCODE_LLM_API_BASE` (capture real request bodies; note the proxy
  host disables the deepseek-only reasoning gate — control runs must
  use the real endpoint)
- `strings` on the binary for feature presence, md5-copy the binary to
  survive parallel `cargo clean`

## 4. Adjudicate (主 agent 判决)

- Compare every dimension against the recorded baseline; thresholds are
  order-of-magnitude tripwires (CI runners are ~2x slower than the dev
  box), not realtime guarantees.
- New failure = fix or rollback. Turn-count +30% = investigate.
- On a breach: **root-cause first, revert only if unfixable**. Record
  the breach, the fix, and the re-verification in
  `docs/perf-baseline.md`.
- Fixes found by the battery get regression tests in the same PR.

## 5. Known traps (已知陷阱)

- Parallel `cargo clean`/`cargo build` races: cargo lock waits are
  normal; an ICE under contention usually passes on retry.
- `cargo clean` wipes target/ including report dirs created there.
- nextest ANSI colors on CI TTYs break log parsing (NO_COLOR=1).
- Tests that chdir mutate the process-global cwd: serialize them.
- First builds of the release binary may lag the branch HEAD — check
  timestamps, rebuild when behind.

## Report format (每个 agent 报告尾部)

```markdown
## <维度> | 项 | 值 |
（逐项：判分点、证据、结论 PASS/FAIL）
```
The main agent compiles the table, adjudicates, and records the
verdict.
