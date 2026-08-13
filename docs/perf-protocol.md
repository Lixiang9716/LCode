# LCode 性能/E2E 对比测试协议

本协议用于"资源管理 feature"的基线轮（改动前）与终验轮（改动后）对比。
**两轮必须严格按本协议执行**，采集字段、命令、任务提示词不允许修改；
结果由主 agent 汇总进 `docs/perf-baseline.md` 并对照阈值判决。

> 环境约定：在仓库根执行；DeepSeek key 经环境变量注入，绝不写入文件；
> 所有 E2E 工作目录用 mktemp 创建、结束后可删。

## 指标与采集命令

### P1. release 构建（冷构建 ×1）
```bash
cd /home/lx/LCode && cargo clean && /usr/bin/time -f "%e s" cargo build --release 2>&1 | tail -1
```
记录：冷构建墙钟秒（取 time 输出）。

### P2. release 二进制大小
```bash
ls -l /home/lx/LCode/target/release/lcode | awk '{print $5}'
```
记录：字节数。

### P3. CLI 启动（×5 取均值）
```bash
for i in 1 2 3 4 5; do /usr/bin/time -f "%e" /home/lx/LCode/target/release/lcode --help > /dev/null 2>>/tmp/lcode_startup.txt; done
awk '{s+=$1} END {printf "%.3f\n", s/NR}' /tmp/lcode_startup.txt
```
记录：均值秒。

### P4. 测试套件（构建后全量）
```bash
cd /home/lx/LCode && /usr/bin/time -f "%e s" cargo nextest run 2>&1 | tail -2
```
记录：通过/失败总数 + 墙钟秒。

### P5. read 延迟（绝对界限测试，回归绊线）
`tests/unit_scrub.rs::scrub_10mb_text_under_200ms`：10MB 文本过 scrub 路径 < 2s
（本机 ~145ms、共享 CI runner ~350ms；被否决的 secrets_scanner 后端为 4-13s——
2s 界区分数量级回归，容忍跨机器方差）。基线轮记录 N/A。

## 真实 E2E（E2E agent，DeepSeek key）

每个任务独立临时目录，配置 `.lcode.toml`：
```toml
[llm]
provider = "openai_compatible"
model = "deepseek-v4-flash"
api_base = "https://api.deepseek.com"
reasoning_effort = "low"

[agent]
require_approval = false
```
运行方式（key 走环境变量，不进任何文件）：
```bash
cd <tmp>/<task> && LCODE_LLM_API_KEY=$DS_KEY \
  /home/lx/LCode/target/release/lcode run --auto-approve --max-turns 8 "<TASK>" 2>&1 | tail -6
```
采集字段：成功/失败、`Task completed in N turns`、`📊 Tokens:` 行、墙钟（外层 /usr/bin/time）。

### 任务提示词（固定原文）
- **T1 纯对话**：`Reply with exactly: OK`
- **T2 多轮文件修改**：`Create notes.md containing three lines: alpha, beta, gamma. Then change the second line to bravo. Then read notes.md and confirm the final content in your reply.`
- **T3 web_search 检索**：`Search the web for the latest stable Rust version and its release month, then write a one-sentence answer to rust.txt.`
- **T4 bash 资源检查**：`Use bash to run: rustc --version, git --version, and ls of the current directory. Write the outputs to checks.txt, one per line.`

## 回归（回归 agent）
```bash
cd /home/lx/LCode && cargo nextest run 2>&1 | tail -2
cargo clippy --all-targets 2>&1 | grep -cE "^warning: [a-z]"   # 期望 0
cargo fmt --check && echo FMT_OK
./scripts/check-style.sh 2>&1 | tail -1                          # 期望 🎉
```
记录：四项布尔通过 + 测试总数。

## 能力基线（能力 agent，只读）
1. 枚举 `src/tools/mod.rs` `ToolRegistry::new` 注册的工具清单（记录名称集合）
2. 运行 `cargo nextest run --test unit_llm --test unit_mcp_pool --test unit_wiring --test unit_background --test unit_compaction` 记录通过数
3. `grep -rn "list_dir\|edit_file" tests/` 统计直接依赖这两个工具的用例数（终验轮应为 0 或已改造）

## 安全基线（安全 agent，只读）
1. `grep -rn "allowed_dirs" src/` —— 记录该配置是否被文件工具消费（基线预期：仅声明未消费）
2. 运行审批/hooks 相关测试：`cargo nextest run --test unit_wiring --test unit_event_publish` 通过数
3. 记录 `src/agent/retry.rs` is_transient 特判列表（作为行为基线）

## 一键运行（scripts/e2e-battery.sh）

本协议已固化为脚本（`make e2e` 或 `scripts/e2e-battery.sh [out-dir]`）：
- 离线维度恒跑：P1 冷构建（绊线 90s）、P2 二进制（绊线 10MB）、P3 启动（绊线 200ms）、
  P4 测试套件（绊线 ≥500 通过）、clippy/fmt/style 门禁、P5 由 scrub 测试自身断言（<2s）
- 真实 API 任务（T1-T4 + 均值轮次绊线 4.5）仅在 `LCODE_E2E_API_KEY` 设置时运行
  （CI 为 repo secret `DEEPSEEK_API_KEY`）
- 输出 JSON 报告（out-dir/report.json）+ PASS/FAIL 判决与退出码；夜间 CI
  （.github/workflows/e2e-nightly.yml，UTC 02:00 + 手动触发）自动运行
- 绊线是回归绊线而非实时保证：按 CI 共享 runner 标定，抓数量级退化
  （见 perf-baseline.md 的 scrub 延迟教训）

## 输出格式（每个 agent 报告尾部，主 agent 直接收录）
```markdown
## [维度名] 结果
| 指标 | 值 |
|---|---|
| ... | ... |
```
