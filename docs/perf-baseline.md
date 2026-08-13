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

## 终验轮记录（Phase F，commit bcd5cf5 + 修复 6a2241f）

| 维度 | 基线 | 终验 | 阈值 | 判决 |
|---|---|---|---|---|
| P1 冷构建 | 56.19 s | 69.19 s → **56.85 s**（+1.2%） | +20% | ⚠️→✅（见下） |
| P2 二进制 | 6,280,272 B | 6,388,520 B（+1.7%） | +30% | ✅ |
| P3 启动 | ~0（厘秒分辨率不足） | 5.83 ms（纳秒法） | +50 ms 绝对 | ✅ |
| P4 测试套件 | 457 通过 | 482/482 通过 | +25% 时长 | ✅ |
| P5 read 延迟 | N/A | 145 ms（10MB scrub） | < 200 ms 绝对 | ✅ |
| E2E T1-T4 | 1/4/7/3 轮，全成功 | 1/4/6/2 轮，全成功，0 重试 | 新增失败 / 轮次 +30% | ✅（平均轮次 -13%；T2 正确改用 write_file replace） |
| 能力 | 7 工具；55/55；残留 17 行 | 5 工具；55/55；残留 0；新功能 57/57 | 基线功能回退即修 | ✅ |
| 安全 | allowed_dirs 死配置 | 已消费（读/写+canonicalize 防逃逸）；28/28；scrub 11/11；is_transient 一致 | 基线回退即修 | ✅ |
| 回归 | 457 全绿 | 482 全绿（2 次并行负载 flaky，单测复跑各 3/3 稳定） | — | ✅ |

### 阈值触达记录（如实）
- **P1 首次终验 +23.1% 超限**：根因为 reqwest `blocking` feature 使冷构建 56.19s → 69.19s。
  修复（6a2241f）：fetcher 线程改用 async client + 自建 current-thread runtime，移除 blocking
  feature；复测 56.85s（+1.2%）与二进制 +1.7%，均回阈值内。**未发生回退**——根因修复优于
  revert，修复后数据达标。
- P5 延迟界测试在并行全量下偶发超时（隔离复跑 3/3 稳定 142-148ms）：标记 `#[serial]` 消除负载
  干扰，并记录为环境噪声而非回归。
- 回归 agent 报告的两例 flaky（scrub 延迟界、openai wiremock）每次用例不同、隔离复跑稳定，
  与基线轮观察一致，判定为已知环境噪声。

### 最终判决：PASS（全部维度在阈值内，无需回退）
资源管理 feature 六阶段提交（34550ac / 3718246 / b8c5af5 / fefacaa / bcd5cf5 / 6a2241f）
按原计划合入；性能对比数据、阈值触达与修复过程如上，全程留痕。

## P0 终验记录（Phase E，commit 8cecfec + 修复 dc6c085）

P0 批次（预算硬闸 / 质量闭环 / 沙箱 / doctor+events），同一 5-agent 电池、同协议：

| 维度 | 基线 | 终验 | 阈值 | 判决 |
|---|---|---|---|---|
| 默认 E2E T1-T4 | 1/4/7/3 轮全成功 | 1/4/6/2 轮全成功，0 重试 | 新增失败即回退 / 轮次 +30% | ✅（轮次持平或更优） |
| sandbox=auto 专项 | N/A | 首测 ❌（git 因 /dev/null 被拒即死）→ 根因修复 → 复验 ✅ T4 沙箱下 2 轮完成 | 专项必须通过 | ✅（见下） |
| self_review=true 专项 | N/A | 审查轮真实发生（代理观测 1 次审查请求+APPROVE，false 对照 0 次）；T2 4 轮持平；成本 +902 prompt tokens | 轮次 ≤ 4+2 | ✅ |
| 回归 | 520 | 521/521、clippy 0、fmt、style | — | ✅ |
| 性能 | 56.85s / 6.39MB | 58.61s（+3.1%）/ 6.52MB（+2.1%） | +20% / +30% | ✅ |

### 阈值触达记录（如实）
- **sandbox 专项首测失败**：landlock 规则集未授权 `/dev`，git 启动时
  `openat("/dev/null", O_RDWR)` 被拒（strace 证据），所有 git 命令即死。根因修复
  （dc6c085：`/dev` 子树全权——设备操作不是文件系统逃逸面），补回归测试
  （landlock 下 /dev/null 可用），主 agent 复验 T4 任务沙箱下 2 轮完成、checks.txt
  正确。**未回退**——根因修复优于 revert，修复后数据达标。
- 默认配置（无 P0 新配置）行为与基线完全一致：P0 四个特性全部 opt-in 或失败才触发，
  零默认回归。

### 最终判决：PASS（全部维度在阈值内，无需回退）
P0 五阶段提交（218b2ea / 67e1cd1 / a617922 / 8cecfec / dc6c085）按计划合入；
阈值触达与根因修复全程留痕。
