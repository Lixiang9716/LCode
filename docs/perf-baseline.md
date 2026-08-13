# LCode 性能/E2E 基线（资源管理 feature）

- 基线 commit：`b1f9dd4`（feat/token-usage-cost）
- 采集日期：2026-08-13
- 协议：见 `docs/perf-protocol.md`（两轮同协议；终验轮结果追加到本文档对比）

## 基线值（5 agent 并行采集）

| 维度 | 指标 | 基线值 |
|---|---|---|
| 性能 P1 | release 冷构建 | 56.19 s |
| 性能 P2 | release 二进制 | 6,280,272 字节 |
| 性能 P3 | CLI 启动（--help ×5 均值） | ~0.000 s（低于 /usr/bin/time 厘秒分辨率） |
| 性能 P4 | 测试套件 | 457/457 通过；墙钟 12.06 s（含编译；nextest Summary 1.70 s） |
| 性能 P5 | read 延迟 | N/A（终验轮由新增延迟界测试提供绝对值） |
| E2E T1 | 纯对话 | 成功，1 轮，4319+3 tokens，1.34 s |
| E2E T2 | 多轮文件修改 | 成功，4 轮，18242+284 tokens（13056 命中），4.76 s；notes.md=alpha/bravo/gamma |
| E2E T3 | web_search 检索 | 成功，7 轮，36277+1433 tokens（28800 命中，926 reasoning），20.32 s；rust.txt=1.97.1 |
| E2E T4 | bash 资源检查 | 成功，3 轮，13576+206 tokens（8576 命中，6 reasoning），3.71 s；checks.txt 3 行 |
| 回归 | 门禁 | 457/457 通过；clippy 0；fmt OK；style ✅（1 次 wiremock 偶发 flaky，复跑稳定，记噪声不记回归） |
| 能力 | 工具清单 | read_file, write_file, edit_file, list_dir, grep, glob, shell（7 个）；5 套件 55/55 |
| 能力 | 删改依赖 | tests 中 list_dir/edit_file 直接引用 17 行（integration_test 6 / unit_tools 8 / unit_streaming 2 / unit_providers 1） |
| 安全 | allowed_dirs | **死配置**：仅声明/合并（settings.rs:189,207 / mod.rs:80-81 / commands.rs:30），文件工具不消费（file.rs:62、file_edit.rs:66 仅 join workspace_root，无 canonicalize 防逃逸） |
| 安全 | 审批/hooks 测试 | 15/15 通过 |
| 安全 | is_transient 特判 | 429 / 529 / timeout / rate limit / overloaded / 500 / 502 / 503 / 400∧"tool_use ids were found without tool_result" |

## 回退阈值（终验轮对照）

| 指标 | 警戒线 |
|---|---|
| 冷构建 / 二进制 / 测试套件墙钟 | +20% / +30% / +25% |
| 启动 | P3 分辨率不足 → 终验轮改用纳秒计时（date +%s%N），+50ms 绝对警戒 |
| E2E 失败数 / 平均轮次 | 新增失败即回退 / +30% |
| 能力/安全断言 | 任何基线功能回退即修 |

## 终验轮记录（Phase F 后追加）

（待填）
