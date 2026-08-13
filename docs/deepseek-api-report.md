# DeepSeek API 全面测试报告

> 测试日期：2026-08-13 · 执行方式：7 个并行子 agent × 7 个维度，约 100 次真实 API 请求
> 模型：`deepseek-v4-flash` / `deepseek-v4-pro` · 端点：`https://api.deepseek.com`（OpenAI 格式）+ `/anthropic`（Anthropic 格式）

## 1. 调研结论（API 文档要点）

### 模型与定价

| | deepseek-v4-flash | deepseek-v4-pro |
|---|---|---|
| 上下文长度 | 1M tokens | 1M tokens |
| 最大输出 | 384K（max_tokens 合法区间 [1, 393216]） | 384K |
| 输入（缓存未命中） | $0.14 / 1M | $0.435 / 1M |
| 输出 | $0.28 / 1M | $0.87 / 1M |
| 输入（缓存命中） | $0.0028 / 1M | $0.003625 / 1M |
| Tool Calls / JSON Output | ✅ | ✅ |

### 关键参数

- `temperature` 0-2（默认 1）、`top_p`、`max_tokens`、`stop`（≤16 序列）
- `stream` + `stream_options.include_usage`（流式末块携带 usage）
- `response_format: json_object`（要求 prompt 含 "json" 字样，否则 400）
- `tools`（≤128 函数，实测 2048+ 仍接受）、`tool_choice`（auto/none/required/specific）
- `frequency_penalty` / `presence_penalty`、`logprobs` + `top_logprobs`（≤20）、`user_id`
- `thinking: {type: enabled|disabled}`（**默认 enabled**，思考模式开关）
- `prefix`（Beta，需 `https://api.deepseek.com/beta` 端点）

### Context Caching

默认自动启用；命中判定基于请求前缀；响应 usage 含 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`；best-effort，TTL 数小时到数天（实测 ≥25 分钟）。

### 错误码

400 格式/参数错 · 401 认证失败 · 402 余额不足 · 429 限流 · 500/503 服务端。实际参数错误统一以 400 返回（非 422），报文含合法取值范围。

## 2. 测试矩阵（7 维度）

| 维度 | 覆盖 | 结果 |
|------|------|------|
| 参数矩阵 | temperature 边界/确定性、max_tokens 截断、stop、penalties、logprobs、top_p、user_id | 3 major 发现 |
| 流式 SSE | 分块结构、include_usage、截断/stop、thinking 流式、UTF-8 边界 | 全 PASS |
| 新特性 | thinking 开关、prefix Beta、json_object | 全 PASS |
| Function Calling | tool_choice 全模式、多工具、多轮闭环、流式分片、超限 | 1 major 发现 |
| Context Caching | 构建/命中/部分命中/多轮/TTL/延迟 | 1 major 发现 |
| 双模型双端点 | flash vs pro、旧模型名兼容、OpenAI vs Anthropic | 2 major 发现 |
| 错误路径 | 401/400/402/415/404/405 实际报文、结构一致性、敏感信息 | 3 major 发现 |

## 3. [major] 发现

### M1. v4 推理模型的参数语义陷阱（4 个维度交叉确认）

- **默认 thinking 开启**：每个请求消耗隐藏 `reasoning_tokens`（16-99+），响应含 `reasoning_content` 字段
- **`max_tokens` = 推理+答案的总预算**：设 5/10/50 时全部被思考消耗 → `content` 为空字符串（`finish_reason: length`）
- **`temperature=0` 不具确定性**：两个模型同 prompt 两次调用结果均不同（长度波动达 2.5 倍）——回归比对/快照测试场景失效
- **`penalty` 参数对重复输出零效果**：freq/pres=2 与 0 的输出逐字符相同（推理阶段采样不受约束）
- **`tool_choice=required` / 指定函数模式在 thinking 下直接 400**（"Thinking mode does not support this tool_choice"）；需显式 `thinking: {type: disabled}`。`reasoning_effort: "none"` 也可禁用；顶层布尔 `thinking: false` 会反序列化报错

### M2. 旧模型名静默降级到 v4-flash

| 请求模型名 | 实际响应 model | 行为模式 |
|---|---|---|
| deepseek-v4-flash | deepseek-v4-flash | 推理模式（有 reasoning_content） |
| deepseek-v4-pro | deepseek-v4-pro | 推理模式 |
| deepseek-chat | **deepseek-v4-flash** | **纯 chat 模式**（无推理、prompt_tokens 少 79） |
| deepseek-reasoner | **deepseek-v4-flash** | 推理模式 |

旧名不报错而是别名映射；**要 pro 级质量必须显式 `deepseek-v4-pro`**。

### M3. 错误响应结构不统一

JSON `{"error": {message, type, param, code}}` 契约只在部分路径生效：

| 场景 | 状态 | body |
|------|------|------|
| 无效 key | 401 | JSON（回显 key 末 3 位） |
| 缺 Authorization 头 | 401 | **纯文本**，无 content-type |
| 畸形/空 JSON | 400 | **纯文本**（octet-stream） |
| 参数越界/非法模型名 | 400 | JSON（含合法范围/合法模型名，`param` 恒为 null） |
| 415 / 404 / 405 | — | 纯文本 / **空 body** |

JSON-only 解析器会在 6 条路径上失效，客户端必须做容错解析。

### M4. 缓存匹配不锚定前缀

文档宣称"前缀匹配"，实测近似"**最长公共连续段**"：
- 头部插入 20 字符不同内容后仍命中 2432 hit
- 尾部 80% 重叠的请求**首次调用 hit=0**，重试后才全命中
- 真正的部分命中（1920/511）只在缓存中存在更短的精确前缀条目时出现

结论：hit/miss 分布不可按前缀直觉预测；但内容无串味、计费算术自洽（`hit + miss = prompt_tokens` 恒成立）、TTL ≥25 分钟。

## 4. ✅ 确认良好

- 流式：SSE 结构标准（role 块 → delta 块 → finish_reason 块 → `[DONE]`）；`include_usage` 生效；UTF-8 码点级完整（中文/emoji 跨块无损坏）；stop 语义与 OpenAI 一致
- Function calling：多轮闭环正确；13 个 arguments 全部合法 JSON；流式分片按 `delta.tool_calls[index]` 累积后为合法 JSON
- JSON mode：强制合法输出；prompt 无 "json" 字样时正确 400 拒绝
- 双端点：延迟几乎无差异；Anthropic 流式 event 序列符合协议（thinking 块 + text 块分离）
- 缓存：连续命中稳定（2432/55）、多轮整段历史命中（2560/9）、完全不同前缀正确 miss

## 5. 对 LCode 的集成结论

| 发现 | LCode 影响 |
|------|-----------|
| 小 max_tokens 被推理预算吃光 | ✅ RetryProvider 已有对症逻辑（length → 提升 max_tokens 重试），恰好是正确策略 |
| tool_choice=required/指定模式报错 | ✅ LCode 只用 `tool_choice: auto`，不受影响 |
| deepseek-chat → flash 纯 chat | ✅ LCode E2E 全部使用 deepseek-chat，行为一致；文档注明 pro 需显式模型名 |
| thinking 默认注入 ~79 prompt tokens | ⚡ 可选增强：加 `thinking: disabled` 配置（prompt_tokens 29 vs 108，响应更快更直接） |
| 流式 usage 上报 | ✅ 已接入（`StreamEvent::Done` 携带 usage，见 minor backlog 修复） |
| Anthropic thinking 块 signature 伪造（UUID 而非签名） | ✅ LCode 不校验 signature，无影响 |
| 错误响应非 JSON 路径 | ⚡ 建议：LCode 的 provider 错误处理对纯文本/空 body 容错（当前 `response.text()` 已兼容） |

## 6. 复现方法

```bash
# 基础调用
curl -s --noproxy '*' https://api.deepseek.com/chat/completions \
  -H "Authorization: Bearer $DEEPSEEK_API_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'

# 流式 + usage
curl -sN --noproxy '*' https://api.deepseek.com/chat/completions \
  -H "Authorization: Bearer $DEEPSEEK_API_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","stream":true,"stream_options":{"include_usage":true},"messages":[{"role":"user","content":"Count to 5"}]}'

# 关闭思考
curl -s --noproxy '*' https://api.deepseek.com/chat/completions \
  -H "Authorization: Bearer $DEEPSEEK_API_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","thinking":{"type":"disabled"},"messages":[{"role":"user","content":"hi"}]}'

# 缓存命中观测：连续两次相同长前缀请求，对比 usage.prompt_cache_hit_tokens
```

> 本报告基于 2026-08-13 实测；DeepSeek 服务端行为可能随版本演进，重跑验证请以上述复现命令为准。
