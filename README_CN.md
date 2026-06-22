<h1 align="center">CodeSeeX</h1>

<p align="center">
  <img alt="Version 0.5.0" src="https://img.shields.io/badge/version-0.5.0-1f6feb">
  <img alt="Platform Windows macOS Linux" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-2ea043">
  <img alt="License AGPL-3.0-only" src="https://img.shields.io/badge/license-AGPL--3.0--only-bd561d">
</p>

<p align="center">
  面向 Codex 与 DeepSeek V4 的本地原生 Agent Runtime，而不是普通 API 转发器。
</p>

<p align="center">
  <img alt="CodeSeeX desktop manager" src="docs/img/1.png" width="860">
</p>

<p align="center">
  非官方项目，和 Codex、OpenAI、DeepSeek 及搜索服务商均无隶属关系。请使用你自己的凭据，并遵守相关服务条款。
</p>

CodeSeeX 通过本地 `/v1` 适配器把 Codex Desktop 连接到 DeepSeek 兼容上游。它的目标不是简单把一个 HTTP API 转成另一个 HTTP API，而是在 Codex Agent 边界处理请求语义、工具调用、上下文回放、推理展示、网络搜索、本地文件操作、用量统计和桌面管理。

CodeSeeX 面向的是当前 AI 工具市场中的一个明确空缺：

- 通用 API 网关擅长模型、渠道、密钥和协议路由。
- 简单转接脚本擅长让某个模型临时接入另一个 endpoint。
- CodeSeeX 面向 Codex 风格的真实 Agent 会话，重点是工具生命周期、上下文卫生、服务请求分类、用量可观测和长期稳定性。

当前版本：`0.5.0`

```text
Codex Desktop  ->  CodeSeeX 本地 Agent Runtime  ->  DeepSeek 兼容上游
                         |
                         +-> Codex 工具、网络搜索、用量、诊断、桌面管理器
```

## 为什么需要 CodeSeeX

让 Codex 通过非原生上游回答一次问题并不难。真正困难的是让它在长会话、工具循环、大型仓库、后台服务请求和反复文件修改中仍然像一个稳定的 Codex Agent。

CodeSeeX 关注的是这部分工程问题：

- 保留 Codex 原生语义，而不是把所有请求都当成普通聊天。
- 让工具执行过程可观测、可限制、可回放。
- 避免工具结果、可见 thinking、服务请求和 full-context payload 污染后续上下文。
- 按用户任务和 Agent 阶段展示用量，而不是只给出一串上游 API 调用记录。
- 提供本地桌面控制面，用于日志、设置、模型目录、余额、工具和运行状态管理。

## 不是普通转发器

直接转接工具通常只负责转发请求体、映射模型名和返回响应。这类工具有价值，但很多 Codex Agent 行为并不会因此自动正确。

CodeSeeX 在转发层外增加了本地 Runtime：

| 维度 | 普通 API 转接 | CodeSeeX |
|---|---|---|
| 请求语义 | 主要转发 chat/responses payload | 识别用户 turn、服务请求、compact/replay 状态和客户端工具 handoff |
| 工具 | 通常透传 tool schema | 管理 CodeSeeX 工具，保留 Codex 原生客户端工具，桥接延迟工具发现，并记录工具生命周期 |
| 上下文 | 客户端发什么就转什么 | 编译上下文、保留已验证工具事实、避免重复保存 Codex 全量 transcript，并限制 replay 数据 |
| DeepSeek 行为 | 把输出当作普通模型文本 | 在 provider 边界适配 DeepSeek thinking / tool protocol 行为 |
| 用量 | 平铺上游请求记录 | 按用户任务、服务请求、handoff 阶段和 tool-loop segment 聚合 |
| 桌面体验 | 多依赖外部配置文件 | Tauri 管理器提供状态、日志、用量、设置、Adapter TOML、更新、余额和托盘控制 |
| 安全边界 | 依赖客户端或上游 | 增加本地/私网保护、工具输出上限、诊断脱敏和 community tool 信任边界 |

换句话说，CodeSeeX 面向的是希望在 Codex 中使用 DeepSeek，同时不牺牲 Codex Agent 运行质量的用户。

## 你会得到什么

- 在 Codex 中使用 DeepSeek V4 模型：`deepseek-v4-pro` 和 `deepseek-v4-flash`。
- 自动生成 Codex TOML，包含机器相关的 `model_catalog_json` 和本地 `base_url`。
- 内置模型目录，用于首次运行或缺少原生 Codex catalog 的环境。
- Flash 与 Pro 的 1M context 元数据和 95% effective context window。
- Codex 原生 Apply Patch 处理和客户端工具 handoff 行为。
- CodeSeeX 托管的 Web Search，包含有界执行、source diagnostics、自动打开证据页和本地/私网目标保护。
- 只读 workspace 工具，用于文件和仓库检查。
- 可选 Vision 模块，支持 OpenAI 兼容的图像理解和图像生成 endpoint。
- 上下文编译：已验证工具事实、compact summary、binary/data URL 脱敏和工具结果有界 replay。
- 用量会话：区分普通用户 turn、服务请求、模型迭代、工具阶段、handoff、缓存命中、缓存未命中、输出 token 和估算费用。
- 桌面管理器：托盘、自动启动、更新检查、日志、用量、余额、设置、工具和 Adapter 配置。
- Community tool discovery：位于 `~/.codeseex/extension/tools/<tool>/manifest.json`，默认关闭，仅通过显式命令 manifest 执行。

## 截图

<p align="center">
  <img alt="CodeSeeX dashboard and balance view" src="docs/img/legacy-dashboard-balance.png" width="860">
</p>

<p align="center">
  <img alt="CodeSeeX usage view" src="docs/img/legacy-usage.png" width="860">
</p>

下面的 Adapter 设置截图来自较早的桌面布局。实际使用时请以当前 CodeSeeX 应用生成的 TOML 为准。

<p align="center">
  <img alt="CodeSeeX adapter settings view" src="docs/img/legacy-config-toml.png" width="860">
</p>

## 快速开始

1. 从 [GitHub Releases](https://github.com/TasteSteak/CodeSeeX/releases) 下载对应平台的最新版本。
2. 启动 CodeSeeX。
3. 打开 `Settings -> Proxy`，确认本地服务运行在默认端口 `8787`。
4. 从 CodeSeeX 的 Adapter 卡片复制生成的 Codex TOML。
5. 将该 TOML 放入你用于 DeepSeek 的 Codex 配置中。
6. 修改 TOML 后重启 Codex。
7. 在 Codex 中选择 `deepseek-v4-pro` 或 `deepseek-v4-flash`。

建议优先使用应用内生成的 TOML，因为 catalog 路径和本地端口都和当前机器有关。

```toml
model_provider = "custom"
model = "deepseek-v4-pro"
disable_response_storage = true
model_reasoning_effort = "xhigh"
# CodeSeeX 会在生成的 TOML 中加入机器相关的 model_catalog_json 路径。

[model_providers.custom]
name = "DeepSeek"
wire_api = "responses"
requires_openai_auth = true
base_url = "http://127.0.0.1:8787/v1"
```

如果要使用更快的模型，将模型改为：

```toml
model = "deepseek-v4-flash"
```

## 桌面管理器

桌面应用是本地 Runtime 的控制面：

- Dashboard：代理状态、当前端口、余额、更新状态和故障排查提示。
- Usage：按用户任务展示模型阶段、工具阶段、缓存命中/未命中、输出、耗时和费用。
- Logs：紧凑运行日志和安全诊断。
- Settings：上游 URL、模型行为、代理模式、UI 选项、计费单价和工具设置。
- Adapter：生成 Codex TOML 并展示模型目录状态。
- Tools：内置工具开关、Web Search、Vision 设置和 community tool discovery。

Proxy 才是核心服务。服务启动后，Codex 请求不应该依赖桌面 UI 是否打开。

## 工具与 Agent Runtime

CodeSeeX 将工具视为 Agent Runtime 的一部分，而不是普通 function call。

- Codex 客户端工具会以 Codex 期望的形态交还给 Codex 执行。
- 启用的 CodeSeeX 基础工具可以直接暴露给模型。
- Codex 延迟/原生工具仍可通过 tool-search bridge 发现。
- Web Search 是有边界的，具备 source 诊断，并阻止 localhost/private-network 目标。
- 工具结果会在 replay 前压缩，降低 token 污染风险。
- 重复失败和重复 tool signature 会被追踪，避免死循环，同时不阻断正常复杂工具链。

Community tool 是本地命令执行器，默认关闭。启用某个 community tool 意味着你信任它 manifest 中声明的命令。

## 上下文与用量

Codex 拥有会话 transcript。CodeSeeX 只保留完成当前请求和解释运行行为所需的 bridge state。

CodeSeeX 会重点处理：

- full-context Codex 请求，
- compact summary，
- 标题和 ambient suggestions 等服务请求，
- client tool handoff，
- 工具结果 replay，
- 可见 thinking 展示，
- cache hit / cache miss 统计，
- 按用户 turn 聚合用量。

这很重要。普通转发器可能表面能工作，但会静默重复发送大上下文、重复注入工具输出、误把后台服务请求当普通对话，或者把一个小用户任务拆成许多难以理解的 billable API 调用。

## 上游与模型

CodeSeeX 通过生成的 catalog 向 Codex 暴露 `deepseek-v4-pro` 和 `deepseek-v4-flash`。默认可使用 DeepSeek 兼容上游；也可以在 `Settings -> Proxy` 中设置自定义 OpenAI 兼容上游 URL。

默认本地 Codex endpoint 是 `http://127.0.0.1:8787/v1`。如果修改监听端口，请重新复制生成的 TOML 并重启 Codex。

## Vision 模块

Vision 模块是可选功能，可在桌面 Tools 设置中配置。你可以配置完整请求 URL、模型名和 API key：

- Analyze endpoint：OpenAI 兼容 `/responses` 或 `/chat/completions`。
- Generate endpoint：支持图像生成的 OpenAI 兼容 `/responses` 或 `/images/generations`。
- 图像输入：当前 Codex `input_image` 附件、HTTP(S) URL、`data:image` URL、`file://` URL、workspace 路径或允许的本地绝对路径。
- 图像生成结果会以可展示 Markdown 和本地文件返回；生成的 base64 payload 会保存到磁盘，而不是直接内联回传。

CodeSeeX 不会重写 Vision endpoint URL。你配置的请求 URL 就是实际使用的请求 URL。当本地图像通过远程 endpoint 分析时，图像像素会发送到你配置的服务。

## 安装与更新

Windows 用户建议使用 NSIS `CodeSeeX_*_setup.exe` 安装包进行普通桌面安装和更新。安装器支持语言选择、当前用户/所有用户安装模式，并会在安装 Tauri 版本前处理早期 Electron 版本的迁移。

## 凭据边界

CodeSeeX manager settings 不应被当作上游凭据存储。余额查询会读取 Codex auth 来源或已缓存的请求 `Authorization: Bearer ...` header。旧版 `DEEPSEEK_API_KEY` 环境变量仍可作为直接上游请求 fallback，但它不是余额凭据来源。

Vision 等工具专用凭据属于你配置的工具 endpoint，应视为本地 secret。除非你信任 community tool 的命令 manifest，否则不要启用它。

## 隐私说明

CodeSeeX 是本地 bridge，但模型请求会转发到你配置的上游服务。Vision 分析会把图像像素发送到配置的 Vision endpoint。Web Search 可能请求搜索结果页或普通网页。相关服务可能有自己的服务条款、留存策略、速率限制和反滥用规则。

默认日志是紧凑且脱敏的。开发诊断可能暴露更多 request shape 信息，只应在调试时开启。

## 运行数据

CodeSeeX 使用常规发布数据目录：

```text
~/.codeseex/
  config.toml
  model-catalog.json
  logs/
  extension/tools/
  secrets/
```

Store 保存当前进程 bridge state、有界日志、显式 compact payload material、用量摘要和诊断。它不是 Codex transcript storage 的替代品。

## 故障排查

### 余额查询失败

- 确认 Codex auth 已为同一用户配置。
- 确认当前机器可以访问配置的 DeepSeek 兼容上游。
- 如果需要系统代理或 VPN，请在 CodeSeeX 中启用系统代理模式。

### Codex 看不到 DeepSeek 模型

- 确认 `model_catalog_json` 指向存在的 `~/.codeseex/model-catalog.json`。
- 使用 CodeSeeX 生成的 TOML，不要手动拼路径。
- 修改 TOML 后重启 Codex。
- GPT/OpenAI 的 TOML 不需要 `model_catalog_json`，不受 CodeSeeX 影响。

### 对话请求失败

- 查看 CodeSeeX Logs 页面中的上游错误。
- 确认 Codex `base_url` 指向 CodeSeeX，例如 `http://127.0.0.1:8787/v1`。
- 如果使用自定义上游，确认该 URL 可访问且兼容 OpenAI API。
- 确认没有其它进程占用 CodeSeeX 配置的端口。

### 工具行为异常

- 确认该工具已在 CodeSeeX 设置中启用。
- 对于 Codex 原生工具，确认当前 Codex 会话本身支持并暴露该工具。
- 对于 community tool，请先检查 manifest 和命令。
- 对于 Web Search，请查看 source diagnostics 和网络/代理设置。

## 开发

核心 workspace 需要 Rust。

```sh
cargo run -p codeseex-proxy
cargo test --workspace
```

源码构建需要 model catalog seed。可以设置 `CODESEEX_MODEL_CATALOG_SEED` 指向本地 seed 文件，或将 `model-catalog.seed.json` 放在 `.private/` 下。

Windows helper scripts 会在可用时加载 MSVC Build Tools、导入 `.env`，并默认将 Cargo cache 放到可配置的本地开发目录：

```powershell
.\scripts\check-windows.ps1
.\scripts\start-desktop-windows.ps1
```

桌面 UI 通过 Tauri custom protocol 服务于 `apps/ui/public`，常规工作流中没有 Vite dev server。

## 文档

- Release notes 发布在 [GitHub Releases](https://github.com/TasteSteak/CodeSeeX/releases) 页面。
- [docs/architecture.md](docs/architecture.md)：Runtime 架构。
- [docs/installer-migration.md](docs/installer-migration.md)：安装器和旧版迁移行为。
- [docs/state-contract.md](docs/state-contract.md)：runtime/log 状态边界。
- [docs/community-tools.md](docs/community-tools.md)：community tool manifest 与执行规则。

## License

CodeSeeX 使用 AGPL-3.0-only 许可证。详见 [LICENSE](LICENSE)。
