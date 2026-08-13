# Assets skill 遵守度 E2E 报告（2026-08-13）

3 个并行 E2E agent 用真实 DeepSeek API 验证模型对内置 `assets` skill
约定的遵守情况。判分点全部客观化：sidecar 字段逐一核对 +
`lcode assets check` 独立复跑退出码（0 = 完全合规）。

## 结果

| 场景 | 任务完成 | 轮次 | sidecar 合规 | 值泄露 | assets check | load_skill |
|---|---|---|---|---|---|---|
| file（notes.txt 注册+sha256） | ✅ | 7 | ✅ kind=file、sha256 与实算精确一致 | 无 | ✅ 退出 0 | ✅ 首调 |
| url（rust-docs 注册+检查） | ⚠️ 轮次耗尽 | 12 | ✅ kind=url、value/last_status/checked_at 齐全 | 无 | ✅ 退出 0 | ✅ 首调 |
| env/tool（RUST_LOG + rustc） | ✅ | 6 | ✅ kind 与必填字段齐全、installed_version 与实测一致 | ✅ 标记值 0 命中 | ✅ 退出 0 | ✅ 首调 |

**约定遵守率：3/3（sidecar 层面）**。三个场景模型均把 `load_skill("assets")`
作为第一个工具调用，并严格按 skill 配方执行（sha256sum、curl 检查、
presence-only 检查）。

## url 场景任务失败的根因（非约定问题）

1. **shell 工具超时是死代码**（`src/tools/shell.rs` 解析 `timeout` 参数后丢弃，
   `wait_with_output` 无超时）——模型跑 `find /` 定位 lcode 二进制时在 WSL 的
   /mnt/c 上挂死十几分钟无法被中断。**已修复**：真实超时轮询（50ms 轮询 +
   超时 kill + 输出 drain 线程），新增测试 `test_shell_timeout_kills_hung_command`。
2. 测试环境 PATH 缺少 lcode——真实用户场景 lcode 在 PATH 上；skill 已补充
   "不在 PATH 时用绝对路径"提示。
3. skill 的 curl 配方（`-sI` 无 `-L`）对 doc.rust-lang.org 返回 302 而非 rubric
   期望的 200——skill 已改为 `-sIL` 并注明 2xx/3xx 均为可达。

## 附带修复

- `lcode assets check` 新增 sidecar `name` 与文件名一致性校验（env/tool 场景
  暴露模型照抄示例名，checker 原不校验）。
- shell 输出截断的 UTF-8 边界 panic 隐患（与 render 同类）一并修复。

## 判决

约定承载度适中：模型严格遵守，无需把校验器下沉为模型工具；E2E 暴露的两个
真实缺陷（shell 超时死代码、name 一致性）已修复并有回归测试固化。
