# 更新日志

## 0.7.1 - 2026-09-10

CodeSeeX 0.7.1 把模型清单与定价改为带版本、可远程更新的数据文档，并始终保留完整的离线保底数据；同时恢复与要求 Codex 客户端身份的上游地址的兼容。

### 重点更新

- 模型目录改为带版本的数据文档，按层解析：用户覆盖 > 远程清单 > 本地缓存 > 内置文档。模型、别名、窗口与价格可以在不发新客户端的前提下更新。
- 远程清单在启动、每 6 小时以及设置页手动触发时复查；任何失败都保留上一层数据，且不会阻塞代理。
- 上游 `GET /v1/models` 仅作为可用性探测，不提供窗口、能力或定价，也绝不会删除模型。
- 定价按精确 slug 匹配，并提供显式分组回退与显式「未定价」状态；未知模型不会被静默套用其它模型的价格。
- 峰谷计费收敛为单一实现（store 与前端共用），窗口边界统一为 `[起, 止)`。
- 部分上游地址会因缺失 Codex 客户端身份而拒绝请求：此前被丢弃的 `originator`、`user-agent`、`session_id`、`conversation_id` 现已补齐并原生透传，非官方 endpoint 同样原样透传客户端 Authorization。

### 新增

- 新增 `crates/core/src/pricing.rs`：数据化定价表，包含货币、单位、按模型费率、显式费率分组与峰谷窗口/倍数。
- 新增 `crates/core/src/catalog.rs` 目录文档：schema、体积、重复 slug、最低应用版本校验，字段级合并、原子缓存写入与用户覆盖。
- 新增内置目录 `crates/core/assets/catalog.default.json` 与公开清单 `docs/catalog/model-catalog.json`。
- 新增远程目录拉取：ETag 复用、拉取节流、短超时、失败记录与内置回退，与 release notes 的拉取链路同构。
- 新增 `GET /api/catalog`、`POST /api/catalog/refresh`、`GET /api/upstream/probe`、`POST /api/upstream/test`（同时支持 `/manager/upstream/test`）与 `POST /api/upstream/credential`。
- 新增显式上游凭据来源：`auto`、`request`、`env`、`codex_auth`、`secret`；上游请求附带 `x-codeseex-credential-source` 便于定位凭据来源。

### 改进

- 模型别名与出站 slug 改写改为数据驱动：`aliases`、`alias_patterns`、`upstream_slug` 全部来自目录，不再硬编码 `gpt-5*`。
- 设置页按目录为每个模型生成一行可编辑费率，并提供时区、峰时时段与峰时倍数，替代原来的三张固定卡片。
- 用量成本估算读取当前生效的价目表，未定价模型显式标注。
- `GET /v1/models` 与 `/api/models` 现在返回当前生效的目录文档，而非内置文档。
- `model-catalog.json` 磁盘契约保持不变，仅生成来源改为合并后的目录文档。
- 客户端身份头原样透传：CodeSeeX 只负责路由，不重写「请求是谁发出的」。

### 修复

- 修复部分上游地址因缺失 Codex 客户端身份而被拒绝的问题。客户端的 `originator`/`user-agent`/`session_id`/`conversation_id` 此前被丢弃，现已原生透传；Codex App 形态请求的客户端 Authorization 也不再被丢弃——仅官方 endpoint 保留凭据隔离。
- 修复 `GET /v1/models` 返回内置目录而非当前生效目录的问题。
- 修复远程清单「已拉取但未变化」时不保存 ETag、导致后续每次刷新都重新下载整个文档的问题。
- 修复设置页把未知模型静默按 Pro 费率计价的问题。
- 修复峰谷窗口与倍数在 Rust 与 JavaScript 中各写一遍的问题，二者现在都来自价目表。
- 修复上游原生分组工具声明（`namespace`、`tool_search`）被判定为无法翻译而直接拒绝的问题。CodeSeeX 现在会先校验、再原样透传这类声明，让 endpoint 保留它自己负责的工具分组与 namespace；未知或残缺的声明仍然显式失败，而不是被静默丢弃。
- 修复默认配置下任何带工具的请求都不可用的问题：在原生 Responses 传输 + CodeSeeX 本地 web 搜索后端时，Codex 总会声明的上游原生 `web_search` 会被直接拒绝。现在这类搜索会在原生传输内部执行，而不再转交 Chat API 兼容通道，因此工具归属仍不会被静默改变。
- 修复上游只要索要 Codex 自己的工具（例如 `read_thread`）就会以 `mixed tool group` 失败的问题。整组都是 Codex 自有的工具调用会原样交回 Codex，并在内存中保留该工具组用于续接校验；只有真正混用自托管与客户端工具的一轮才继续显式失败。
- 修复 Codex App 重启后原生 Responses 重放被 `Duplicate namespace name 'codex_app'` 拒绝的问题。重复的分组声明会合并进首次声明，而不再作为重复项转发，并在事件日志中记录这次修复。
- 修复原生 Responses 兼容性失败在日志中丢失 `issue`、`selected_web_search_backend`、`fallback` 字段、导致无法从日志定位不兼容原因的问题。
- 修复原生 Responses 重放被 `Invalid schema for function 'codex_app::automation_update'` 拒绝的问题。Codex 的延迟加载应用工具可能只声明顶层 `oneOf` 联合、没有 `type` 的参数 schema，上游会直接拒绝；CodeSeeX 现在会把这类联合声明为 object 类型（不改动联合本身），并在事件日志中记录这次修复。
- 修复原生工具续接被 `The native tool continuation did not retain the provider tool group visible to Codex` 拒绝的问题。Codex 在重放历史前会丢弃非前缀限定的 item id，以及它自身 item 结构无法承载的上游字段；续接校验现在改为比对协议单元——按顺序的 item 类型与工具调用身份——而上行请求仍使用 CodeSeeX 自己保存的工具组副本。
- 修复托管工具循环把 CodeSeeX 自己执行过的托管轮次从上行续接中丢弃的问题。当 Codex 自有的工具轮次紧跟在 CodeSeeX 执行的搜索之后时，被保留的工具组现在会按该轮次在客户端可见锚点中的原始位置重新回放，上游因此仍能看到它产生该工具组时的完整上下文。

### 兼容说明

- 0.7.0 的 `BILLING_*` 配置键仍可读取，并会迁移到 `[billing]`；新写入只使用结构化键。
- 默认远程清单指向仓库 `main` 分支，推送到 `main` 后生效；在此之前使用缓存或内置文档。可用 `CODESEEX_CATALOG_URL` 指向镜像，或设为 `off` 关闭远程拉取。
- 用户覆盖与私有提示词 overlay 永远优先于远程清单；远程文档无法改写 `base_instructions`/`model_messages`。
- 逐请求费率快照（历史用量的价格复现）不在 0.7.1 范围内；计费桶会携带 pricing revision 与解析后的费率，保证估算有明确标注。
## 0.7.0 - 2026-08-25

CodeSeeX 0.7.0 是一次面向 DeepSeek Responses API 的正式适配更新。官方 DeepSeek endpoint 默认使用原生 Responses，Chat API 兼容模式保留为用户主动选择的实验性回退。

### 重点更新

- 官方 DeepSeek endpoint 的所有配置模型默认走原生 Responses，包括原生 SSE 和 Responses 工具项。
- 保留 CodeSeeX 本地 Web Search 与 DeepSeek 官方 Web Search 两种独立后端，并要求用户明确选择。
- 修复 DeepSeek thinking 模式上下文连续性，assistant 的 `reasoning_content` 会与消息一起回放。
- 保持 Codex full replay 为权威输入，并保持原生客户端工具组的原子续接。
- 将图像理解与图像生成拆为两个独立工具，支持 DeepSeek Vision 和独立凭据。

### 新增

- 在实验性设置中新增 Chat API 兼容模式，用于上游兼容和明确排障回退。
- 新增原生 Responses 的 full replay、response identity 映射、function/custom 工具、取消、终止状态和最终用量处理。
- 新增 Web Search 后端选择：CodeSeeX 本地搜索或 DeepSeek 官方服务端搜索。
- 新增 reasoning 回放测试，覆盖普通 assistant turn、工具 turn、full-context 存储和兼容模式预算处理。
- 新增独立的图像理解与图像生成工具，支持 DeepSeek Vision、独立凭据和独立 Usage 计账。
- 新增图像能力说明文档，覆盖 provider、限制、隐私、凭据、用量与计费边界。
- 补齐内置语言包的完整键结构；尚未翻译的新文案显式使用英文 fallback，不再显示原始 key。

### 改进

- `auto` 在官方 `https://api.deepseek.com` endpoint 下对所有配置模型选择 `/responses`；自定义 endpoint 继续保守使用 Chat 兼容路径。
- Chat API 兼容是实验性显式选项；当请求必须由 CodeSeeX 本地工具执行器处理时，仍使用这条成熟的兼容路径。
- 原生 Responses 保留 provider 事件顺序和 sequence，不再合成 Chat 风格的 `[DONE]`。
- thinking 模式的 `reasoning_content` 在非流式、流式、旧 response 重建和有界 full-context 运行时存储中保持可回放。
- Web Search 不会双发，也不会静默替换用户选择的搜索归属。
- 识图与生图不再共用工具开关或凭据；DeepSeek Vision 不会加入 Codex 主模型 catalog。
- 图像理解通过一次性能力配置迁移，在全新安装和旧配置升级后默认开启；迁移完成后继续尊重用户选择，图像生成保持独立且默认关闭。
- Vision 用量作为独立会话阶段记录，显示自己的 token、耗时、模型和峰谷费用估算。
- 默认 Vision 诊断只保留 provider、模型、图片数量、细节模式、耗时和归一化 usage，不保存原图、base64、完整 prompt 或 provider response。

### 修复

- 修复 0.6.0 已暴露的 thinking 模式连续性问题：assistant 的 `reasoning_content` 会被错误丢弃，导致下一次 Chat API 请求缺少推理上下文。
- 修复原生工具续接在上游失败或未完成时过早结算 pending 原子工具组的问题。
- 修复未知 `previous_response_id` 可以绕过匹配的原生 pending 工具组锚点的问题。
- 修复无终止空行的有界原生 SSE 帧没有映射 provider response id 的问题。
- 修复旧 Chat 历史重建和预算处理丢失有界 reasoning 内容的问题。
- 修复旧版 Vision URL、模型和 `VISION_API_KEY` 在合并配置迁移时可能丢失的问题。
- 修复旧版显式工具列表导致升级后新的图像理解能力仍处于关闭状态的问题。
- 修复生图可能复用新的识图凭据，或被旧生图字段隐式启用的问题。
- 修复密码字段 autosave：空输入保持原 secret，只有明确填写新值或勾选清除时才保存变更。

### 兼容说明

- 官方 DeepSeek endpoint 默认使用 Responses。上游需要回退，或请求必须由 CodeSeeX 本地工具执行器处理时，才建议在实验性设置中选择 Chat API 兼容模式。
- CodeSeeX 本地 Web Search 继续保留并仍是默认后端；DeepSeek 官方 Web Search 由 provider 执行，可能产生额外 token 或多次服务端搜索调用。
- 自定义 OpenAI 兼容 endpoint 在 `auto` 下继续使用 Chat 兼容；强制自定义 endpoint 使用原生 Responses 仅支持高级 TOML/环境变量，能力不兼容时会明确失败。
- CodeSeeX 不读取或写入 Codex JSONL，也不在原生 Responses 路径中增加 Codex App 专用注入。
- DeepSeek Vision 需要明确的 `DEEPSEEK_API_KEY` 来源；自定义识图与生图使用独立 secret。没有安全凭据库的平台会 fail closed，不会把密钥写回明文 TOML。

## 0.6.0 - 2026-07-11

CodeSeeX 0.6.0 是一次上下文运行时正确性更新。它将 Codex HTTP full replay 作为权威输入，保持工具协议组完整，限制 workspace 工具输出，并新增带安全离线回退的应用内更新日志。

### 重点更新

- 新增 Canonical Session Core：仅用匿名指纹在内存中对齐活跃 HTTP replay，不读取 Codex 对话记录，也不创建持久化对话存储。
- Codex full replay 现在会作为权威上下文直接转发，不再被改写为本地 tail-only continuation。
- 工具调用批次和全部对应结果按原子协议组处理，包括内部工具与客户端工具混合的批次。
- workspace 检查工具现在会对大型文件和目录返回有界、可分页的结果，同时仍保留广泛仓库搜索能力。
- 在“关于”页面的官方网站操作下新增“更新日志”入口。

### 新增

- 新增按版本固定的结构化更新日志，提供英文和简体中文；其它更新日志语言统一回退英文。
- 新增本地更新日志 API：使用 GitHub tag 查询、ETag 校验、短网络超时、内存重试退避和内置离线内容。
- 新增本地 fake upstream smoke 示例，可零成本检查 replay 形状、缓存前缀连续性、工具配对和脱敏后的出站 trace。
- 新增分页 `list_directory`、支持超长行续读的有界 `read_file_range`，以及带明确截断诊断的 source-first `workspace_search`。

### 改进

- 移除代理专用的 96k full replay 预算。上下文现在只受配置的上游模型窗口、输出预留和工具预留约束。
- full replay 出现分歧或 Codex 压缩后，会按新的 Codex 输入重建活跃内存对齐；不再静默保留本地尾部。
- 工具输出、binary/data URL、凭据、搜索片段和诊断信息会在进入 replay 前保持有界并脱敏，避免无控制的成本增长。
- Apply Patch 兼容路径只修复已经明确的“空白上下文行缺少前缀”格式问题；正确或含义不明确的补丁保持原样。
- 发布资源与官网缓存版本现在会持续与桌面版本保持一致。

### 修复

- 修复代理侧 tail continuation 或 replay 截断导致的缓存命中重置、上下文使用率跳动和语义上下文丢失问题。
- 修复混合工具历史可能以上游不完整 assistant tool-call 组形式发送的问题。
- 修复长文件读取、目录列举和宽泛搜索可能生成过大工具结果的问题。
- 修复手工 fake-upstream trace 默认写入仓库根目录的问题；现在默认写入已忽略的 `.private` 目录。

### 兼容说明

- CodeSeeX 仍不会读取、修改或恢复 Codex jsonl 对话记录。Canonical 对齐仅存在于内存中，并在代理重启或会话 TTL 到期后清除。
- 如果权威 Codex replay 无法装入真实上游上下文窗口，CodeSeeX 会返回受控的上下文限制诊断，不会静默摘要或丢弃历史。
- 更新日志会优先读取对应 GitHub tag。GitHub 不可用或 tag 尚未发布时，应用内置更新日志仍可离线使用。

## 0.5.4 - 2026-07-08

CodeSeeX 0.5.4 是一次聚焦 Agent 稳定性的热修复版本。它让客户端工具失败继续回填给模型处理，避免旧的 handoff guard 阻断后续请求，并增强上游响应解码错误诊断。

### 重点更新

- 重复客户端工具失败默认不再变成代理层终止错误。
- “请继续”等后续请求不再继承旧的客户端工具 handoff 停止状态。
- 上游响应体解码失败现在会记录更安全的诊断信息，便于排查网络、代理和上游响应问题。

### 改进

- 客户端工具失败追踪改为按工具名、参数 hash、压缩失败摘要 hash 判断，而不是只按工具名判断。
- 重复失败保护改为仅记录诊断，让模型能够看到失败结果并自行选择恢复路径。
- handoff preflight 不再因为之前的重复失败状态而提前截断后续请求。

### 修复

- 修复正常排障流程中多个 `shell_command` 失败后被过早中断的问题。
- 修复重复 apply_patch 或 shell 失败可能污染后续继续请求的问题。
- 修复 `error decoding response body` 日志信息过少的问题；现在会记录状态码、安全响应头摘要和 reqwest 错误类型，但不会记录原始上游正文。

### 兼容说明

- 本版本不改变模型 catalog、费用估算、更新签名或 Web Search 策略。
- 如果外部诊断依赖客户端 handoff guard stop，重复失败记录现在应作为 warning 处理；只有出现独立 terminal error 时才表示请求被终止。

## 0.5.3 - 2026-07-06

CodeSeeX 0.5.3 是一次发布前稳定性与桌面更新能力增强版本，重点改进 Codex App 集成安全性、catalog 诊断和应用内更新链路。

### 重点更新

- 新增应用内更新流程，支持检查更新、下载进度、取消下载和受支持桌面包的静默安装。
- 改进 Codex App 切换模型后的上下文连续性，保护 full-context replay 与 prompt cache 会话锚点。
- 新增 Codex runtime catalog 诊断，帮助用户确认 Codex 是否真的读取了 CodeSeeX 模型目录。
- 加固发布打包流程，改进 Windows、macOS、Linux 的签名 updater manifest 生成。

### 新增

- 新增桌面更新命令与下载进度事件，用于更新弹窗。
- 新增实验性 Codex App 模型列表注入开关：默认开启、持久化保存；如果 Codex App 兼容性变化，可以手动关闭。
- 新增 catalog 路径不一致、模型目录未加载、启动期 catalog 行为等故障排除诊断。

### 改进

- 启动 Codex App 时，只有在实验性开关开启时才尝试渲染端模型列表注入。
- Codex App 切换模型后的 full-context replay 优先保留客户端 replay 内容，避免短用户历史被裁掉。
- 更新红点改为应用本次运行内已读，不再对当前版本永久隐藏。
- 发布 manifest 现在会同时写入 installer 专用 target 与基础 target，提升自动更新兼容性。

### 修复

- 修复 Codex App 切换模型且未发送 `previous_response_id` 时可能出现的缓存/上下文连续性风险。
- 修复更新安装体验，下载更新时显示后台进度，而不是只跳转到 release 页面。
- 修复 catalog 故障排除验证时展开区域被折叠的问题。
- 修复发布工作流可能遗漏 updater 兼容平台条目的问题。

### 兼容说明

- 如果需要确保 catalog 准确，仍建议从 CodeSeeX 桌面端直接复制 TOML。部分 CCS 导入流程可能不会保留 Codex 模型目录。
- 应用内更新依赖 GitHub release manifest 中的签名 updater 产物。
- Codex App 模型列表注入仍是实验性功能，可关闭；关闭后不影响 CodeSeeX 代理的正常使用。

## 0.5.2 - 2026-07-02

CodeSeeX 0.5.2 是一次小版本稳定性与计费显示更新，重点修复长时间 Agent 任务中的工具调用中断问题，并适配 DeepSeek 峰谷计费估算。

### 重点更新

- 修复长时间运行任务中，客户端工具调用重复触发后被过早中断的问题。
- 新增 DeepSeek 峰谷计费估算模式，并在配置中默认开启。
- 优化识图工具配置界面，地址与 API Key 输入框更长，模型输入框更紧凑。
- 修复右键“全选文本”会选中整个应用页面的问题。

### 新增

- 用量信息支持峰谷计费估算：
  - 北京时间 09:00-12:00、14:00-18:00 按峰时倍率估算。
  - 其它时间按普通倍率估算。
  - 配置页提供“峰谷计费模式”开关，默认开启。
- 工具配置字段支持通用宽度规则：
  - URL、Endpoint、API Key、Token 等字段默认使用较长输入框。
  - Model 字段默认使用较短输入框。
  - 第三方工具配置可通过字段元数据声明宽度。

### 改进

- 优化 Usage 费用聚合，按模型与峰谷时段拆分计费桶，避免跨时段记录被混合估算。
- 调整配置页用量显示区域，使峰谷计费开关与其它设置项保持一致的分割线和开关样式。
- 改进识图工具配置布局，减少输入框宽度不一致造成的视觉混乱。
- 优化右键菜单的“全选文本”行为，现在只选择当前可见页面或当前输入框内容。

### 修复

- 修复长时间任务中相同客户端工具调用签名重复后导致流连接中断的问题。
- 修复峰谷计费开关未显示为标准开关样式的问题。
- 修复峰谷计费开关与下方计费费率设置之间缺少分割线的问题。
- 修复密码类工具配置输入框因外层与内层双重宽度限制导致显示偏短的问题。
- 修复右键全选会选择隐藏页面或整个 workspace 文本的问题。

### 兼容说明

- 峰谷计费只影响 CodeSeeX 的费用估算显示，不改变真实上游计费结果。
- 已有用户配置不会被强制覆盖；未配置时峰谷计费默认开启。
- 第三方工具无需修改即可继续使用，新的字段宽度元数据为可选能力。

## 0.5.1 - 2026-06-23

CodeSeeX 0.5.1 是 0.5 Rust/Tauri 版本线的一次稳定性与体验更新，重点改进 UI 体验、可选 Codex App 模型切换、Web Search 行为和发布文档。

### 重点更新

- 优化桌面 UI，改进 Usage、Logs、设置、截图和发布页面体验。
- 新增可选 Codex App 模型切换能力，可在 Codex App 中显示 DeepSeek V4 Flash / Pro；仍建议优先从 CodeSeeX 内切换模型以获得更稳定的运行时行为。
- 优化 Web Search 的源探针、证据打开、fallback 行为和诊断信息。
- 更新 README 截图、官网入口、更新提示和发布文档。

### 改进

- 改进 Usage 和 Logs 的布局、滚动、事件展示与详情加载，不改变计费语义。
- 更新 Codex App 集成，使 Flash / Pro 可出现在 Codex App 模型菜单中，同时保持 CodeSeeX 侧模型切换作为推荐工作流。
- 优化 Web Search 的网络健康排序、证据收集和诊断能力，同时保留本地/私有目标保护。
- 改进生成的 Codex 配置、截图和官网链接相关文档。

### 修复

- 修复 Usage 页面滚动、活动会话刷新、服务请求标签和临时中间记录问题。
- 修复 Logs 信息过平或噪声过多，难以解释请求、工具、缓存和网络行为的问题。
- 修复桌面更新链接，使其可以从 WebView 打开系统浏览器。
- 改进 Codex App 模型切换稳定性，同时仍建议使用 CodeSeeX 作为主要模型切换入口。

### 打包说明

- 从 0.5.0 升级的用户，如果使用 Codex App 集成，建议安装后完整重启 CodeSeeX 和 Codex App。
- 生成的 Codex TOML 与 catalog 路径仍和本机环境相关，因此仍建议从桌面管理器复制配置。
