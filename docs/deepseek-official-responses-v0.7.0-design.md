# CodeSeeX 0.7.0：DeepSeek 官方 Responses API 工程书

> 状态：实现完成，进入发布验证。原生 production route、RAM-only client-tool coordinator、配置持久化、UI 选项与 Issue #17 修复已接入，并通过 fake upstream、Codex CLI fake-responses 和官方 Flash/Pro 小额验证。发布策略以官方全模型 Responses 为默认路径；Chat API 仅作为实验性、用户主动选择的回退。
>
> 范围：CodeSeeX 的官方 DeepSeek 上游路径。Codex CLI 是兼容目标；Codex App 只是同一 runtime 的 GUI，不增加 renderer/CDP/注入依赖。

## 1. 背景与结论

DeepSeek 已在官方 `https://api.deepseek.com/responses` 提供 Responses API，并明确面向 Codex 场景。它降低了 CodeSeeX 在官方上游上执行 `Responses -> Chat Completions -> Responses` 协议转换的必要性。

2026-07-31 的脱敏真实直连基线已验证：

- full replay 的上下文标记正确保留，三次输入单调增长，缓存命中为 89.51%–94.71%；
- SSE 使用带严格递增 `sequence_number` 的语义事件，以 `response.completed` 结束，没有 `[DONE]`；
- `function` 与 `custom/apply_patch` 在自动工具选择下均能正确返回调用；
- 思考模式拒绝强制 `tool_choice`，返回 HTTP 400；
- 官方 server-side `web_search` 能工作，但一个只要求页面标题的案例仍产生 4,647 total tokens、两个 search call（一个失败），不能假设其成本或重试数可预测。

这不意味着 CodeSeeX 应被删除。0.7.0 的目标是让官方路径优先使用原生 Responses 语义，同时保留 CodeSeeX 的本地安全边界、上下文保真、工具归属、使用量、日志、配置和兼容上游价值。

### 1.1 2026-07-31 追加实测：必须以此约束实现边界

以下测试均使用隔离的 Codex CLI 0.146.0、临时 `CODEX_HOME` 和官方 DeepSeek endpoint；不经过 CodeSeeX 生产路由，不读取用户 Codex `jsonl`，测试产生的临时凭据配置在结束时清除。仅保留脱敏的类型、计数、哈希、状态和耗时。

| 项目 | 结果 | 对 0.7.0 的约束 |
| --- | --- | --- |
| Flash 原生 Responses SSE | 通过。`response.completed` 收尾，无 `[DONE]`；27 个 event sequence 严格递增；reasoning/output 分别作为 item/part 传输。 | 原生路径必须逐事件转发，不得重新包装为 Chat SSE 或 `[DONE]`。 |
| Flash `function_call` / `custom_tool_call(apply_patch)` 与完整 output replay | 通过官方真实请求验证。 | 已有官方能力直接透传；CodeSeeX 不模拟工具调用、不拆工具组。 |
| Codex CLI -> 官方 `web_search` | 通过。CLI 事件中出现 `web_search` 后正常完成。 | 保留独立的 `official` backend；它是 provider-owned，不应被本地搜索重复执行。 |
| CodeSeeX 本地 `web_search` | 本轮没有删除或替换；当前配置默认仍为 `local`，且现有执行路径仍属本地工具。 | 必须保留 `local` / `official` 两种显式选择；禁止静默 fallback 或双发。 |
| `deepseek-v4-pro` 原生 Responses / Codex | 早期真实请求曾返回 HTTP 400，属于官方分阶段开放期间的历史观测。 | 0.7.0 按未来官方全模型 Responses 支持作为产品默认；如上游暂时不兼容，用户可在实验性页主动切换 Chat API，不做静默自动回退。 |
| Codex CLI `exec resume` -> Flash | 两回合均 exit 0，但第二回合没有复述仅在第一回合给出的 nonce。 | 这是待定位现象，**不能**据此断定 CodeSeeX 应重写上下文，也不能断定 provider 不接受 full replay。 |
| fake Responses 对 resume / tool handoff payload 的可观测 | 已建立。隔离的 Codex CLI 0.146.0 在官方 Flash catalog 形状下完成双回合 resume；第二请求从 5 个增长到 8 个 input item，保留 assistant / reasoning，且没有 `previous_response_id`。混合 `apply_patch` + `shell_command` 时，后续请求完整带回两个 call 和两个 matching output。 | HTTP full replay 与完整工具组是可复现的 CLI wire contract；native coordinator 不得自行切为 tail 或 partial continuation。 |

2026-08-01 复核：官方 Responses 文档页 HTTP 200，页面仍包含 `web_search`、`web_search_2025_08_26`、`function_call_output`、`custom_tool_call` 和 `response.completed` 的协议说明。对 Flash 的最小真实 SSE probe 收到 46 个严格递增 sequence，包含独立的 reasoning/output item 与 `response.completed`；没有 `[DONE]`。同日对 Pro 的同等最小请求仍得到官方 HTTP 400；这是分阶段开放期间的历史观测，仅保留为兼容回退诊断背景，**不再**作为发布版的模型路由条件。2026-08-22 新一轮官方 Pro 对照已全部成功：full replay marker 保留、SSE sequence 严格递增、function/custom apply_patch/official web_search 均完成。

同日将 CLI fake resume probe 固化为零模型费用模式：临时 isolated `CODEX_HOME`、本地回环 HTTP fake、脱敏 request hash / item count / role count / tool type / nonce-presence，结束后清空临时配置。fixture 显式设置 `[features] plugins = false`，避免与本测试无关的 Codex 插件同步创建后台 Git 进程。官方 Flash catalog 形状下，CLI 两回合均 exit 0；第二个 `/responses` 请求没有 `previous_response_id`，从 5 个 input item 增长为 8 个，保留了 assistant / reasoning 历史并能回显首轮 nonce。混合工具场景中，真实 CLI 完成 `apply_patch` 后再执行 `shell_command`，第二请求同时包含两个原始 call item 与各自 output。测试只保存结构、哈希、长度和状态，不保存 request 正文、密钥或用户 Codex 数据。

官方资料与此次实测共同确认：Responses endpoint 目前是无状态的；`previous_response_id`、`conversation`、`store`、`prompt_cache_key` 等不应被当成服务端续接能力。CodeSeeX 的职责是保留 Codex 提供的权威 `input` 和协议顺序，不是构造隐藏历史或替 Codex 语义压缩。

## 2. 目标

1. 为规范化官方 DeepSeek endpoint 的所有模型提供同一条原生 Responses 协议路径；0.7.0 按官方全面支持的发布时状态设计，不能把历史 Flash/Pro 观测散落为模型特化或路由门槛。
2. 对官方 Responses 上游，优先保持 Codex 请求/响应/SSE 的原始 Responses 语义，消除不必要的 Chat 转换、DSML 解析和 SSE 重组。
3. 提供明确的 Web Search 后端选择：`本地 CodeSeeX` 或 `DeepSeek 官方`，不双重执行，不静默回退。
4. 保持 0.6.0 的上下文正确性底线：Codex HTTP full replay 为权威输入；无 tail-only continuation、无代理专属 96k 截断、无拆分工具组。
5. 将成本、缓存、工具来源和失败状态以不泄露正文的方式呈现在 Usage/Logs。
6. 保留 Chat Completions 适配路径，服务于自定义兼容上游、用户主动选择的实验性回退，以及在 native local-tool coordinator 完成前必须使用既有本地工具生命周期的请求。

## 3. 非目标

- 不把历史阶段性模型可用性当作发布版的自动路由门槛：规范化官方 endpoint 的 `auto` 必须默认原生 Responses。协议以外的具体 feature（如文件输入、强制工具选择）仍须按未知能力处理，不能凭空模拟。
- 不读取或写入 Codex `jsonl`，不接管 Codex thread，不以本地 transcript 模拟 `previous_response_id`。
- 不因原生路径而删除 Canonical Session Core、工具组校验、工具输出边界或安全日志。
- 不为了让调用“继续成功”而静默修改 full replay、自动改写思考模式、自动关闭工具或切换模型。
- 不让官方 Web Search 失败时偷偷改用本地搜索；这会改变隐私、成本、来源和可重复性。
- 不把第三方 OpenAI-compatible endpoint 误判为官方 DeepSeek Responses API。

## 4. 必须区分的三层能力

“模型名称可选”不等于“所有 feature 都完整可用”。0.7.0 将官方模型的原生 Responses 路由视为统一默认，但仍必须将下列信息分开：

| 层级 | 作用 | 例子 |
| --- | --- | --- |
| 协议实现 | CodeSeeX 是否能按原生 Responses 传输 | SSE 事件、function call、`custom/apply_patch` |
| provider 能力 | 当前 endpoint 是否公开支持某项能力 | 规范化官方 DeepSeek endpoint 的模型默认使用原生 Responses；自定义 endpoint 不自动假定兼容 |
| 当前请求条件 | 本次参数组合是否被允许 | Thinking mode + 强制 `tool_choice` 在真实测试中被拒绝 |

因此内部模型不能使用 `if model == flash` 作为协议分支。应建立可扩展、数据驱动的 `UpstreamCapabilities`：

- `transport`: `auto`（官方默认原生）、`native_responses`（仅 TOML/env 的显式原生）、`chat_compat`（实验性回退）；
- `model_native_responses`: 规范化官方 endpoint 默认 / 自定义 endpoint 未验证 / 用户显式 Chat 回退；
- `streaming_responses`、`function_tools`、`custom_apply_patch`、`official_web_search`；
- `thinking_forced_tool_choice`: 允许 / 不允许 / 未知；
- `input_image`、`file_input`、`previous_response_id`、`conversation`、`store`、`truncation` 等字段支持状态。

能力状态的来源必须可说明：内置且经发布验证、用户显式验证的结果，或 provider 返回的确定错误。不得在后台为“探测能力”发起付费 completion，也不得抓取网页后悄悄改变正在进行的会话。

能力状态必须以“规范化 endpoint + transport + model + CodeSeeX 版本/验证版本”为作用域，并带有验证时间。切换 endpoint、模型、transport 或进行显式重新验证后，旧状态不能继续作为自动路由依据；不同凭据配置也不得互相泄露验证或诊断数据。

## 5. 目标架构

```text
Codex CLI / HTTP Responses full replay
             |
             v
local capability-token guard + request classification
             |
             +--> official native Responses transport
             |       - preserve authoritative input order
             |       - forward upstream SSE semantics
             |       - ownership-only handling for configured local tools
             |
             +--> Chat compatibility transport
                     - current conversion/DSML/tool adapter path
                     - when users explicitly select experimental compatibility, for custom endpoints in Auto mode,
                       or when an existing CodeSeeX-owned local tool requires the mature local lifecycle
```

### 5.1 原生路径的允许变换

原生路径不是“盲目字节转发”，但变换必须少而可列举：

- 本机 capability token、上游 Authorization、模型 alias、超时和安全的请求头处理；
- 对仅供 Codex/CodeSeeX 使用且上游明确不支持的字段做受控筛除或保留诊断；
- 配置决定的本地工具所有权注入/回收；
- 安全脱敏的 Usage/Logs 记录；
- 对不完整工具组返回可诊断协议错误。

下列行为禁止发生：

- 把 full replay 改写为本地 tail；
- Chat message 重排、DSML 文字解析、reasoning 文本拼接或重新生成 SSE sequence；
- 将 upstream 的 `response.completed` 重新包装成伪造的 `[DONE]`；
- 以重试名义重复调用官方工具或重复计费。

### 5.2 官方 endpoint 识别与字段策略

原生官方模式的 endpoint 识别必须精确、可测试：

- 只接受规范化后确认为官方 host 与受支持 scheme 的 base URL；不得用 `contains("deepseek")` 等模糊规则，也不得把用户自建反向代理自动当作官方能力。
- URL 必须通过 URL resolver 生成 `/responses`，禁止字符串拼接造成 `/v1/responses`、双斜杠或丢失路径。最终官方请求 URL、选择的 transport 和原因应出现在脱敏诊断中。
- Custom/third-party endpoint 默认保持 Chat compat，除非用户明确选择并验证 native Responses；这避免把“OpenAI-compatible”误写成“DeepSeek 官方支持”。
- 对请求字段采用显式兼容表：已验证字段保真转发；已知不支持字段不伪造功能；未知字段的处理必须在实现时列明、测试并记录。不得为了让请求成功而静默删掉可能影响 Codex 语义的字段。
- 输出中的未知 item/字段必须保留为安全的协议事实或受控错误，不能被当作可展示正文，也不能在日志中落入原始 JSON。

### 5.2 会话、缓存与模型切换

官方 Responses 当前仍是无状态接口：返回 response id 不代表可用的服务端续接；`previous_response_id`、`conversation`、`store`、`prompt_cache_key` 等不能被当作隐藏记忆使用。DeepSeek 缓存由稳定、完整匹配的前缀单元自动管理。

因此：

1. full replay 仍是请求权威；Canonical Session Core 只存匿名结构指纹，继续用于对齐和诊断。
2. 若客户端只发送 `previous_response_id` 而没有足够的权威 `input`，CodeSeeX 不得从磁盘或旧日志重构；应给出受控的 `context_required` 诊断。
3. 官方原生、Chat compat、不同 endpoint 及不同模型之间不得隐式拼接隐藏历史。模式或模型切换必须依赖 Codex 提供的新 full replay，允许缓存自然重新建立。
4. 接近真实上游上下文限制时，返回受控 limit 结果；不代理摘要、不静默删工具结果。Codex 自身的压缩 replay 是新的权威检查点。

## 6. 流式与非流式协议要求

原生 upstream 的 event 名、output item 类型和 `sequence_number` 是协议事实。

- 流式必须保留事件顺序和递增 sequence；完成、截断和失败分别以 `response.completed`、`response.incomplete`、`response.failed` 表达。
- 不得混入 `[DONE]`，不得将某个 upstream `usage` snapshot 重复累计。
- Usage 只以本次 upstream iteration 的最终 usage 为准；映射 `input_tokens_details.cached_tokens`、cache miss、输出和 reasoning tokens。
- 非流式和流式的 response item、tool-call 和 usage 口径必须一致。
- 本地诊断走 Logs/Usage side channel，不插入模型可见正文，也不能破坏 upstream sequence。

### 6.1 取消、重试与超时

- 一旦上游已接受流式请求或已经看到任一输出事件，CodeSeeX 不得自动重发同一 completion；这可能重复产生 reasoning、官方搜索、工具调用和费用。
- 客户端断开时，应取消对应 upstream stream、标记 turn 为 cancelled/aborted，并且不将不完整 usage 当作 completed 账单。
- 只允许在“上游尚未接受请求且没有可见输出”的明确网络失败窗口内进行有限重试；重试次数、原因与 idempotency 证据必须进入安全诊断。
- `429`、provider timeout、非 JSON 错误体和 SSE 半帧必须映射为明确、可翻译的错误类别；不得将 HTTP 成功但 `response.failed` 的流误记为成功。

## 7. 工具策略

### 7.1 工具协议底线

assistant 的全部 tool calls 与相应的所有 tool outputs 是一个原子组。无论原生还是 Chat compat，都不得部分删除、单独去重、重排或截断。

原生 `custom/apply_patch` 的正常结果应原样交还 Codex。既有的空白上下文行微修复只可在**明确 parser 失败**、仅包含可证明裸空白 hunk 行时触发；正确的 patch 不应被第二次改写，模糊错误不做代理修正。

对 Thinking mode 下的强制 `tool_choice`：

- 不自动关闭 thinking，也不默默把 required/指定工具改为 auto；
- UI/Logs 给出明确、可翻译的能力诊断；
- 若 Codex 正常运行路径确实依赖强制选择，必须先完成端到端兼容测试，不能仅凭独立 API 成功就升为默认。

### 7.2 Web Search 后端配置

新增一个独立于网络代理设置的枚举配置，暂定名称：

```toml
[tools.web_search]
backend = "local" # local | official
```

最终字段名和 TOML schema 在实现前确认；不得复用已经表示“网络代理模式”的旧字段。

| 选项 | UI 文案建议 | 工作方式 | 优点 | 明确代价/限制 |
| --- | --- | --- | --- | --- |
| `local` | CodeSeeX 本地搜索（推荐） | CodeSeeX 请求已配置的公开搜索源，规范化并有界返回结果 | 可见 source health、可控输出长度、可走本机网络策略、结果来源较明确 | 依赖本机网络/搜索源可达性；模型仍会消耗阅读结果的 token |
| `official` | DeepSeek 官方搜索 | 将原生 `web_search` 工具交给 DeepSeek 服务端 | 无需本地抓取；可能利用上游自身检索能力 | 结果/重试步骤不完全由本机控制；额外推理与检索上下文会计入 token；数据会发送给 DeepSeek；真实测试已观察到多次 search call 和失败状态 |

强制规则：

- 默认建议为 `local`，因为成本、网络来源和失败行为更可观测；这是一项待发布确认的产品决定。
- `official` 仅在当前请求走官方原生 Responses 时可用；用户显式选择 `chat_compat` 或使用 custom endpoint 时，声明官方 Web Search 的请求会得到明确错误。
- 若当前请求走 custom endpoint 或用户明确选定的 `chat_compat`，而工具表仍声明 `web_search` / `web_search_preview` / native `web_search`，CodeSeeX 返回明确的 `400 official_web_search_incompatible`。即使请求没有工具表，Chat compatibility 也不再自动注入本地 `web_search`；因此不会有“未声明时偷偷本地执行”的旁路。它绝不会把这个显式的官方选择偷换成本地搜索；用户可明确改选 `local` 后继续使用既有本地搜索。
- 工具定义不是唯一防线：若不受信任/custom Chat upstream 无视工具表，仍自行返回 `web_search`，执行器也会在执行前拒绝它。这样官方模式不会因为上游的未声明 call 而触发本地网络访问。
- 选择 `official` 时，不得同时把 CodeSeeX 本地 `web_search` 作为同名可调用工具注入；必须只有一个执行所有者。
- 选择 `local` 时，不能让 upstream 再执行同一用户意图的官方 web search。若 Codex 请求中包含 provider-native web search item，必须在协议边界显式映射或拒绝，不能双发。
- 官方工具失败时保留 upstream 状态；除非用户未来显式启用“允许降级到本地”的单独开关，否则不回退。
- 选择变更只影响后续 request chain；正在进行的 turn 保持原选择，防止同一工具组跨后端。

### 7.3 其它官方工具

官方文档已明确 `function`、`web_search` 和 `custom/apply_patch` 的边界，但并不代表 file search、computer use、MCP 等所有 Responses 内置工具都可用。未被能力表确认的类型必须：

1. 不宣称支持；
2. 不伪装为本地执行成功；
3. 通过安全诊断标明“provider ignored”或确定错误；
4. 保持 Codex client-owned tool 结果的完整回传路径。

## 8. Usage、Logs 与成本口径

0.7.0 不改变实际 provider 计费，只改进来源可见性与准确归属。

- 每个 usage segment 增加安全的 `transport`（native/chat compat）和 `web_search_backend`（local/official）标识。
- `official` 搜索的 token 一律采用 upstream 最终 usage；不根据 search call 数虚构独立费用，也不漏算模型为搜索生成的 reasoning/output。
- Logs 仅显示风险信号：官方搜索调用数、成功/失败状态、累计 token、缓存比例、重试/截断标志；不记录搜索 query、正文、完整来源或工具原文。
- Usage 仍以用户任务聚合；不能因 SSE `response.completed` 与同轮普通完成事件而重复生成账单行。
- 费用估算读取版本化费率配置。官方价格页存在峰谷说明，但最终展示应以用户开启的估算策略为准，并标为“估算”，不是账单真值。

真实基线提示：一个仅要求搜索官方文档标题的官方搜索请求使用 4,378 输入 token（其中 2,944 cached）、269 output token，估算正常时段约 CNY 0.002031。此数值不能外推为固定单次价格，但足以证明 UI 不应把官方搜索描述为“零额外成本”。

## 9. 配置、迁移与回退

建议将“传输偏好”和“Web 后端”拆开：

```text
transport = auto | native_responses | chat_compat
web_search.backend = local | official
```

语义：

- `native_responses`：保留给 TOML/环境变量的强制原生选项；设置页以默认 Responses 状态呈现，但保存其它设置时必须保留该值。custom endpoint 不兼容时返回可读错误，不隐式改走 Chat。
- `chat_compat`：实验性兼容路径，供第三方 upstream、问题定位与上游协议回退使用。
- `auto`：默认策略。所有规范化的官方 DeepSeek endpoint 与模型优先原生 Responses；custom endpoint 保留 Chat compatibility。其判定结果必须记录在安全诊断中。

迁移要求：

- 新安装和升级用户在未显式选择 `chat_compat` 时均采用 `auto`，即官方 Responses 默认路径。
- 已明确设置 `chat_compat` 的用户配置必须保持不变。
- 配置保存继续采用 revision/CAS 或 dirty-field patch；UI autosave 不得覆盖托盘或外部改动。
- 任何不兼容的设置组合在保存前给出内联解释，不用阻塞弹窗打断普通输入。
- 回退由用户或显式兼容策略选择；回退本身不能恢复旧 tail-only/96k 截断行为。

配置页、Usage、Logs 和错误提示必须使用现有 translation key 机制；缺失翻译回退到英文，而不能把 provider 原始错误、内部字段名或中文硬编码散落在第三方语言包界面。当前 active transport、模型能力状态、Web 后端与最后验证时间应可见，但不显示 endpoint 凭据、完整 query 或模型正文。

## 10. 安全与隐私

- `/v1/*` 继续使用本机 capability token 和 Origin/Fetch-Metadata 保护；原生路径不削弱本地访问边界。
- API key 仅来自 secret store/进程环境；不进入 API、UI state、日志、TOML、deep link、测试报告或 crash 输出。
- `official` Web Search 表示 query 与必要上下文会发送至 DeepSeek；UI 必须直接说明，而不能隐藏在普通“联网”描述里。
- `local` Web Search 继续执行 SSRF/private-network 防护、输出大小上限、二进制/敏感内容脱敏与来源健康检查。
- 原生 SSE 与错误正文也必须经安全分类，避免上游回显 Authorization、prompt 或工具正文后进入日志。

## 11. 实施前必须完成的测试矩阵

### 11.1 零成本测试

新增一个 DeepSeek-native Responses fake upstream，能返回真实事件序列和可控 capability matrix，不使用任何 API key。至少覆盖：

| 场景 | 关键断言 |
| --- | --- |
| 普通 full replay | input 顺序、item 类型和稳定前缀不被改变 |
| Codex 压缩 replay | 作为新权威 checkpoint，不拼旧 tail |
| stream/non-stream | output items、usage、终止状态语义一致 |
| 并行 function + custom 工具 | 全部 call/result 成组，绝不产生缺失 tool message 400 |
| 原生 apply_patch | 已正确 patch 原样通过；仅确定裸空白 hunk 错误允许微修复 |
| forced tool choice + thinking | 得到明确能力错误，不自动改变模型参数 |
| official/local web 选择 | 只出现一个后端；失败不隐式双发或回退 |
| usage snapshots | 同轮 final/replace，不重复累计 |
| 失败/超时/断流 | 正确 `response.failed` 或受控本地错误，不伪造 completed |
| endpoint 识别 | 官方 URL 仅生成一个正确 `/responses` 路径；自定义 proxy 不会被误判为官方 |
| native 未知字段 | 不静默丢弃有语义的请求字段；未知 output item 不污染正文或日志 |
| client cancel / SSE 半帧 | 取消 upstream、无二次 completion、无 completed 账单或重复工具组 |
| model catalog | Flash/Pro 与未来模型的 transport 能力来自矩阵；模型展示/选择不被协议迁移破坏 |
| 配置与语言 | autosave 冲突安全；新增字段有 i18n key，缺失翻译回退英文 |

### 11.2 小额真实验证

真实验证必须使用私有、脱敏 harness，默认预算很小并可中止。以同一稳定 full replay 先后运行 Chat compat 与 Native 的配对测试，比较：

- 是否保留完整上下文标记；
- input、cached、cache miss、output、reasoning token；
- 端到端延迟与 SSE 事件合法性；
- `function`、`apply_patch`、local web、official web 各自的工具所有权与工具组闭环；
- 模型切换、Codex 压缩、代理重启后 full replay、超过真实窗口；
- usage/logs 是否无重复账单、无明文敏感数据。

真实测试报告只存 hash、数量、token、延迟、状态和安全类别。不得保存 API key、完整 prompt、模型正文、完整工具输入/输出或 Codex JSONL。

## 12. 发布门槛与回滚

0.7.0 不应仅因“单个 API 调用成功”发布。至少满足：

1. `cargo fmt`、clippy、全 workspace 测试及前端静态检查通过；
2. fake native upstream 覆盖上述矩阵，且上下文/工具/usage 回归不低于 0.6.0；
3. 在官方可用模型上完成小额真实成对测试；未支持模型维持明确 Chat fallback；
4. 原生路径不会读取 Codex 本地文件，不引入 App-specific 注入；
5. UI、README、故障排除、CHANGELOG 和官方网站同时说明模型能力、Web 后端、成本及回退方式；
6. 出现上游协议回归时，用户能切换到 `chat_compat`，但回退不破坏现有 session 的 full replay 正确性。

## 13. 已确认产品决策

以下决策已由本轮产品要求冻结，并作为 0.7.0 发布口径：

1. `local` 是 Web Search 默认后端；`official` 必须由用户显式选择。两者不双发、不静默互换。
2. 官方 Web Search 失败时不自动回退本地搜索；隐私、来源、成本和可重复性必须保持可解释。
3. 官方 endpoint 的 `auto` 默认原生 Responses；Chat API 仅作为实验性、用户主动选择的兼容路径。原生失败不静默回退。
4. 第三方/自定义 OpenAI-compatible endpoint 在 `auto` 下继续使用 Chat compatibility；原生强制选项保留给高级 TOML/env，并在不兼容时明确失败。
5. 原生路径对已知允许字段尽量透明转发；未知或未验证的语义能力不由 CodeSeeX 猜测、改写或伪造，按明确 provider 结果或受控错误处理。

## 14. 参考

- [DeepSeek Responses API](https://api-docs.deepseek.com/zh-cn/guides/responses_api)
- [DeepSeek Context Caching](https://api-docs.deepseek.com/zh-cn/guides/kv_cache)
- [DeepSeek 模型与价格](https://api-docs.deepseek.com/zh-cn/quick_start/pricing)
- [现有 Codex HTTP 上下文运行时工程书](codex-context-runtime-design.md)
- [现有架构说明](architecture.md)

## 15. 2026-08-01 当前代码映射与实施状态

本节最初依据 `main` 的 `80eed2d release: 0.6.0` 只读核对，随后按当前工作树更新。它记录的是 0.7.0 已实现与仍受限的边界，不代表已发布版本。

### 15.1 现有请求路径

Codex 已通过 Responses 协议访问 CodeSeeX：catalog 为模型声明 `wire_api = "responses"`，客户端入口为本地 `/v1/responses`。因此 0.7.0 **不需要**为了官方上游而重做 Codex catalog、模型暴露或任何 Codex App 注入。

当前 `crates/proxy/src/server.rs` 的 `responses(...)` 会在 Chat compiler 之前调用 `native_runtime::dispatch_if_selected(...)`。所有「严格官方 DeepSeek endpoint + transport=auto/native_responses」请求进入原生 route；`chat_compat` 与 Auto 下的 custom endpoint 进入既有 Chat compatibility lifecycle。native route 先完成 response-id 唯一性和 request checkpoint，再把权威 `input` array 原样交给 native payload builder；它不调用 `build_response_context(...)`，不重建 Chat messages。

因此现有路径是：

```text
Codex Responses request
  -> transport eligibility
  -> native route (official model) -> DeepSeek /responses, verbatim item replay
  -> or Chat compatibility -> Responses item -> ChatMessage compiler -> /chat/completions
```

Chat compatibility 的非流式路径仍通过 `complete_chat_with_tools(...)` 执行 CodeSeeX-owned tools 或交还 client-owned tools，再合成 Responses output；其流式路径继续合成 sequence 与 `[DONE]`。native transport 不复用这些转换：它只去除本地 `id` / `previous_response_id`，映射已验证的 tool schema，并保留 provider SSE 的顺序、sequence 与终止事件；它不追加 `[DONE]`。

### 15.2 可复用边界与必须隔离的部分

| 当前组件 | 0.7.0 处理 | 理由 |
| --- | --- | --- |
| `/v1` capability-token、Origin / Fetch-Metadata guard | 直接复用 | 与上游 wire protocol 无关，且是本地安全边界。 |
| `resolve_authorization_header(...)` 与 timeout/client 创建 | 抽取后复用 | Authorization 选择规则相同；native client 只应换 endpoint 与 Accept/流处理。 |
| response id 唯一性、checkpoint、取消状态、Usage/Logs 安全记录 | 复用生命周期语义，native 单独接线 | 本地任务归属与可观测性仍需要；不得直接假定上游 response id 等于本地 request id。 |
| Canonical full replay 判定和内存 canonical-session 诊断 | 复用为**只读判定/诊断** | Codex full replay 仍是权威；不能把其编译后的 Chat messages 当成 native request body。 |
| `build_response_context(...)` 和 `compile_responses_input_with_tool_outputs(...)` | 仅 Chat compatibility 使用 | 它们会把 item 改写为 Chat messages、工具 message 和文本；native 需保存原始 item 顺序及字段。 |
| `normalize_chat_payload(...)`、DSML adapter、Chat conversion | 保持 Chat-only | 原生 Responses 不应解析 DSML、重写 reasoning，或由代理伪造 Responses SSE。 |
| `complete_chat_with_tools(...)` | 不直接复用；提炼 tool ownership / bounded executor 后由 native coordinator 调用 | 其 payload 续接和 usage 合并假设 Chat `messages`，直接复用会重新引入协议转换。 |
| `response_stream_from_chat(...)` | 保持 Chat-only | 它合成 `response.created` 等事件、sequence 和 `[DONE]`；native 必须保留 upstream 事件顺序，不得附加 `[DONE]`。 |
| `response_usage_from_chat_usage(...)` / `merge_response_usage(...)` | 可复用数值归一化，新增 native final-usage adapter 测试 | 当前字段读取已兼容 Responses 常见字段，但 native 不可把 SSE snapshot 逐块相加。 |
| `tools::ownership`、本地工具安全执行器、apply_patch 微修复 | 复用所有权与执行安全规则 | 所有权、大小限制和“明确 parser 错误才修空白上下文行”的底线不取决于上游 transport。 |

`upstream::post_responses(...)` 与 `responses_url(...)` 已单独实现并有 URL、auth/header、payload 精确性测试。官方 endpoint 固定解析为 `https://api.deepseek.com/responses`；custom endpoint 虽可正确构造 `/responses` URL，但不属于已验证 native capability，`auto` 会保留 Chat compatibility。`official_v1_compat` 仍只控制 Chat endpoint，不是 native transport 开关。

### 15.3 已验证的上下文与工具底线

当前 `build_response_context(...)` 已不使用历史 tail 覆盖 Codex full replay：检测到 Codex full context 且不存在显式 `previous_response_id` 时，策略是 `canonical_authoritative_replay`，跳过 local history，并把当前 replay 完整交给 Chat compatibility 适配层。预算模块对这种 `AuthoritativeReplay` 只会拒绝超出计算上游窗口的请求，不会裁剪旧消息；标准 Chat history 路径才允许有界压缩。

当前 Chat replay 有一套明确的 tool-group 保护：assistant tool calls 与全部对应 tool outputs 作为连续块持久化和预算；缺少 outputs 的组不会被单独重放。它是避免 `tool_calls` 后缺失 tool message 400 的有效现有保障。native 实现必须建立同等的**Responses item 原子组**检查，而不能把这套 ChatMessage 结果再转回 native item 来“借用”它。

当前 prompt-cache anchor 会从 store 寻找同一 `prompt_cache_key` 的已完成请求，用于本地生命周期归属；而 full replay 仍跳过本地 history。native 路径可以保留这一信息作 checkpoint/诊断，但不得用它重建、拼接或替换 native `input`。该区别必须有单测：anchor 命中与未命中都不得改变发往 native upstream 的权威 full replay 字节结构（除经明确允许的本地工具所有权变换）。

### 15.4 配置与 UI 接线事实

`UpstreamConfig` 现含 `transport: auto | native_responses | chat_compat`；`UserWebSearchToolConfig` 现含 `backend: local | official`。配置 payload、runtime signature、manager API、UI radio control 与英文/简体中文文案已同步接线，仍使用现有 revision/CAS 保存保护。

两项设置严格独立，且不得复用：

- `DEEPSEEK_OFFICIAL_V1_COMPAT`：它只控制 Chat endpoint 是否使用 `/v1/chat/completions`；
- `NETWORK_PROXY_MODE` / legacy `WEB_SEARCH_PROXY_MODE`：它们只控制网络代理；
- model override：它只选择 upstream model slug，不声明某个模型具备 native Responses 能力。

现有 UI 文案与语言包为静态 HTML + `apps/ui/public/lang/*.json`。任何后续新增选项必须进入这一机制；缺失 key 回退英文，不能直接显示 provider 原始错误。

### 15.5 已冻结的 native 合约与保留限制

以下合约已由官方 Flash probe、fake upstream 与 Codex CLI fixture 共同验证：

1. **response identity。** provider response id 只用于脱敏诊断；Codex-facing response id 仍是 CodeSeeX lifecycle id。原生 response 顶层/事件中的 provider response id 会被替换为 local id，绝不改写 item id 或 `call_id`。
2. **client-owned tools。** `apply_patch`、`shell_command` 等由 Codex 执行；同一 provider response 的所有 call/output 是不可拆分组。RAM-only coordinator 只在完整权威 replay、锚点和输出类型全部精确匹配后构造下游 continuation；网络或 HTTP 失败不结算 pending group。流式 Responses 在每个 `response.output_item.done` 观察原始 item，受限地保留完整 provider group，并在 `response.completed` 后走同一 pending 注册；不会因为 stream 而绕开部分 output 拒绝。若 item 数、总字节、帧或 JSON 无法安全观察，运行时把该流标记为不可验证，不虚构 pending group。
3. **Web Search ownership。** `local` 保留既有 CodeSeeX bounded `web_search`，并令有本地工具的请求继续 Chat compatibility；`official` 才把 web capability 规范化为 provider `{ "type": "web_search" }`。不双发、不静默回退。官方后端若与 Chat compatibility 组合，直接返回兼容性错误，不改走 local。
4. **compaction、SSE 与取消。** Codex 是语义压缩所有者。native 不追加 compaction item 或 `[DONE]`；SSE 只在 `response.completed` 结算 completed，`failed`/`incomplete` 一律 failed，客户端取消为 interrupted。relay 只重写 JSON data 内的 response identity，原样保留 `event:`、`id:`、`retry:`、comment、扩展字段与行结束符。

当前明确保留的限制：native route **不执行 CodeSeeX-owned local tools**。这不是删除本地搜索或 workspace/vision/community tools，而是因为这些工具的执行器属于 CodeSeeX，尚未纳入 provider-native wire contract；它们继续使用成熟的 Chat compatibility 生命周期。该选择会记录 `selection=compatibility_required_by_request`，因此不是上游失败后的隐式回退。显式 `native_responses` 遇到此类请求返回可读错误；`auto` 在请求需要本地工具时选择 Chat compatibility。若用户明确选了 `official` Web Search 且同一请求还含本地工具，`auto` 也返回可读错误，绝不悄悄回退到 local search 使选择失效。

### 15.5.1 官方归属审计与真实 probe（2026-07-31）

在实现生产 native 路由前，已通过 `https://api.deepseek.com/responses`、`deepseek-v4-flash`、系统代理和私有脱敏 harness 进行了额外的六请求所有权 probe。结果只保存 request/response id hash、状态、SSE sequence、item type、token/延迟；不保存 key、prompt、response 正文或工具 payload。

| 问题 | 官方文档/真实结果 | CodeSeeX 的正确边界 |
| --- | --- | --- |
| 本地 `id`、`previous_response_id`、`store`、`metadata`、`prompt_cache_key` | 文档声明无状态且不支持的字段会忽略；真实请求确认 response 返回新的 provider id、`previous_response_id` 为空、`store=false`。 | provider id 不冒充本地 lifecycle id；CodeSeeX 仅在本地 store/诊断中关联两者。不得尝试把 local id 写成 provider session 或伪造 server continuation。 |
| 普通 `function` 调用 | 官方返回完成的 `function_call`；将原始 input、原始 provider output items 和 `function_call_output` 组成 full replay 后，下一请求正常完成。 | native route 保留 client-owned function 的原生 item replay；CodeSeeX-owned function 目前明确留在 Chat compatibility，不伪造本地 continuation。 |
| `custom` / `apply_patch` | 官方返回完成的 `custom_tool_call`；将原始 input、原始 output items 和 `custom_tool_call_output` 组成 full replay 后，下一请求正常完成。 | `apply_patch` 是 Codex client-owned：CodeSeeX 直接交还官方 item；之后 Codex 带回 output 时按原生 full replay 转发。仅保留既有、可证明的空白 context 行微修复。 |
| SSE 终止与 response identity | 真实流 31–34 个严格递增 sequence，唯一 provider response id，终止于 `response.completed`，没有 `[DONE]`。 | native stream 保留 upstream event body/order/sequence；只在安全的 response-id 边界作映射，绝不追加 `[DONE]`、重编号或合成 completed。 |
| 官方 Web Search | 官方文档定义 `{ "type": "web_search" }` / `web_search_2025_08_26`，并给出 server-side call 状态；此前真实基线已看到 completed 与 failed call。 | `official` 后端由 provider 完整执行和计费，CodeSeeX 不注入同名 local function、不重试、不回退。`local` 继续走现有函数工具。 |
| compaction、取消与 Usage | 官方不支持 `truncation`、`context_management`、background/server continuation；SSE final response 带最终 usage。连接取消与本地账单不是 provider 的会话功能。 | Codex 仍拥有语义压缩；native 不追加本地 compaction item。CodeSeeX 只负责在客户端断开时中止 HTTP stream、把 terminal event 映射到本地 status，并以 terminal usage 记账。 |

因此原 15.5 的前三项已有足够一手证据，不再是“是否可做”的阻断项；它们已转化为以下确定 contract：

```text
provider response id: provider scope, diagnostics only
CodeSeeX response id: Codex-facing lifecycle/store scope
provider native output: verbatim protocol data
local function continuation: authoritative input + provider output + function_call_output
client apply_patch continuation: authoritative Codex replay, no proxy tool execution
```

2026-08-01 追加了最小真实 mixed-tool probe（仅 Flash，系统代理，最多三次小额请求；报告只存哈希、状态、token 和计数）。provider 在同一 response 返回两个 `function_call` 后，full replay 若只携带其中一个 `function_call_output` 会稳定返回 HTTP 400 `No tool output found for tool call …`；携带两个对应 output 后正常完成。结论是：**同一 provider tool-call group 不能 partial continuation**。这不是 CodeSeeX 可通过重排规避的行为。

因此，原生协调器将同一 provider response 的所有 client-owned calls 作为 pending 原子组：它等待 Codex 交回全部 output，再以原始顺序构造完整 full replay。若 pending 归属、call id 或客户端重放无法精确对齐，返回受控 `context_required` / `tool_output_required` 诊断，绝不臆造、丢弃或替换输入。协调器的泛化数据结构可表达未来 local output，但 production route 目前不会进入该分支。

实现注意：planner 已将 `web_search_2025_08_26` 归一化为 `{ "type": "web_search" }`，并为 `local` / `official` 写有互斥测试；官方搜索仍仅在用户明确选择时生效。

取消路径已接入 production relay，并有 fake-stream integration test：已收到 `response.completed` 的 stream 即使随后取消仍按 completed 结算；`response.failed` / `response.incomplete` 一律失败；未收到 terminal event 的 stream 仅在明确 client cancel 时标记 interrupted，否则标记 failed。取消会停止读取 reqwest upstream body，且 native 不伪造 Chat-style terminal frame。

2026-08-01 完成了 isolated Codex CLI fake-responses resume / tool-handoff probe 的兼容性修复。此前 timeout 的根因是 Node `spawn()` 默认创建的 stdin pipe 被 `codex exec` 识别为额外 prompt，CLI 等待 EOF；fixture 显式关闭该 pipe 后，源码最小 catalog 与 DeepSeek 官方 Flash catalog 均稳定完成两次 `/responses` 请求、两回合 exit 0。第二次请求不带 `previous_response_id`，但保留完整历史，且已观察到 `assistant` / `reasoning` replay item。

同一 fixture 还验证了混合 client-owned tool group：provider 在一个 response 中给出 `custom_tool_call(apply_patch)` 与 `function_call(shell_command)`，Codex 实际执行二者；第二个 `/responses` 请求同时包含对应的 `custom_tool_call_output(call_fake_patch_1)` 和 `function_call_output(call_fake_shell_1)`。在仅用于该测试、可丢弃 workspace 的显式无沙箱执行中，`apply_patch` 成功写入 proof file，随后 shell 成功读取它。测试报告只保留 payload hash、item type、call id、输出长度/哈希与成功类别；不保留 API key、prompt、response 正文、工具输入或工具输出。

这项 fake 证据只证明 Codex CLI 对经验证 SSE item 顺序的消费、client tool 输出回放和完整工具组行为；它不替代官方服务端语义证据。后者仍由上文 Flash 真实 ownership/mixed-tool probes 覆盖。两者合在一起确认：native coordinator 必须等待同一 provider group 的全部 client output，然后将完整 authoritative replay 原样发送；它仍不得读取用户 Codex JSONL、不得拼接 tail、不得把工具组拆开。

2026-08-01 的私有小额 SSE probe 对 `deepseek-v4-pro` 返回 HTTP 400，内容为“early August 2026…use deepseek-v4-flash instead”。这是官方分阶段开放期的历史观测，不再构成 0.7.0 的模型路由门槛：产品发布目标按官方全模型 Responses 支持设计，`auto` 对规范化官方 endpoint 的所有模型选择 native；发生上游兼容问题时，用户可在实验性页主动改用 `chat_compat`。

### 15.6 从 0.6.0 暴露、由 0.7.0 修复的 Issue #17：thinking `reasoning_content` 回放

DeepSeek 官方思考模式文档明确要求：多轮 Chat Completions 请求必须把上一轮 assistant message 的 `content`、`reasoning_content` 与 `tool_calls` 一起追加回下一轮 `messages`。Responses 到 Chat compatibility 的转换因此不能把 reasoning 只当成 UI 展示数据。

当前根因已修复：

- Responses input 中独立的 `reasoning` item 在后面跟随普通 assistant message 时，会合并为 `ChatMessage.reasoning_content`，即使该 assistant 没有 tool calls；
- 非流式最终 Chat 回复持久化时保留 `reasoning_content`；
- 流式最终回复持久化时保留累计的 `turn_reasoning`；
- Codex full-context 运行时存储对 reasoning 做与正文相同的有界截断，不再无条件清空；
- Chat compatibility budget 保留并限制 `reasoning_content`，避免在预算整理时丢失；
- 没有 `turn_messages` 的旧记录从 response 重建 assistant 消息时，也恢复 reasoning 内容。

该问题属于 0.6.0 已发布链路暴露的兼容性缺陷，0.7.0 将其作为发布前修复项关闭。版本归属与修复归属必须区分：Issue #17 不是 0.7.0 新增功能，而是 0.7.0 对既有 Chat compatibility 回放行为的修复。

本地回归覆盖：普通无工具 assistant、带工具 assistant、full replay 当前 turn、budget 整理、代理实际发往 Chat upstream 的 `messages`。这只影响 Chat compatibility 的历史回放；native Responses 仍原样转发 Codex authoritative `input`，不解析或重写 provider reasoning item。原始 Issue：[CodeSeeX #17](https://github.com/TasteSteak/CodeSeeX/issues/17)。

### 15.7 Vision 能力边界

一手口径以 [DeepSeek 图像理解指南](https://api-docs.deepseek.com/zh-cn/guides/vision)、[Responses API 指南](https://api-docs.deepseek.com/zh-cn/guides/responses_api) 与 [模型和价格](https://api-docs.deepseek.com/zh-cn/quick_start/pricing) 为准；发布前必须重新核对模型名、限制和费率。

0.7.0 将图像理解与图像生成视为两种独立能力。Agent 工具分别为 `vision_analyze` 与 `image_gen`；它们拥有独立的启用状态、配置字段、凭据槽位、协议边界、Usage segment 和日志摘要。底层 HTTP client、图片路径解析、data URL、URL 安全检查、大小限制、响应截断和脱敏逻辑继续共享。

图像理解后端为 `deepseek | external`：

- `deepseek` 固定使用官方 `deepseek-v4-flash-vision-exp` 与 `https://api.deepseek.com/responses`，图片内容使用 Responses 的 `input_image`，图片细节支持 `auto | low | original`。
- DeepSeek Vision 只读取受信任的 DeepSeek 凭据来源，不使用本地 capability token、Codex inbound Authorization，也不把请求回送到 CodeSeeX 自身的 `/v1`。
- `external` 保留既有 OpenAI-compatible `/responses` 或 `/chat/completions` 识图方式。外部模式保持旧请求形状，不强加 DeepSeek 专用字段。

图像生成不受图像理解后端影响，继续使用独立的生成 URL、模型和 secret，支持 `/images/generations` 与 Responses `image_generation` 工具形状。DeepSeek Vision 专用模型不会加入 Codex 主模型 catalog。旧版统一 `VISION_API_KEY`、以及旧版挂在 `vision_analyze` 下的生成 URL/模型，只作为升级输入迁移到独立安全凭据和 `vision_generate` 配置；新保存不会把任何新 key 写入 TOML。配置 API 仅返回 `configured` 状态，不回显 secret。

Vision provider usage 从 `tool_result.vision.usage` 解析为独立的 `vision` Usage segment，并与主 Agent request 行分开。最终 token 以 provider usage 为准，请求前图片 token 估算不重复计费；自定义 provider 没有费率时只显示 token，不虚构 CNY。DeepSeek Vision 默认费率单独维护：缓存命中 `0.05`、缓存未命中 `1.5`、输出 `4.5` CNY/百万 token，峰时倍率沿用当前峰谷计费配置。

本切片不实现 Files API、远端图片持久化或图片生成 provider catalog；这些能力需要单独的隐私、生命周期和计费设计。

### 15.8 当前 production slice 与发布前验收

已实现：严格 `responses_url(...)` / HTTP client、official-default transport eligibility、native request planner、非流式 client-tool continuation、原生 SSE relay、RAM-only pending coordinator、`DEEPSEEK_TRANSPORT` / `WEB_SEARCH_BACKEND` 的独立保存与 UI 控件。相关测试覆盖 payload 不改写、response-id 映射、SSE sequence/terminal/usage、完整工具组、local search fallback 与配置迁移。

发布前验收仍必须包含：完整 proxy lib 测试、四个高压工具边界测试、Issue #17 reasoning replay 回归、Codex CLI fake resume/mixed-tool probe、独立代码审查和一次通过系统代理的最小真实 Flash SSE。不得将测试凭据、原始 prompt、工具输出或 Codex JSONL 加入仓库。
