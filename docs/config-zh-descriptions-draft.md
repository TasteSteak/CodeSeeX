# 设置页中文文案草案（Settings 页面）

> 本页仅供审阅，本轮不改动任何代码或语言包。
> 落地时按 `i18n key` 写回 `apps/ui/public/lang/zh_cn.json`，并同步英文到 `apps/ui/public/lang/en_us.json`。

## 一、范围与来源

- 覆盖：设置视图 `data-config-panel="client" | "proxy" | "tools" | "experimental"` 的配置项标签、分组标题、分段选项文字，以及 `<small class="muted">` 提示。
- 来源：现状中文取自 `apps/ui/public/lang/zh_cn.json`，现状英文对照 `apps/ui/public/lang/en_us.json`。
- 不含：左侧导航、通用按钮、日志页、用量页。
- 附录列出页内标签、按钮、占位符等边界文案，仅备查。

## 二、硬性风格规则

1. 不使用句号、分号（`。`、`；`、`.`、`;`），可用逗号（`，`）。
2. 精简、通用、陈述式短句，不写实现细节长句。
3. 同类描述句式统一，例如“修改后重启代理生效”。
4. 中英语义一致，中文自然，不做逐字硬译。
5. 术语统一：上游 Upstream、传输方式 transport、思考链 thinking chain、模型 model。
6. 例外：文件名点号（如 `config.toml`）与示例地址不受规则 1 约束。

## 三、术语表

| 中文 | 英文 | 说明 |
| --- | --- | --- |
| 上游 | Upstream | 被 CodeSeeX 代理的目标服务 |
| 传输方式 | transport | 上游请求走原生 Responses 或 Chat 兼容 |
| 思考链 | thinking chain | 模型的推理过程内容 |
| 模型 | model | 对话或视觉模型 |
| 识图 | image understanding | 图像理解能力 |
| 生图 | image generation | 图像生成能力 |
| 重启代理后生效 | takes effect after restarting the proxy | 需要重启生效的描述统一收尾 |

## 四、Client 面板（个性化 / 其它）

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| personalization | 分组标题 | 个性化 | 个性化 | Personalization |
| language | 标签 | 语言 | 语言 | Language |
| theme | 标签 | 主题 | 主题 | Theme |
| themeSystem | 选项 | 跟随系统 | 跟随系统 | System |
| themeLight | 选项 | 浅色 | 浅色 | Light |
| themeDark | 选项 | 深色 | 深色 | Dark |
| reasoningSummaryLabel | 标签 | 思考链显示 | 思考链显示 | Thinking chain display |
| reasoningSummaryHint | 描述 | 只决定 Codex 界面里显示多少思考链内容。上游模型照常思考，会话照常续接，发给模型的内容也完全不受影响。 | 只影响 Codex 界面显示的思考链内容，不改变上游模型的思考与发送内容 | Only changes how much of the thinking chain shows in the Codex window, the upstream model and the request stay unchanged |
| reasoningSummaryMode_none | 选项 | 不显示 | 不显示 | Hidden |
| reasoningSummaryMode_smart | 选项 | 智能 | 智能 | Smart |
| reasoningSummaryMode_fixed | 选项 | 定长 | 定长 | Trimmed |
| reasoningSummaryMode_full | 选项 | 完整 | 完整 | Complete |
| other | 分组标题 | 其它 | 其他 | Other |
| autoStart | 标签 | 开机自启动 | 开机自启动 | Start at login |
| autoStartHint | 描述 | 登录后在托盘后台启动，不主动打开主窗口。 | 登录后在托盘后台启动，不打开主窗口 | Starts in the tray without opening the main window |
| closeBehavior | 标签 | 关闭行为 | 关闭行为 | Close behavior |
| closeBehaviorExit | 选项 | 退出 | 退出 | Exit |
| closeBehaviorTray | 选项 | 最小化到托盘 | 最小化到托盘 | Minimize to tray |
| logRetention | 标签 | 日志保留 | 日志保留 | Log retention |
| logRetention1d | 选项 | 1 天 | 1 天 | 1 day |
| logRetention3d | 选项 | 3 天 | 3 天 | 3 days |
| logRetention7d | 选项 | 7 天 | 7 天 | 7 days |
| logRetention30d | 选项 | 30 天 | 30 天 | 30 days |

## 五、Proxy 面板（连接 / 模型行为）

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| proxyConnection | 分组标题 | 连接 | 连接 | Connection |
| deepseekBaseUrl | 标签 | DeepSeek 上游接口地址 | 上游接口地址 | Upstream URL |
| deepseekBaseUrlHint | 描述 | 留空则使用官方 DeepSeek API。自部署时可填写 OpenAI 兼容接口，例如 http://127.0.0.1:8000/v1。 | 留空使用官方 DeepSeek API，填写后写入 Codex 的 config.toml（[codeseex] upstream_base_url） | Leave blank to use the official DeepSeek API, a value is written to the Codex config.toml ([codeseex] upstream_base_url) |
| networkProxyMode | 标签 | 出站网络代理 | 出站网络代理 | Outbound network proxy |
| networkProxyModeHint | 描述 | 用于上游请求、Web Search、视觉模块和更新检查。修改后请重启代理。 | 用于上游请求、网页搜索、图像理解与更新检查，重启代理后生效 | Used by upstream requests, web search, image understanding, and update checks, takes effect after restarting the proxy |
| networkProxyMode_system | 选项 | 跟随系统 | 跟随系统 | Follow system |
| networkProxyMode_none | 选项 | 无代理 | 无代理 | No proxy |
| proxyListenPort | 标签 | CodeSeeX 监听端口 | CodeSeeX 监听端口 | CodeSeeX listen port |
| proxyListenPortHint | 描述 | 仅用于本地 Codex API 接入地址；修改后需要重启代理。 | 本地 Codex API 的接入端口，重启代理后生效 | Local Codex API endpoint port, takes effect after restarting the proxy |
| modelBehavior | 分组标题 | 模型行为 | 模型行为 | Model behavior |
| thinkingMode | 标签 | 思考模式 | 思考模式 | Thinking mode |
| thinkingAuto | 选项 | 自动跟随 | 自动跟随 | Auto |
| thinkingEnabled | 选项 | 强制开启 | 强制开启 | Force on |
| thinkingDisabled | 选项 | 强制关闭 | 强制关闭 | Force off |
| temperaturePreset | 标签 | 采样温度 | 采样温度 | Sampling temperature |
| temperatureDefault | 选项 | 默认 | 默认 | Default |
| temperatureStrict | 选项 | 严谨 | 严谨 | Strict |
| temperatureBalanced | 选项 | 均衡 | 均衡 | Balanced |
| temperatureGeneral | 选项 | 通用 | 通用 | General |
| temperatureCreative | 选项 | 创作 | 创作 | Creative |
| billingCachedInput | 标签 | 输入缓存命中 | 输入缓存命中 | Cached input |
| billingCacheMissInput | 标签 | 输入缓存未命中 | 输入缓存未命中 | Cache miss input |
| billingOutput | 标签 | 输出 | 输出 | Output |
| billingUnit | 描述 | 单位：CNY / 每百万 Tokens | 单位：CNY / 每百万 tokens | Unit: CNY / 1M tokens |

## 六、Tools 面板（工具卡片）

### 6.1 网页搜索配置与卡牌标签

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| webSearchBackend | 标签 | Web Search 后端 | 网页搜索后端 | Web Search backend |
| webSearchBackendHint | 描述 | 本地模式保留 CodeSeeX 的有界搜索；官方模式由 DeepSeek 服务端搜索，可能使用更多 tokens。CodeSeeX 只使用当前选定的一个后端；同一请求还需要本地工具时，应选择本地模式或 Chat 兼容。 | 本地模式使用 CodeSeeX 有界搜索，官方模式由 DeepSeek 服务端搜索并可能消耗更多 tokens，同一请求需要本地工具时请选择本地模式 | Local uses the CodeSeeX bounded search, official searches on the DeepSeek side and may use more tokens, pick Local when the same request also needs local tools |
| webSearchBackend_official | 选项 | DeepSeek 官方 | DeepSeek 官方 | DeepSeek official |
| webSearchBackend_local | 选项 | CodeSeeX 本地 | CodeSeeX 本地 | CodeSeeX local |
| toolLabelSystem | 标签 | 系统 | 系统 | System |
| toolLabelBuiltIn | 标签 | 内置 | 内置 | Built-in |

### 6.2 工具名称与描述

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| toolApplyPatchName | 工具名称 | 应用补丁 | 应用补丁 | Apply Patch |
| toolApplyPatchDescription | 工具描述 | Codex 原生补丁编辑工具，用于精确修改文件；作为系统工具跟随 Codex 设置。 | Codex 原生补丁编辑工具，用于精确修改文件，作为系统工具跟随 Codex 设置 | Native Codex patch editor for precise file changes, this system tool follows Codex settings |
| toolWebSearchName | 工具名称 | 网页搜索 | 网页搜索 | Web Search |
| toolWebSearchDescription | 工具描述 | 网页搜索与公共页面打开工具，遵循当前网络代理策略。 | 网页搜索与公共页面打开工具，遵循当前网络代理策略 | Search the web and open public pages with the configured network proxy policy |
| toolMcpServerName | 工具名称 | MCP 服务器 | MCP 服务器 | MCP Server |
| toolMcpServerDescription | 工具描述 | 使用 Codex 已发现的 MCP 工具；服务器配置仍保留在 Codex。 | 使用 Codex 已发现的 MCP 工具，服务器配置仍保留在 Codex | Use MCP tools discovered by Codex, server configuration stays in Codex |
| toolListDirectoryName | 工具名称 | 列出目录 | 列出目录 | List Directory |
| toolListDirectoryDescription | 工具描述 | 浏览工作区目录，支持深度限制、过滤和简洁元数据。 | 浏览工作区目录，支持深度限制、过滤和简洁元数据 | Browse workspace folders with depth limits, filters, and compact metadata |
| toolReadFileRangeName | 工具名称 | 读取文件范围 | 读取文件范围 | Read File Range |
| toolReadFileRangeDescription | 工具描述 | 读取工作区 UTF-8 文本文件的指定行范围；图片和二进制文件会被拒绝。 | 读取工作区 UTF-8 文本文件的指定行范围，图片和二进制文件会被拒绝 | Read selected UTF-8 text lines from workspace files, images and binary files are rejected |
| toolWorkspaceSearchName | 工具名称 | 工作区搜索 | 工作区搜索 | Workspace Search |
| toolWorkspaceSearchDescription | 工具描述 | 在工作区文件中搜索文本，支持包含/排除过滤和行匹配。 | 在工作区文件中搜索文本，支持包含/排除过滤和行匹配 | Find text across workspace files with include and exclude filters and line matches |
| toolVisionAnalyzeName | 工具名称 | 图像理解 | 图像理解 | Image understanding |
| toolVisionAnalyzeDescription | 工具描述 | 使用 DeepSeek Vision 或自定义 OpenAI 兼容识图端点分析图片。 | 使用 DeepSeek Vision 或自定义 OpenAI 兼容识图端点分析图片 | Inspect images with DeepSeek Vision or a custom OpenAI-compatible image understanding endpoint |
| toolVisionGenerateName | 工具名称 | 图像生成 | 图像生成 | Image generation |
| toolVisionGenerateDescription | 工具描述 | 通过单独配置的图像生成端点生成图片。 | 通过单独配置的图像生成端点生成图片 | Generate images through a separately configured image generation endpoint |

### 6.3 图像理解配置

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| visionAnalyzeBackend | 标签 | 图像理解后端 | 图像理解后端 | Image understanding backend |
| visionAnalyzeBackendHint | 描述 | DeepSeek Vision 使用官方 Responses API；自定义模型保留现有 OpenAI 兼容端点方式。 | DeepSeek Vision 使用官方 Responses API，自定义模型使用 OpenAI 兼容端点 | DeepSeek Vision uses the official Responses API, a custom model uses an OpenAI-compatible endpoint |
| visionAnalyzeBackend_deepseek | 选项 | DeepSeek Vision | DeepSeek Vision | DeepSeek Vision |
| visionAnalyzeBackend_external | 选项 | 自定义模型 | 自定义模型 | Custom model |
| visionImageDetail | 标签 | 图片细节 | 图片细节 | Image detail |
| visionImageDetailHint | 描述 | DeepSeek 当前将“自动”视为“原始”。“低”适合简单识图，并可能降低图片处理成本。 | DeepSeek 当前将“自动”视为“原始”，选择“低”适合简单识图并可能降低处理成本 | DeepSeek treats Auto as Original today, Low suits simple inspection and may reduce image processing cost |
| visionImageDetail_auto | 选项 | 自动 | 自动 | Auto |
| visionImageDetail_low | 选项 | 低 | 低 | Low |
| visionImageDetail_original | 选项 | 原始 | 原始 | Original |
| visionDeepSeekModel | 标签 | DeepSeek 识图模型 | DeepSeek 识图模型 | DeepSeek Vision model |
| visionDeepSeekModelHint | 描述 | 留空则使用内置识图模型（deepseek-v4-flash-vision-exp）。专用识图模型不会加入 Agent 主模型目录。 | 留空使用内置识图模型，专用识图模型不会加入 Agent 主模型目录 | Leave empty to use the built-in image understanding model, the dedicated model is never added to the main Agent model catalog |
| visionAnalyzeRequestUrl | 标签 | 识图请求地址 | 识图请求地址 | Analyze request URL |
| visionAnalyzeRequestUrlHint | 描述 | 完整的 OpenAI 兼容识图端点。本地图片像素会发送到此端点处理。 | 完整的 OpenAI 兼容识图端点，本地图片像素会发送到此端点 | Complete OpenAI-compatible image understanding endpoint, local image pixels are sent to this endpoint |
| visionAnalyzeModel | 标签 | 识图模型 | 识图模型 | Analyze model |
| visionAnalyzeModelHint | 描述 | 发送给视觉识图端点的模型名称。 | 发送给识图端点的模型名称 | Model name sent to the image understanding endpoint |
| visionAnalyzeApiKey | 标签 | 图像理解 API Key | 图像理解 API Key | Image understanding API key |
| visionAnalyzeApiKeyHint | 描述 | 仅用于自定义图像理解端点的 Bearer Token。 | 仅用于自定义图像理解端点 | Bearer token used only by the custom image understanding endpoint |

### 6.4 图像生成配置

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| visionGenerateRequestUrl | 标签 | 生图请求地址 | 生图请求地址 | Generate request URL |
| visionGenerateRequestUrlHint | 描述 | 完整的 OpenAI 兼容生图端点。建议使用 /responses；/images/generations 仅用于 OpenAI 官方图像模型接口。 | 完整的 OpenAI 兼容生图端点，建议使用 /responses，/images/generations 仅用于 OpenAI 官方图像模型接口 | Complete OpenAI-compatible image generation endpoint, prefer /responses and use /images/generations only for the official image-model API |
| visionGenerateModel | 标签 | 生图模型 | 生图模型 | Generate model |
| visionGenerateModelHint | 描述 | 发送给视觉生图端点的模型名称。 | 发送给生图端点的模型名称 | Model name sent to the image generation endpoint |
| visionGenerateApiKey | 标签 | 图像生成 API Key | 图像生成 API Key | Image generation API key |
| visionGenerateApiKeyHint | 描述 | 仅用于图像生成端点的 Bearer Token。 | 仅用于图像生成端点 | Bearer token used only by the image generation endpoint |

## 七、Experimental 面板（实验性功能）

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| experimentalFeatures | 分组标题 | 实验性功能 | 实验性功能 | Experimental features |
| deepseekTransport | 标签 | Chat API 兼容模式 | 上游传输方式 | Upstream transport |
| deepseekTransportHint | 描述 | 所有上游默认使用原生 Responses。当上游未实现 Responses API，或请求需要 CodeSeeX 本地工具执行器时，可选择 Chat API 兼容。 | 所有上游默认使用原生 Responses，上游未实现 Responses API 或需要 CodeSeeX 本地工具时改用 Chat 兼容 | Native Responses is the default transport for every upstream, switch to Chat compatibility when the upstream lacks the Responses API or the request needs a CodeSeeX local tool |
| deepseekTransport_native | 选项 | 原生 Responses | 原生 Responses | Native Responses |
| deepseekTransport_chat | 选项 | Chat API 兼容 | Chat API 兼容 | Chat API compatibility |
| codexAppModelListInjection | 标签 | Codex App 模型列表注入 | Codex App 模型列表注入 | Codex App model list injection |
| codexAppModelListInjectionHint | 描述 | 实验性功能，默认开启。“启动 Codex”会同时尝试修改 Codex App 渲染端模型列表；如果 Codex App 兼容性变化，可手动关闭。 | 实验性功能，默认开启，“启动 Codex”会同时尝试修改 Codex App 渲染端的模型列表，兼容性变化时建议关闭 | Experimental and enabled by default, Launch Codex also tries to patch the Codex App renderer model list, turn it off if Codex App compatibility changes |

## 八、附录 A 页内标签与状态（建议保持）

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| configTabClient | 页内标签 | 客户端 | 客户端 | Client |
| configTabProxy | 页内标签 | 代理 | 代理 | Proxy |
| configTabTools | 页内标签 | 工具 | 工具 | Tools |
| configTabExperimental | 页内标签 | 实验性 | 实验性 | Experimental |
| restartRequired | 状态 | 部分更改需要重启代理后生效 | 部分更改需要重启代理后生效 | Some changes require restarting the proxy |

## 九、附录 B 边界文案（按钮 / 提示 / 占位符，仅备查）

这些键已符合硬性风格规则或属于示例值，本轮不改，列出便于复核。

| i18n key | 类型 | 现状中文 | 建议中文 | 建议英文 |
| --- | --- | --- | --- | --- |
| catalogFetchModels | 按钮 | 更新模型列表 | 更新模型列表 | Update model list |
| catalogEmpty | 状态 | 尚未拉取到任何模型 | 尚未拉取到任何模型 | No models fetched yet |
| catalogEmptyHint | 描述 | 点击上方按钮从上游获取 | 点击上方按钮从上游获取模型 | Use the button above to fetch the list from upstream |
| modelLockPin | 图标提示 | 锁定为上游模型 | 锁定为上游模型 | Pin as upstream model |
| modelLockClear | 图标提示 | 取消锁定，回到跟随客户端 | 取消锁定，回到跟随客户端 | Unpin and follow the client model |
| billingUnpriced | 状态 | 未定价 | 未定价 | Unpriced |
| billingGroupPriced | 状态 | 按组定价 | 按组定价 | Group priced |
| visionDeepSeekModelPlaceholder | 占位符 | deepseek-v4-flash-vision-exp | 保持 | deepseek-v4-flash-vision-exp |
| visionAnalyzeRequestUrlPlaceholder | 占位符 | https://api.example.com/v1/responses | 保持 | https://api.example.com/v1/responses |
| visionAnalyzeModelPlaceholder | 占位符 | gpt-4o-mini | 保持 | gpt-4o-mini |
| visionApiKeyPlaceholder | 占位符 | sk-... | 保持 | sk-... |
| visionGenerateRequestUrlPlaceholder | 占位符 | https://api.example.com/v1/responses | 保持 | https://api.example.com/v1/responses |
| visionGenerateModelPlaceholder | 占位符 | gpt-image-1 | 保持 | gpt-image-1 |

## 十、关键改动点

1. `deepseekBaseUrl` 改为“上游接口地址”，描述说明留空使用官方 API、填写后写入 Codex 的 config.toml（[codeseex] upstream_base_url）。
2. `deepseekTransport` 由“Chat API 兼容模式”改为“上游传输方式”，与 Native Responses / Chat 兼容两个选项对应。
3. `reasoningSummaryHint`、`networkProxyModeHint`、`proxyListenPortHint`、`codexAppModelListInjectionHint` 统一精简，删除句号与分号。
4. 需要重启的提示统一为“重启代理后生效”，覆盖网络代理与监听端口两处。
5. 工具描述统一删除句尾句号与分号，保留原有信息，句式统一为陈述式短句。
6. 术语收敛到“上游、传输方式、思考链、模型”，`webSearchBackend` 与网页搜索工具名称对齐为“网页搜索”。
