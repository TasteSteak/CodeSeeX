# CodeSeeX 0.7.1：模型目录与定价数据化设计（工程书）

- 状态：设计稿（本轮**只做设计，不改代码**）
- 目标版本：0.7.1
- 触发背景：DeepSeek 即将发布新模型与新定价。当前「模型清单 / 上下文窗口 / 资费 / 峰谷规则」全部硬编码在二进制与前端里，跟进上游变化必须发一个完整客户端版本。
- 关联文档：`docs/deepseek-official-responses-v0.7.0-design.md`、`docs/state-contract.md`、`docs/image-capabilities.md`

---

## 1. 结论摘要

1. 模型与定价必须从「代码常量」变成「分层数据文档」，并对每个字段记录**来源与版本**。
2. 远程清单只负责「新模型 / 新价格 / 峰谷规则 / 上下文窗口」，本地**必须**自带一份完整可用目录（离线、断网、被墙、清单损坏都能正常工作）。
3. 上游 `GET /models` 只能作为**可用性探测**，不能作为元数据真源。它只返回 id，没有窗口、能力、定价；而且多数中转不实现该路由、或需要额外鉴权、或直接 401/404。把它当成真源会让「模型存在但没窗口没价格」和「探测失败就删模型」这两类事故同时发生。
4. 定价不能按「名字里含 flash / vision」猜分组（现状做法），必须由 slug 精确匹配 + 显式默认组 + 「未定价」显式状态。
5. 峰谷计费目前在两处各写一遍（Rust store 与前端 JS），必须收敛为**单一实现 + 快照**；否则改价会静默重算历史用量。
6. 与本次路由排查直接相关的结论（实测后修正，见附录 A.6）：上游请求头被最小化到 `Content-Type` / `Accept` / `Authorization`，`originator` / `User-Agent` 等客户端标识全部丢弃——中转据此判定「不是 Codex 客户端」并返回 `401 unauthorized client detected`，**这是主因**。同时凭据解析会在「Codex App 形态请求」上丢弃客户端 Authorization 并改用本地来源（`crates/proxy/src/upstream.rs:162-191`），**这是并发的第二处缺陷**。两者都需要在 0.7.1 一并修掉（见第 11 节、附录 A）。

---

## 2. 目标

- 模型、上下文窗口、能力开关、定价、峰谷规则全部由数据描述，代码只保留解析、校验、合并与消费逻辑。
- 支持远程更新（联网即更新），同时保留本地保底（内置 → 磁盘缓存 → 用户覆盖）。
- 支持用户本地覆盖（自定义中转的 slug、别名、自己实际支付的价格）。
- 上游 `GET /models` 的探测结果只影响「可用性标记」，不影响窗口/能力/定价，也绝不删除模型。
- 用量与成本估算**可复现**：历史记录不会因为之后调价而被改写。
- 远程清单损坏 / 不可达 / 版本过新时，产品必须继续可用，并在 UI 上如实说明当前数据来源。

## 3. 非目标

- 不在 0.7.1 内做在线账号体系、云同步或服务端定价计费。
- 不改变 Codex 侧 `model_catalog_json` 的文件契约（磁盘上的 `model-catalog.json` 结构保持不变，只改它的生成来源）。
- 不引入第二套代理/网关；本设计仍运行在现有本地 route 之上。
- 不把远程清单当作可信计费依据：UI 上展示的仍是**估算**成本。

---

## 4. 现行硬编码盘点

| 位置 | 现状 | 目标来源 |
| --- | --- | --- |
| `crates/core/src/models.rs:3-5` | `MODEL_FLASH` / `MODEL_PRO` / `DEFAULT_CONTEXT_WINDOW=1_000_000` / `95%` | 目录文档 `models[].context_window` 等字段 |
| `crates/core/src/models.rs:74-91` | `available_models()` 写死两项 | 目录解析结果 |
| `crates/core/src/models.rs:38-45` | `default_upstream_slug()`：空 → pro，`gpt-5*` → pro，其余原样 | 目录 + 别名表（`aliases` / `upstream_slug`） |
| `crates/core/src/models.rs:17-25` | `UpstreamModelOverride` 三值枚举 | 目录驱动的模型选择（保留兼容枚举，仅作 UI 回退） |
| `crates/core/src/catalog.rs:401-402` | `MODEL_FLASH → "Flash"`、`MODEL_PRO → "Pro"` | 目录 `short_display_name` |
| `crates/core/src/catalog.rs:440` | `catalog_model_is_default` 按 slug 判定 | 目录 `is_default` / `default_model` |
| `crates/core/src/catalog.rs:487-497` | `catalog_value_is_compatible` 要求 `models.len() == 2` 且必须含两个固定 slug | 改为「包含目录要求的必需 slug」，长度不设常数 |
| `crates/core/src/catalog.rs:467-485` | `codex_toml_snippet` 写死 `model = "deepseek-v4-pro"`、`name = "DeepSeek"` | 目录 `default_model` / `provider_name` |
| `crates/core/build.rs` + `.private/model-catalog.seed.json` | 编译期嵌入，`models` 手写两项，含大段 `base_instructions` | 拆分为「可公开目录」+「私有 overlay」两层（见 6.3） |
| `crates/proxy/src/manager_service.rs:1046-1047` | 回报 `context_window: 1_000_000`、`95%` | 目录字段 |
| `crates/proxy/src/manager_service.rs:1349` | 回报 `"default_model": "deepseek-v4-pro"` | 目录字段 |
| `crates/proxy/src/manager_service.rs:531-540` | 9 个 `BILLING_*` 硬编码默认值 | 定价表 `rates` |
| `apps/ui/public/index.html:468-545` | 固定三张资费卡（Flash / Pro / Vision） | 由目录生成行 |
| `apps/ui/public/app.js:32-37` | `DEFAULT_BILLING_RATES_CNY`、`BILLING_PEAK_MULTIPLIER = 2` | 后端下发的定价文档 |
| `apps/ui/public/app.js:4635-4655` | `currentBillingRates` 按名称子串 `vision/flash/else pro` 分组（**未知模型会静默按 Pro 计价**） | slug 精确匹配 + 默认组 + 未定价状态 |
| `apps/ui/public/app.js:4778-4785` | `isDeepSeekBillingPeakTime` 硬编码 09:00-12:00 / 14:00-18:00（北京） | 定价文档 `peak_valley.windows` |
| `crates/store/src/store.rs:3879-3898` | `usage_billing_period` 同样硬编码窗口 + `2.0` | 同一份峰谷规则的单一实现 |
| `crates/proxy/src/config_payload.rs:88-135` | 9 个 `BILLING_*` 读写 | 新结构 `[billing]`，旧键只读兼容 |
| `crates/proxy/src/community_tools.rs:23-36` | `BILLING_*` 键白名单 | 新键白名单 |
| `crates/proxy/src/codex_app.rs:885,949-950` | 注入脚本版本常量 + `deepseek-v4-flash → "Flash"` | 目录 `short_display_name`，版本常量随目录 revision 变化 |
| `crates/proxy/src/tools/vision.rs:28` | `DEEPSEEK_VISION_MODEL` 常量 | 目录（视觉条目），保留常量仅作离线回退 |
| `apps/ui/public/app.js:4687-4689` | `normalizeUpstreamModelOverride` 写死两个 slug | 目录 slug 列表 |

> 结论：硬编码分布在 4 层（core / proxy / store / UI），且**同一份语义在 Rust 与 JS 各写一遍**（峰谷窗口、默认价、分组规则）。这是 0.7.1 要收敛的核心。

---

## 5. 真源分层与优先级

| 层 | 名称 | 载体 | 作用 | 是否可离线 |
| --- | --- | --- | --- | --- |
| L0 | 内置目录 | 二进制内嵌 JSON | 保底真值，必须自洽完整 | 是 |
| L1 | 远程清单 | `catalog/model-catalog.json`（GitHub raw，可换镜像） | 新模型 / 新价格 / 新窗口 | 否 |
| L2 | 本地缓存 | `~/.codeseex/cache/model-catalog.json` + `*.meta.json` | 远程结果的持久化快照与 ETag | 是 |
| L3 | 用户覆盖 | `config.toml` 的 `[models.*]` / `[billing.*]` | 用户自己的 slug、别名与真实价格 | 是 |
| L4 | 上游探测 | `GET {base_url}/models` | 仅可用性标记 | 否 |

**合并优先级（高 → 低）**：L3 > L1 > L2 > L0。L4 不参与元数据合并。

**合并粒度**：以 `slug` 为身份键、**字段级**合并，而不是整对象替换。这样远程可以只更新价格而不覆盖用户改过的窗口，反之亦然。

**必需性校验**：合并结果必须至少包含一个 `is_default = true` 的模型；否则整份候选数据被拒绝，回退到下一层（不能把产品置于「没有任何可选模型」的状态）。

**上游探测的定位**：只在模型选择器与设置页显示 `listed / not_listed / unknown` 三种状态。`not_listed` 不隐藏模型，`unknown`（探测失败、401、404、超时、不实现）与 `not_listed` 必须在语义上区分，绝不触发删除或自动切换 transport。

---

## 6. 目录文档结构

### 6.1 信封（envelope）

```json
{
  "schema_version": 1,
  "revision": "2026-09-10.1",
  "issued_at": "2026-09-10T00:00:00Z",
  "min_app_version": "0.7.1",
  "provider_name": "DeepSeek",
  "default_model": "deepseek-v4-pro",
  "models": [],
  "pricing": {}
}
```

`revision` 是单调递增的字符串（日期序号即可），用于：缓存比对、用量快照、UI 变更提示、注入脚本版本常量。

### 6.2 模型条目

```json
{
  "slug": "deepseek-v4-pro",
  "display_name": "DeepSeek V4 Pro",
  "short_display_name": "Pro",
  "description": "...",
  "context_window": 1000000,
  "max_context_window": 1000000,
  "effective_context_window_percent": 95,
  "is_default": true,
  "visibility": "list",
  "input_modalities": ["text", "image"],
  "supported_reasoning_levels": [{ "effort": "medium", "description": "..." }],
  "default_reasoning_level": "medium",
  "aliases": ["deepseek-v4", "gpt-5.4"],
  "upstream_slug": "deepseek-v4-pro",
  "pricing_group": "pro",
  "source": "remote"
}
```

规则：

- `slug` 唯一、非空、区分大小写。
- `aliases` 用于**入站解析**：客户端请求 `gpt-5*`、老 slug、中转自定义名时，先查别名再查 slug，再走「未知名透传」（保持现状行为）。
- `upstream_slug` 用于**出站改写**。为空则等于 `slug`。这替代了 `default_upstream_slug()` 里的硬编码分支。
- `pricing_group` 只在定价表匹配不到精确 slug 时作为显式回退，且 UI 必须标注「按组定价」。
- 其余 Codex 侧字段（`shell_type`、`truncation_policy`、`apply_patch_tool_type` 等）原样保留并透传，保证 `model-catalog.json` 契约不变。

### 6.3 私有 overlay 与公开目录的拆分

现状：`.private/model-catalog.seed.json` 既含公开元数据，也含大段 `base_instructions`（模型人格/提示词），靠 CI secret 还原（`crates/core/build.rs`）。

0.7.1 拆分：

- **公开目录**：`catalog/model-catalog.json`，进仓库、可远程分发、可被用户覆盖。
- **私有 overlay**：`common_model_fields`（`base_instructions` 等），继续作为编译期私有输入，**不**进远程清单。

合并时以 slug 为键做一次浅合并，overlay 只覆盖它声明的字段。远程清单永远不能改写本地 overlay 的提示词字段。

### 6.4 磁盘产物

`~/.codeseex/model-catalog.json`（Codex 读取的文件）结构不变，内容改为「合并解析结果」的投影。写入仍使用现有的原子写路径（`write_catalog_atomic`），并保留 `catalog_file_is_compatible` 的兼容检查（放宽后版本）。

---

## 7. 定价与峰谷计费设计

### 7.1 数据结构

```json
{
  "pricing": {
    "revision": "2026-09-10.1",
    "effective_from": "2026-09-10T00:00:00Z",
    "currency": "CNY",
    "unit": "per_1m_tokens",
    "peak_valley": {
      "enabled": true,
      "timezone": "Asia/Shanghai",
      "multiplier": 2.0,
      "windows": [
        { "from": "09:00", "to": "12:00" },
        { "from": "14:00", "to": "18:00" }
      ]
    },
    "rates": {
      "deepseek-v4-pro": { "cached_input": 0.025, "cache_miss_input": 3.0, "output": 6.0 },
      "deepseek-v4-flash": { "cached_input": 0.02, "cache_miss_input": 1.0, "output": 2.0 },
      "deepseek-v4-flash-vision-exp": { "cached_input": 0.05, "cache_miss_input": 1.5, "output": 4.5 }
    },
    "groups": { "default": { "cached_input": 0.0, "cache_miss_input": 0.0, "output": 0.0 } }
  }
}
```

规则：

- 未知模型**不得**静默套用 Pro 价（现状 `apps/ui/public/app.js:4637` 的 `else -> pro` 是错误定价来源）。未定价时成本显示为「未定价」而不是一个数字。
- `unit` / `currency` 显式声明，UI 文案由它们生成（不再写死「CNY / 1M tokens」）。
- 定价按 `effective_from` 生效；未来生效的条目在生效前只做提示，不参与计算。
- 峰谷窗口用「分钟边界语义」明确定义：`[from, to)` 左闭右开，避免 12:00 与 14:00 的边界歧义（当前两处实现都是左闭右开，需在文档与测试中固化）。

### 7.2 单一实现

峰谷判定与成本计算下沉到 Rust 一处（例如 `crates/core` 的 pricing 模块），产出：

- `billing_period`（`peak` / `off_peak`）、`billing_multiplier`
- 该次请求的**单价快照**（`cached_input` / `cache_miss_input` / `output`）与 `pricing_revision`

前端只做「渲染与求和」，不再自己判断北京时间窗口，也不再自己决定分组。前端保留的本地常量仅作为「后端未下发时的显示回退」，并且必须在 UI 上标注为回退值。

### 7.3 历史可复现

现状：`costForTokens` 每次渲染都用**当前**费率重算全部历史（`apps/ui/public/app.js:4735`），一旦调价，历史成本会整体变化，与用户当日实际账单不符。

建议（0.7.1 采用 A）：

- **A（推荐）**：在 usage 行/段记录单价快照 + `pricing_revision`。渲染历史优先用快照；缺失快照的旧数据回退当前表并打「估算」标记。
- **B**：只记录 `pricing_revision`，UI 按 revision 查历史价目表。需要长期保留全部历史价目表，复杂度更高。

---

## 8. 上游探测（`GET /models`）设计

- 请求：`GET {base_url}/models`，鉴权与推理请求同源（见第 11 节），`Accept: application/json`，超时 5s，最多解析 500 条。
- 兼容形态：`{data:[{id}]}`、`{data:[{id, ...}]}`、`{data:[{model}]}`、`{models:[...]}`、裸数组。
- 缓存：按 `base_url + 凭据指纹` 缓存 10 分钟，提供手动「立即探测」。
- 失败语义：401 / 403 / 404 / 超时 / 非法 JSON 一律记为 `unknown` 并写入诊断，**不**改变模型可用性结论，**不**改变 transport，**不**改变目录。
- 交叉校验（可选开关 `model_discovery = verify`）：把「目录有、上游未列出」的模型标记为 `not_listed`，提示用户可能是中转未开放或命名不同，并提供「自定义 slug/别名」入口。
- 绝不写入磁盘目录：探测结果只存在内存缓存与诊断中。

---

## 9. 远程拉取与本地保底

### 9.1 复用既有形态

0.7.0 的 release notes 已经实现了「HTTP + ETag + TTL + 超时 + 内置回退」的完整链路（`crates/proxy/src/manager_service.rs:775-830`，常量见 `:25-31`）。目录拉取应当**同构**实现，而不是另起一套：

```text
内置目录（L0）
  -> 磁盘缓存（L2，命中且新鲜则直接用）
  -> 远程清单（L1，带 If-None-Match；304 则刷新 TTL）
  -> 失败：保留缓存 + 记录诊断 + 继续用可用数据
```

### 9.2 拉取时机与 URL

- 启动后延迟拉取（不阻塞窗口打开与代理启动）；成功后若 `revision` 变化，通知 UI 并重写 `model-catalog.json`。
- 默认源：与 release notes 同域的 GitHub raw 路径（`catalog/model-catalog.json`），支持用户自定义镜像 URL。
- 请求不携带任何上游凭据；`User-Agent: CodeSeeX`；重定向限制不超过 3 次。

### 9.3 校验（拒绝即回退）

| 校验项 | 规则 | 失败动作 |
| --- | --- | --- |
| `schema_version` | 必须等于当前支持值 | 忽略远程，保留缓存 |
| `min_app_version` | 小于等于当前版本 | 忽略远程，UI 提示「需要更新 CodeSeeX」 |
| 文档大小 | 不超过 256 KB | 忽略 |
| `slug` | 非空、唯一、字符集受限 | 忽略 |
| 数值字段 | 有限、非负；`multiplier` 在 [1, 100] | 忽略 |
| 时间窗口 | `HH:MM` 合法且 `from < to` | 忽略 |
| 必需模型 | 至少一个 `is_default` | 忽略 |

「忽略」的含义是：本次候选数据整体丢弃，继续使用上一份可用数据（缓存或内置），并记录 `catalog_remote_invalid` 诊断。

### 9.4 原子写与并发

- 先写 `*.tmp` 再 rename 覆盖，避免半个文件被 Codex 读到。
- 拉取与合并串行化（单飞），避免并发写同一文件。
- 写盘前比对内容哈希，无变化则不写、不通知 UI。

### 9.5 信任与安全

- 只信任固定 HTTPS 源（或用户显式配置的镜像）。
- 目录内**禁止**任何 secret 字段；解析器对未知字段容忍，但对可疑字段（`*_key`、`*_token`、`authorization`）记录告警。
- 0.7.x 可选增强：复用 updater 公钥对目录做 detached signature，验签失败即拒绝。0.7.1 先落地「HTTPS + schema 校验 + 原子写」，签名作为后续增量。

---

## 10. 配置与用户覆盖

```toml
[models."deepseek-v5-pro"]
display_name = "DeepSeek V5 Pro"
context_window = 2000000
upstream_slug = "deepseek-v5"
aliases = ["gpt-5.9"]

[billing]
peak_valley_enabled = true
peak_multiplier = 2.0
peak_windows = ["09:00-12:00", "14:00-18:00"]
timezone = "Asia/Shanghai"

[billing.rates."deepseek-v5-pro"]
cached_input = 0.03
cache_miss_input = 4.0
output = 8.0
```

- 用户覆盖只写「与默认不同的字段」。
- UI 提供「恢复远程/内置默认」（按行、按字段）。
- 0.7.0 的 9 个 `BILLING_*` 键：**可读不写**。读取时映射到对应 slug 的 `rates`；写回时只写新结构。保留一个版本后再移除。

---

## 11. 上游档案与凭据绑定（与本次 401 排查同源）

把「上游」从散落字段收敛成一个显式档案对象：

```toml
[upstream]
base_url = "https://relay.example.com/v1"
transport = "chat_compat"                    # auto | native_responses | chat_compat
credential = "secret-store:upstream_api_key" # 或 "env:DEEPSEEK_API_KEY" / "codex-auth" / "request"
official = false
pricing_group = "relay-a"
```

- 凭据来源必须**显式**且随档案切换。现状的隐式顺序（`upstream.api_key` -> auth.json）会在换上游时沿用旧凭据，是 401 的直接来源之一。
- 新增 `POST /manager/upstream/test`：返回「实际使用的凭据来源 + 请求 URL + 上游状态码 + 上游错误摘要（脱敏）」。这是排查这类问题的关键工具，且不泄露 key。
- 默认策略建议：**非官方 endpoint 优先透传客户端 Authorization**；官方 endpoint 保留现有隔离策略（`crates/proxy/src/upstream.rs:162-191`）。理由见附录 A。
- 模型别名映射归入档案：同一份目录在不同中转上可能要用不同 `upstream_slug`，档案级映射优先于目录级默认值。

---

## 12. UI / UX 要求

1. 设置页「模型」区：由目录生成列表，每行展示 slug、显示名、**来源标签**（内置 / 远程 / 用户）、上下文窗口、能力、定价、上游探测状态。
2. 顶部展示「数据更新时间 / 来源 / revision」与「立即更新」按钮；失败时展示原因与上一次成功时间，不弹阻断式错误。
3. 定价区不再固定三张卡：行由目录生成，可编辑、可恢复默认，未定价行显式标记。
4. 峰谷卡片：开关 + 时区 + 窗口 + 倍数，全部可编辑并回显生效值。
5. revision 变化后，在 Usage 页给出一次性、可关闭的提示：「价目已更新（revision A -> B）；历史记录按当时价格保持稳定」。
6. 所有成本文案保留「估算」语义；货币与单位字符串由数据生成。

---

## 13. 迁移与兼容（0.7.0 -> 0.7.1）

- `BILLING_*` 旧键映射到新结构；不删除用户已填数值。
- `catalog_value_is_compatible` 放宽为「必需 slug 全覆盖」，避免用户目录被反复判定不兼容而重写。
- `model-catalog.json` 磁盘契约不变；生成源改为合并结果，注入脚本版本常量改为跟随 `revision`。
- 首个 0.7.1 版本的内置目录必须包含 DeepSeek 新模型（新 slug / 新窗口 / 新价格），以保证完全离线时也能正确展示与计价。
- `UPSTREAM_MODEL_OVERRIDE` 三值枚举保留为 UI 回退，但选项列表改为目录驱动。

---

## 14. 测试矩阵

**零成本（必须全绿）**

- 分层合并：用户 > 远程 > 缓存 > 内置；字段级覆盖；缺失默认模型时逐层回退。
- 校验拒收：schema 版本、`min_app_version`、超大文档、重复 slug、负价、`multiplier` 越界、非法时间窗口、无默认模型。
- 上游探测：200 / 401 / 403 / 404 / 超时 / 非 JSON / 非 OpenAI 形态，结果只影响 `listed / not_listed / unknown`。
- 峰谷边界（北京）：08:59 / 09:00 / 11:59 / 12:00 / 13:59 / 14:00 / 17:59 / 18:00 / 23:59，Rust 与前端判定必须一致。
- 未定价模型：不套用 Pro 价，UI 显示未定价。
- 历史可复现：修改价目后，旧记录成本不变（或明确标记为估算回退）。
- 离线：断网启动、缓存损坏、只读缓存目录、远程返回 304。
- 别名与 `upstream_slug`：请求 `gpt-5*` / 老 slug / 未知 slug 的出站改写正确。
- 原子写：并发拉取不产生半个文件。

**小额真实验证**

- 用真实 DeepSeek 官方 endpoint 与一个自定义中转各跑一次对话，确认窗口、计价、峰谷标记与来源标签正确。
- 在自定义中转上验证 `POST /manager/upstream/test` 报告的凭据来源与实际发送的 Authorization 一致。

---

## 15. 发布门槛与回滚

- 门槛：内置目录自洽（离线可跑）；旧 `BILLING_*` 配置迁移后数值不丢失；远程清单不可达时无阻断性错误。
- 可回滚性：目录文件与客户端版本可分别回滚；远程清单可下发「回退版」revision（内容为上一版），无需发版。
- 观测：新增 `catalog_remote_*`、`catalog_merge_*`、`upstream_probe_*` 三类诊断事件，均脱敏。

---

## 附录 A：`401 unauthorized client detected` 排查记录（0.7.1 需一并处理）

### A.1 现象

更换上游地址后，客户端报：

```text
unexpected status 401 Unauthorized: unauthorized client detected, contact support for assistance at https://discord.gg/HgekCyHJqB, url: http://127.0.0.1:8787/v1/responses
```

### A.2 事实（代码可证）

1. `unexpected status {status}: {body}, url: {url}` 是 **Codex 客户端**的错误格式（`.private/codex-source-rust-v0.146.0/codex-rs/protocol/src/error.rs:552` 附近）。其中 `url` 是 Codex 自己请求的地址，即本地代理 `http://127.0.0.1:8787/v1/responses`。
2. 这个 `url` **不**说明请求绕过了代理或发回了本机；它只说明「Codex -> 本地代理」这一跳，代理再把上游返回的 401 **原样透传**回来（`crates/proxy/src/server/native_runtime.rs:451-517` 把上游 body 字节直接回给客户端；Chat 兼容路径同理）。
3. `unauthorized client detected ... discord.gg/HgekCyHJqB` 这段文案在 CodeSeeX 仓库内不存在，属于**上游中转**返回的错误体。
4. 因此结论是：请求确实走了本地路由并到达了新上游，是新上游拒绝了这次请求的身份/凭据。

### A.3 本地路由实际改动了什么（按嫌疑排序）

1. **Authorization 被替换或丢弃（最可能）**：`crates/proxy/src/upstream.rs:162-191` 对「看起来像 Codex App 的请求」显式**不转发**客户端 Authorization（`payload_looks_like_codex_app_request` 命中 `client_metadata` / `prompt_cache_key` / `metadata.x-codex-installation-id` 任一即为真，而 Codex 的请求恒定命中）。此时凭据只能来自 `DEEPSEEK_API_KEY` 环境变量或 `~/.codex/auth.json`。若用户只在 Codex 侧填了新中转的 key、而 CodeSeeX 侧没有对应凭据来源，上游收到的就是**没有 Authorization**、或**旧供应商的 key**，中转随即返回「unauthorized client」。该行为有测试固化：`crates/proxy/src/upstream.rs:371-389`。
2. **模型 slug 被改写**：`crates/core/src/models.rs:38-45` 会把 `gpt-5*` 一律改写成 `deepseek-v4-pro`，空值也改写为 pro。中转若按「不允许的模型」拒绝，也会表现为 401/403 类错误（需与错误体文案一起判断）。
3. **请求头被最小化**：转发只带 `Content-Type` / `Accept` / `Authorization`（`crates/proxy/src/upstream.rs:90-104` 与 `:123-137`），`originator`、`user-agent`、`chatgpt-account-id`、`session_id` 等一律不带。若中转按客户端标识做白名单，这会被判为「unrecognized client」。
4. **路径拼接**：`crates/core/src/urls.rs:22-49` 对自定义 base 只做 `normalize_base_url` + 追加 `/responses` 或 `/chat/completions`。若用户填的地址已带完整路径（例如以 `/v1/chat/completions` 结尾）或缺少 `/v1`，会得到错误路径；部分网关对未知路径直接回 401。
5. **请求体被重建**：Chat 兼容路径会重建 messages/tools；原生路径会移除 `id` / `previous_response_id` 并注入工具（`crates/proxy/src/server/native_runtime.rs:423-450`）。若「unauthorized client」来自中转让，body 差异也可能触发其风控。

### A.4 两分钟定位步骤

1. 打开 CodeSeeX Logs，查找最近一次失败的 `request_failed` 事件；其 `upstream_error.message` 即为上游原始文案（`crates/proxy/src/server/response_helpers.rs:31-45`）。
2. 在 Logs 里确认该次请求被记为 `native_responses` 还是 `chat_compat`，以及 `model` 与 `requested_model` 的实际取值。
3. 用 `curl` 直接打新中转（同一 base_url、同一 key，`/v1/chat/completions` 与 `/v1/models` 各一次）：
   - 直接 curl 成功、经代理失败：差异在 CodeSeeX 侧（回到 A.3 的 1/3/4/5）；
   - 直接 curl 也 401：凭据或中转账号问题，与本地路由无关。
4. 对比「CodeSeeX 实际发出的 Authorization」与「用户期望的 key」：当前实现没有可见性，这正是 0.7.1 要补 `POST /manager/upstream/test` 的原因。

### A.5 0.7.1 的处理决定（建议）

- 自定义（非官方）endpoint：**默认透传**客户端 Authorization，不再因「Codex App 形态」而丢弃；仅在官方 endpoint 保留隔离。
- **追加（实测后必要）**：把入站 `originator` / `user-agent` / `session_id` / `conversation_id` 原样转发上游。中转普遍按「调用方是否为 Codex 客户端」做白名单，仅修 Authorization 不足以消除 `unauthorized client detected`，详见 A.6。
- 凭据来源显式化并随上游档案绑定；换上游地址时提示「凭据未变更」。
- `POST /manager/upstream/test` 返回凭据来源、最终 URL、状态码与脱敏错误摘要。
- 上游 4xx 透传时，在响应头或诊断中附加 `x-codeseex-upstream-status` 与 `x-codeseex-credential-source`，让客户端报错可自解释。

### A.6 实测结论（0.7.1 落地时复核）

以真实第三方中转 endpoint 复测后，A.3 的嫌疑排序需要修正：**第 3 条（请求头被最小化）才是主因**，第 1 条（Authorization 被丢弃）是并发的第二处缺陷。

同一 key、同一 `/v1/responses`、同一模型，只改请求头：

| 直连请求携带的头 | 上游结果 |
| --- | --- |
| 仅 `Authorization` | `401 unauthorized client detected` |
| `+ originator: codex_cli_rs` | `200` |
| `+ User-Agent: codex_cli_rs/0.1.0` | `200` |
| `originator: zzz-not-codex` / `x` / 空 | `401` |
| `User-Agent: curl/8.0` | `401` |
| 只有 `x-api-key`、没有 `Authorization` | `401` |

即该中转按「调用方是否是 Codex 客户端」做白名单：`originator` 或 `User-Agent` 必须呈现 Codex 客户端身份。0.7.0 的上游请求只发 `Content-Type` / `Accept` / `Authorization`，所以**凭据完全正确也必然 401**，与用户「只负责路由、不该改动任何内容」的判断完全一致。

代码复核（`2f9c7a1`）：

- `crates/proxy/src/upstream.rs` 构造上游请求头时只有四行 `insert`，`originator` / `user-agent` / `session_id` 全部丢弃。
- 同一处的 `can_use_inbound = !payload_looks_like_codex_app_request(payload)` 对**任意** endpoint 生效，而 Codex 请求恒定命中 `client_metadata`，因此客户端 Authorization 也被丢弃并回落到 `auth.json` 凭据。

0.7.1 的修复（两处必须一起做）：

1. `UpstreamPassthrough`：把入站 `originator` / `user-agent` / `session_id` / `conversation_id` 原样转发上游，不合成、不改写、不新增。
2. 隔离策略收窄到官方 endpoint：`codex_app_isolation_applies = upstream_is_official(upstream) && payload_looks_like_codex_app_request(payload)`；自定义 endpoint 始终透传客户端 Authorization。

回归证据：

- 单测：`post_forwards_client_identity_headers_verbatim`、`passthrough_keeps_only_non_empty_identity_headers`、`custom_endpoint_forwards_client_authorization_for_codex_app_payloads`、`official_endpoint_still_isolates_codex_app_credentials`。
- 实测：同一个 Codex 形态 `/v1/responses` 经本地代理转发真实中转，带 `originator` → `200`（返回真实补全），不带 → `401`。

### A.7 与本次排查无关、但实测中确认的上游事实

- 该中转的 `/v1/models` 对同一 key 恒返回 401，`/v1/chat/completions` 返回 404，只有 `/v1/responses` 可用。这印证第 3 节：**上游 `GET /models` 不能作为元数据真源**，只能当作可用性探测。
- 该 key 对该中转的 `deepseek-v4-pro` 无授权（上游 403「该令牌无权访问模型 deepseek-v4-pro」），`deepseek-v4-flash` 正常。这类错误由 CodeSeeX 原样透传，不属于本地路由缺陷。

---

## 附录 B：参考

- `docs/deepseek-official-responses-v0.7.0-design.md`（原生 Responses 边界与 0.7.0 决策）
- `crates/proxy/src/manager_service.rs:25-31,775-830`（release notes 的远程拉取 / ETag / 超时 / 回退范式）
- `crates/core/src/catalog.rs`、`crates/core/src/models.rs`、`crates/core/src/urls.rs`
- `crates/store/src/store.rs:3873-3898`（峰谷桶现状）
- `apps/ui/public/app.js`（定价与峰谷现状）
