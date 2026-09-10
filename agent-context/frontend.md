# 前端代理上下文：模型列表 / 目录 / 定价的 UI（CodeSeeX 0.7.1）

> 本文件是写给「前端代理」的上下文简报。后续代理在本仓库做 **UI 方面优化**时，先读本文件；它聚焦「模型列表拉取 / 模型目录 / 定价」相关的 UI 文件位置、数据流、交互与已知优化点。后端结构与无关页面保持不变，不要顺手重构。

## 1. 相关文件位置（先认路）

| 文件 | 作用 | 关键位置 |
| --- | --- | --- |
| `apps/ui/public/index.html` | 单页 UI 骨架：侧栏 5 个视图（console / usage / logs / config / about） | 目录区块 :488-496；费率网格 `#billingRateGrid` :474；峰谷输入 :475-488；`BILLING_*` 输入 |
| `apps/ui/public/app.js` | 全部前端逻辑（5269 行），无框架、原生 DOM | 函数清单见 §2 / §3 |
| `apps/ui/public/lang/*.json` | 9 个语言包（`de_de/en_us/fr_fr/ja_jp/ko_kr/ru_ru/zh_cn/zh_hk/zh_tw`） | 新增 key 见 §5 |
| `apps/ui/public/styles/...` | 19 个 CSS：`foundation/`、`layout/`、`components/`、`pages/`、`features/config/` | 定价/目录样式在 `styles/features/config/billing.css` |
| 后端数据来源 | UI 不直连上游，全部走后端 manager API | `/api/config`（注入 `CATALOG`/`CATALOG_STATUS`）、`/api/catalog`、`/api/models`、`/codeseex/renderer-inject.js`（Codex App 注入） |

UI 相关后端注入点（前端代理只需知道数据从哪来）：
- `crates/proxy/src/manager_service.rs:641-689`：config payload 注入 `CATALOG`（目录文档）、`CATALOG_STATUS`、`CATALOG_REVISION`、`CATALOG_SOURCE`、`CATALOG_PRICING`、`CATALOG_MODELS`、`UPSTREAM_API_KEY_CONFIGURED` 等。
- `crates/proxy/src/codex_app.rs:96/130/131`：Codex 侧模型目录注入脚本（`renderer_inject_script`，:885），UI 之外的模型选择层。

## 2. 数据流（谁拉谁渲染）

### 加载
1. `init()`（:284）→ `loadConfig()`（:1180）→ `renderConfig()`（:1422）。
2. `applyCatalogPayload(config.CATALOG, config.CATALOG_STATUS)`（:1449，定义 :4827）把目录写入 `catalogState`（:53-63）：`revision / source / providerName / defaultModel / currency / unit / models / pricing / status`。
3. `renderCatalogStatus()`（:4840）渲染目录状态；`renderBillingRateGrid()`（:4684）渲染每模型费率行。

### 目录刷新（模型列表拉取的入口）
- 「Update now」`#catalogRefreshButton` → `refreshCatalogDocument()`（:587）→ `POST /api/catalog/refresh` → 重新 `loadConfig()` 刷新 `catalogState`。
- 「Test upstream」`#upstreamTestButton` → `testUpstreamCredential()`（:608）→ `POST /api/upstream/test`，结果写入 `#catalogStatusJson`。
- 保存配置时会跳过 `READ_ONLY_CONFIG_KEYS`（:51-53：`CATALOG` / `CATALOG_MODELS` / `CATALOG_STATUS`），这些只读注入字段不会被 UI 回写。

### 定价渲染与保存
- 费率来源：`catalogRateFor(model)`（:4861）＝ 精确 slug → `pricing.groups` 分组 → `null`（与后端 `PricingTable::rate_for` 同规则）。
- 渲染：`renderBillingRateGrid()`（:4684）遍历 `catalogState.models`，每模型一行三个费率输入；未定价显示 `billingUnpriced` 徽标，分组回退显示 `billingGroupPriced` 徽标。
- 峰谷：`catalogPeakValley()`（:4882）读 `catalogState.pricing.peak_valley`；`isPeakBillingTime()`（:5063）判定；输入框 `BILLING_PEAK_MULTIPLIER` / `BILLING_TIMEZONE` / `BILLING_PEAK_WINDOWS`。
- 保存：`catalogRateOverrides()`（:4751）+ `catalogPeakPricingPayload()`（:4770）→ payload 里的 `CATALOG_PRICING`（:4370）→ 后端 `apply_catalog_pricing_payload`（`config_payload.rs:162`）写入 `[billing]`。
- 用量页成本：`costForTokens()`（:4995）→ `ratesForTokens()`（:5005）→ 未定价返回 `null` → `formatCostOrUnpriced()`（:5011）显示「未定价」；`sumCosts()`（:5015）聚合；`normalizeRateInput()`（:4906）容错。

## 3. 模型列表 / 目录 UI 现状

`index.html` 的目录区块（:488-496）目前是极简形态：

```html
<span data-i18n="catalogSection">Model catalog</span>
<small id="catalogStatusText" data-i18n="catalogStatusHint">Source, revision, and remote refresh</small>
<button id="catalogRefreshButton" data-i18n="catalogRefresh">Update now</button>
<button id="upstreamTestButton" data-i18n="catalogUpstreamTest">Test upstream</button>
<pre class="catalog-status-pre" id="catalogStatusJson"></pre>
```

现状要点：
- 状态是**原始 JSON**（`#catalogStatusJson`），未结构化展示。
- **没有目录 URL 的可视化配置入口**（只能改 `config.toml` 的 `[catalog] source_url` 或环境变量）。
- UI 没有模型选择器；模型选择发生在 Codex 客户端侧（经 `codex_app` 注入的 `model_catalog_json`）。UI 只消费模型列表用于定价行渲染。
- 目录模型信息（`aliases` / `alias_patterns` / `upstream_slug` / `pricing_group`）已从 `/api/catalog` 下发，但 UI 目前没有展示这些字段。

## 4. 定价 UI 现状

- `#billingRateGrid`：每模型一行（模型显示名 + `cached_input` / `cache_miss_input` / `output` 三个数字输入 + 状态徽标）。
- `#billing-peak-grid`：峰值倍数 / 时区 / 时段（逗号分隔文本 `09:00-12:00, 14:00-18:00`）。
- 开关 `BILLING_PEAK_VALLEY_ENABLED`；单位/货币文案来自 `catalogState.unit` / `catalogState.currency`（不再写死「CNY / 1M tokens」）。
- 兜底常量：`FALLBACK_BILLING_RATES`（全 0）与 `FALLBACK_PEAK_VALLEY`（:34-44）仅在 `catalogState.pricing` 缺失时使用，且语义是「后端未下发时的显示回退」。

## 5. 语言包 keys（0.7.1 新增，9 个语言文件需同步）

`en_us` 参考值：

```text
billingGroupPriced   = "Group priced"
billingPeakMultiplier= "Peak multiplier"
billingPeakWindows   = "Peak windows"
billingTimezone      = "Timezone"
billingUnpriced      = "Unpriced"
catalogRefresh       = "Update now"
catalogRevisionLabel = "Revision"
catalogSection       = "Model catalog"
catalogSourceLabel   = "Source"
catalogStatusHint    = "Source, revision, and remote refresh"
catalogUpstreamTest  = "Test upstream"
```

改动文案或新增 key 时，必须同步全部 9 个语言文件（缺失 key 会回退到英文默认）。

## 6. 样式

- `apps/ui/public/styles/features/config/billing.css`：本次新增 `.billing-rate-grid`、`.billing-peak-grid`、`.catalog-toolbar`、`.catalog-status-pre` 等。
- 样式分层：`foundation/theme.css`（主题变量）、`foundation/base.css`、`layout/app-shell.css` + `layout/sidebar.css`、`components/*.css`、`pages/*.css`。新 UI 应尽量复用组件类（`btn` / `setting-item` / `panel-*` / `mt-12` 等），不要只在页面级 CSS 里堆私有类。
- 深色主题一致性：新类（尤其 `catalog-status-pre`）应跟随 `theme.css` 的 CSS 变量。

## 7. 约束（前端代理必须知道）

- **UI 被编译进二进制**：语言包是 `include_str!`（`apps/desktop/src-tauri/src/lib.rs:1122-1130`），`tauri.conf.json` 的 `frontendDist: "../../ui/public"`，窗口走 `WebviewUrl::App("index.html")`。改 HTML/CSS/JS/语言包后**必须重新构建**才能看到效果：
  ```powershell
  cd D:\桌面文件\codeseex-next
  .\scripts\start-desktop-windows.ps1 -DevRoot "D:\DevTools\CodeSeeXNext"
  ```
  （或仅校验语法：`node --check apps/ui/public/app.js`。）
- 不要在前端写死模型列表或费率；数据一律来自 `catalogState`（后端注入的目录文档）。
- 不要改变 `model-catalog.json`（Codex 侧契约）的生成规则；UI 只消费后端下发的数据。
- 修改 `app.js` 时保持无框架、原生 DOM 的风格，与现有函数命名一致。

## 8. 优化方向（给前端代理的清单，按价值排序）

1. **目录状态结构化展示**：把 `#catalogStatusJson` 的裸 JSON 渲染成来源 / 版本 / 模型数 / 最近成功时间 / 最近失败原因的可读区块（数据已在 `catalogState.status` 与 `CATALOG_STATUS` 中）。
2. **目录 URL 可视化配置**：增加「远端清单 URL」输入（映射到 `CODESEEX_CATALOG_URL` / `[catalog] source_url`），并提示「off」可关闭远程拉取。
3. **模型目录信息展示**：在目录区块或费率行里展示 `aliases` / `alias_patterns` / `upstream_slug` / `pricing_group`，帮助用户理解「客户端模型名 → 上游模型名」的映射（数据已下发，只缺渲染）。
4. **未定价/组定价的友好提示**：未定价（`billingUnpriced`）与分组回退（`billingGroupPriced`）目前是文字徽标，可加悬停说明；用量页成本「未定价」同理。
5. **峰谷窗口可视化**：当前是逗号分隔文本输入，可考虑按天的时间轴/分段编辑。
6. **费率编辑体验**：数字输入的校验（>0）、保存成功/失败反馈、与后端 `[billing]` 覆盖值的同步提示。
7. **多语言与无障碍**：新增 key 全语言同步；深色主题下核对 `catalog-status-pre` 等新类对比度。

## 9. 验证方法

- 语法：`node --check apps/ui/public/app.js`；JSON：`node -e "JSON.parse(require('fs').readFileSync('apps/ui/public/lang/zh_cn.json','utf8'))"`。
- 功能：按 §7 重建并启动桌面客户端 → 设置页「Model catalog」区块操作「Update now / Test upstream」→ 观察状态区；Usage 页成本列与未定价显示。
- 后端数据对照：`GET http://127.0.0.1:8787/api/catalog` 与 `GET /api/config` 里 `CATALOG` / `CATALOG_STATUS` 的值应与 UI 渲染一致。

## 10. 红线（不要改）

- 不在前端写死模型/定价/峰谷窗口；`FALLBACK_BILLING_RATES` 只能全 0 作显示回退。
- 不绕过后端直接拉远程清单；UI 只走后端 manager API。
- 不改 `model-catalog.json` 契约；不动 Codex 注入脚本的既有注入格式。
- 不在本主题外顺手重构其它页面。
