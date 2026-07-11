# Codex HTTP 上下文运行时工程书

> 状态：第一阶段已实施并由本地 fake-upstream 回归测试覆盖。
> 范围：CodeSeeX 的标准 Codex `/v1/*` 代理兼容层。
> 已实施：Canonical Session Core 指纹对齐、权威 full replay 转发、工具组原子化、真实窗口受控拒绝。本文保留后续增强项，但不会以未实现设计描述当前行为。

## 1. 目标与边界

CodeSeeX 的目标是让 Codex CLI 在第三方 HTTP Responses 上游前仍能保持连续、可缓存、工具协议合法的会话运行。Codex App 只是同一 Codex runtime 的 GUI 消费方，不是本设计的适配目标。

本设计必须满足：

- 只通过公开的 Codex `/v1/*` 请求与响应工作。
- 不注入 Codex App renderer，不使用 CDP，不附着私有进程，不读取或修改 Codex 的 `jsonl` 会话文件。
- 不把 CodeSeeX 变成长期 transcript 存储。完整会话内容只能在代理进程的活跃内存中短暂存在。
- DeepSeek 的消息顺序、工具调用与工具结果必须始终合法；不能用静默截断换取“继续运行”。

## 2. 已验证事实

| 层级 | 事实 | 对设计的意义 |
| --- | --- | --- |
| Codex runtime | 官方 app-server 协议存在 `thread/start`、`thread/resume`、`thread/compact/start`、`thread/tokenUsage/updated` 等能力。 | Codex 自身有 thread 与压缩生命周期；代理不能假设这些内部句柄会透传到第三方 HTTP provider。 |
| Codex transport | 官方客户端仅在 provider 支持 WebSocket 时发送 `previous_response_id` 和 incremental input；HTTP Responses 回退构造完整 `ResponsesApiRequest`。 | 收到 Codex HTTP full replay 且没有 `previous_response_id` 是正常协议形态，不是 App 缺陷。 |
| DeepSeek | Chat Completions 是请求携带完整消息的接口；上下文缓存依赖稳定、完全匹配的既有前缀。 | 缓存不是隐藏会话。代理不能任意删掉旧前缀，再期待缓存与语义连续。 |
| 工具协议 | 一条 assistant tool-call 消息必须由全部匹配的 tool result 紧随其后。 | 去重、重放、预算与错误恢复必须以完整工具组为原子单位。 |

## 3. 已修复故障模型

旧 full replay 路径有两个会破坏连续性的本地策略，现已删除：

1. `local_prompt_cache_chain_current_tail` 在判断本地历史覆盖 client replay 前缀后，只选择当前 tail。DeepSeek 因而看不到稳定的完整历史前缀，导致上下文比例跳变、缓存重置和语义工作集缩小。
2. `CODEX_FULL_CONTEXT_REPLAY_BUDGET_TOKENS = 96_000` 对 Codex full replay 使用了独立预算，远小于模型声明的有效上下文预算。超过该值时，代理会压缩、删块或截断内容。

旧历史去重还会按单条 assistant tool-call 签名局部删除。当一批调用同时包含 CodeSeeX 内部工具和 Codex 客户端工具时，可能造成不完整工具序列并让上游返回 400。现在不再对 full replay 做局部去重；不完整工具组会整体降级为历史事实，绝不发送半个 tool-call 组。

这些行为都属于代理层根因，不应通过“短任务/长任务”特化策略掩盖。

## 4. 目标架构：Canonical Session Core

```text
Codex CLI full replay
        |
        v
request identity + structural validation
        |
        v
process-local Canonical Session Core
  - ordered message fingerprints only
  - atomic tool-group validation
  - replay alignment and checkpoints
        |
        v
stable append-only DeepSeek chat history
        |
        v
DeepSeek Chat Completions
```

### 4.1 会话身份与生命周期

- 会话锚点不能只使用 `prompt_cache_key`。它必须结合稳定 client metadata、首次 replay 的结构指纹和当前进程归属进行校验。
- 出现冲突、无法验证或跨进程缺失状态时，不得把新请求拼到旧会话；以本次 Codex full replay 重新建立会话。
- canonical session 仅存在进程内，当前 TTL 为 30 分钟、最多 64 个 session、每个 session 最多 16,384 条消息指纹。进程退出、TTL 到期或主动清理时删除状态。
- core 只保留 SHA-256 消息指纹和匿名 session hash，不保留 replay 正文、不写入磁盘、不读取 Codex JSONL。磁盘日志只能保留脱敏 hash、数量、大小和决策结果。

这份内存状态是代理的临时“发送清单”，不是 Codex 的会话档案。Codex 提供的新 full replay 永远是权威输入。

### 4.2 Full replay 对齐

对每个 HTTP Responses full replay：

1. 解析为有序规范 item 流，保留用户、assistant、reasoning、function/custom tool-call 和 tool result 的协议边界。
2. 用结构化指纹比较 incoming replay 与 active canonical session，不以文本尾部、单个 call id 或模糊子序列作为唯一依据。
3. 完整前缀匹配时记录追加；发送给 DeepSeek 的内容仍直接来自本轮 Codex full replay，原顺序和稳定前缀不被代理重写。
4. incoming replay 与 canonical 前缀分歧时，丢弃该活跃会话的临时清单，以 incoming replay 原子重建。不得降级为 tail-only continuation。
5. 代理重启后没有活跃清单时，同样从 incoming full replay 重建；不尝试从旧日志或 Codex 文件恢复。

`previous_response_id` 在 HTTP replay 中只能作为辅助关联信号，不能覆盖 Codex 当轮完整输入。

### 4.3 工具组不变量

工具组定义为：一条 assistant 消息中的全部 tool calls，加上每个 call id 对应的全部 tool result。任何操作都必须保持以下不变量：

- 组内 tool call 不可部分删除、部分重排或独立截断。
- 只有当一整个组和其全部结果已经在 canonical 前缀中存在时，才允许跳过 replay 中的同一完整组。
- 并行的内部工具、客户端工具和混合批次都属于同一 assistant 组。
- 若缺少任何 result，停止在协议边界并生成受控诊断；不得把残缺序列交给 DeepSeek。

## 5. 上下文、缓存与压缩

第一阶段由 Codex 负责语义压缩，CodeSeeX 负责保真转发与边界保护。

- 删除 full replay 专属 96k 语义预算；预算基于上游模型的真实有效窗口、输出预留和工具定义预留计算。
- 在真实上游预算内，完整保留经 canonical 对齐的历史。正常会话应持续增长，DeepSeek 缓存前缀持续命中。
- Codex 自身发送结构性压缩后的 full replay 时，将其识别为新的权威检查点，原 canonical session 事务性替换；这是一条允许缓存重新建立的明确边界。
- 若真实上游硬限制已无法容纳权威 replay，返回受控、可诊断的上下文限制结果；不静默总结、删旧消息或截断工具输出。
- 代理主动语义摘要不属于第一阶段。是否引入必须另立设计，证明其成本、事实保真和回退行为后才能实施。

## 6. 观测、安全与发布策略

诊断字段只允许包含：匿名会话 hash、replay item 数、对齐结果、追加/重建原因、原子工具组数量、输入大小估算、预算阈值和缓存风险标记。不得包含 prompt、assistant 正文、工具原文、密钥、Cookie 或 Authorization。

未来实现完成后：

- `canonical` 为默认运行时模式。
- 当前没有自动或隐式 legacy 回退。若未来加入显式紧急回退，必须不恢复 tail-only 或 96k 裁剪行为，并记录明显警告。
- 已通过脱敏诊断和 fake upstream 验证：连续增长、单次权威重建、工具组合法性和缓存前缀稳定性。

## 7. 零成本虚拟上游

仓库测试已提供 DeepSeek-compatible `/chat/completions` fake upstream。该服务不接受真实凭据，也不请求外部模型。

需要手工观察真实代理出站形状时，可在仓库根目录运行：

```powershell
cargo run -p codeseex-proxy --example context_smoke_upstream
```

它默认监听 `127.0.0.1:8892`，只将脱敏 trace 写入 `.private/context-smoke-upstream-payload.json`。trace 仅包含模型、消息角色顺序、工具名、大小、估算 token 和 payload hash，不保存 prompt、工具输出或 Authorization；可用 `FAKE_UPSTREAM_PORT`、`PAYLOAD_FILE` 和 `BALANCE_FILE` 环境变量隔离并行试验。

它必须：

- 捕获脱敏后的 outbound payload 形状、消息角色顺序、工具组、哈希、消息数和估算 token。
- 可脚本化返回普通回复、内部工具、客户端工具、并行混合工具、上游错误、重试、模型切换和权威压缩 replay。
- 支持断言 cache-prefix 是否连续，而不是仅断言请求成功。

必须覆盖：

| 场景 | 断言 |
| --- | --- |
| 连续 full replay | 上下文在压缩前单调增长；不存在任意 tail reset 或 96k 截断。 |
| 稳定前缀 | 相邻 DeepSeek 请求共享相同前缀；仅 Codex 权威压缩或明确分歧允许重建。 |
| 混合工具批次 | assistant tool calls 与所有 result 完整成组，上游不会出现缺少 tool message 的 400。 |
| 重启与过期 | 清空内存后仅依赖下一份 full replay 重建，不读取磁盘 transcript。 |
| 分歧 replay | incoming full replay 覆盖旧临时清单，不拼接无关历史。 |
| 安全 | fake 记录和诊断没有 key、完整 prompt 或完整工具输出。 |

## 8. 实施状态

1. 已完成：fake upstream、sanitize fixture 与协议断言，覆盖单调增长、工具配对、模型切换和超限拒绝。
2. 已完成：进程内 canonical 指纹状态替换 tail-only replay；不提供自动回退。
3. 已完成：full replay 不再使用 96k 专用预算；超过真实上游窗口时以 `context_limit_exceeded` 拒绝，不静默截断。
4. 已完成：不完整工具组不再拆分发送；已收到结果以历史事实保留。
5. 已完成：`architecture.md`、`state-contract.md` 与 README 的上下文说明同步为当前实现。

## 9. 非目标

- 不模拟或接管 Codex app-server 的私有 thread 状态。
- 不新增 App renderer 模型列表注入、CDP 调试依赖或系统进程操作。
- 不将 CodeSeeX 变为跨重启的对话存档、记忆系统或网页档案库。
- 不以“短任务/长任务”标签改变上下文正确性策略。

## 10. 参考资料

- [OpenAI Codex app-server protocol schema](https://github.com/openai/codex/tree/main/codex-rs/app-server-protocol/schema/json/v2)
- [OpenAI Codex Responses client transport](https://github.com/openai/codex/blob/main/codex-rs/core/src/client.rs)
- [DeepSeek Context Caching](https://api-docs.deepseek.com/guides/kv_cache)
- [DeepSeek Function Calling](https://api-docs.deepseek.com/guides/function_calling)
- [CCSwitch Codex chat history bridge](https://github.com/farion1231/cc-switch/blob/main/src-tauri/src/proxy/providers/codex_chat_history.rs)
- [CCX Responses stream session](https://github.com/BenedictKing/ccx/blob/main/backend-go/internal/handlers/responses/stream_session.go)
