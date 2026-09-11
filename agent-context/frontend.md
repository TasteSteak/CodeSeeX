# 前端代理上下文：模型列表 / 目录 / 定价的 UI（CodeSeeX 0.7.1）

> 本文件是写给「前端代理」的上下文简报。后续代理在本仓库做 **UI 方面优化**时，先读本文件；它聚焦「模型列表拉取 / 模型目录 / 定价」相关的 UI 文件位置、数据流、交互与已知优化点。后端结构与无关页面保持不变，不要顺手重构。

## 1. 相关文件位置（先认路）

| 文件 | 作用 | 关键位置 |
| --- | --- | --- |
| `apps/ui/public/index.html` | 单页 UI 骨架：侧栏 5 个视图（console / usage / logs / config / about） | 侧栏状态药丸 + 内联控制图标按钮 :39-50；页面容器 `.page-container` :71（收口 :615）；「计费模式费率设置」区块 :460-469：拉取按钮 + 模型列表 + 费率卡 |
| `apps/ui/public/app.js` | 全部前端逻辑（约 5400 行），无框架、原生 DOM | 函数清单见 §2 / §3 |
| `apps/ui/public/lang/*.json` | 9 个语言包（`de_de/en_us/fr_fr/ja_jp/ko_kr/ru_ru/zh_cn/zh_hk/zh_tw`） | 新增 key 见 §5 |
| `apps/ui/public/styles/features/config/billing.css` | 定价 / 模型列表样式 | `.billing-split` / `.billing-model-*` / `.billing-rate-card` |
| `apps/ui/public/styles/layout/app-shell.css` | 布局骨架；`--page-max-width` + `.page-container` :120-144 | 除日志台外所有页面的宽度上限与居中 |
| `apps/ui/public/assets/icons/*.svg` | 图标资源（mask 用法，`fill="currentColor"`，`viewBox="0 0 24 24"`，LF 换行） | 控制按钮：`play.svg` / `refresh.svg` / `stop.svg` |
| 后端数据来源 | UI 不直连上游，全部走后端 manager API | `/api/config`（注入 `CATALOG`/`CATALOG_STATUS`）、`/api/catalog`、`/api/models`、`/codeseex/renderer-inject.js`（Codex App 注入） |

UI 相关后端注入点（前端代理只需知道数据从哪来）：
- `crates/proxy/src/manager_service.rs:641-689`：config payload 注入 `CATALOG`（目录文档）、`CATALOG_STATUS`、`CATALOG_REVISION`、`CATALOG_SOURCE`、`CATALOG_PRICING`、`CATALOG_MODELS`、`UPSTREAM_API_KEY_CONFIGURED` 等。
- `crates/proxy/src/codex_app.rs:96/130/131`：Codex 侧模型目录注入脚本（`renderer_inject_script`，:885），UI 之外的模型选择层。

## 2. 数据流（谁拉谁渲染）

### 加载
1. `init()`（:286）→ `loadConfig()`（:1194）→ `renderConfig()`（:1436）。
2. `applyCatalogPayload(config.CATALOG, config.CATALOG_STATUS)`（:4926）把目录写入 `catalogState`（:56-66）：`revision / source / providerName / defaultModel / currency / unit / models / pricing / status`。
3. `renderBillingCatalog()`（:4690）渲染左栏模型卡片 + 右栏选中模型的费率卡（**唯一的目录渲染入口**，不再有独立的目录状态行）。

### 目录刷新（模型列表拉取的入口）
- 「更新模型列表」`#catalogRefreshButton` → `refreshCatalogDocument()`（:589）→ `POST /api/catalog/refresh` → 重新 `loadConfig({render:false})` 拉取最新 `catalogState` 并重渲染列表；按钮文案按结果短暂闪成「已更新 +N / 已是最新 / 更新失败」（`flashCatalogLabel()` :612，1.6s 后复原）。这是**真实接口请求**，不是模板页里的模拟批次。
- 上游连通性测试的 UI 入口（原「测试上游」按钮）已移除；后端 `POST /api/upstream/test` 仍在，但前端不再调用。
- 保存配置时会跳过 `READ_ONLY_CONFIG_KEYS`（:51-54：`CATALOG` / `CATALOG_MODELS` / `CATALOG_STATUS`），这些只读注入字段不会被 UI 回写。

### 定价渲染与保存
- 费率来源：`catalogRateFor(model)`（:4942）＝ 精确 slug → `pricing.groups` 分组 → `null`（与后端 `PricingTable::rate_for` 同规则）。
- 渲染：`renderBillingCatalog()`（:4690）→ `renderBillingModelList()`（:4711，左栏卡片）+ `renderBillingCard()`（:4759，右栏费率卡）；左右栏共用 `selectedCatalogModel`，默认选中 `catalogState.defaultModel`，否则第一个模型。签名去重靠 `currentBillingRatesSignature`（:65）。
- 徽标：`catalogModelBadge()`（:4833）取 `short_display_name`；`catalogPricingBadge()`（:4839）未定价 → `billingUnpriced`、按组回退 → `billingGroupPriced`（皆为 `.is-warn` 徽标）。
- 左栏高度：`updateBillingModelListHeight()`（:4855）按首个卡片实测高度把 `.billing-model-list` 限制为 `VISIBLE_MODEL_CARDS`（=4）行，超出滚动；卡片 ≤ 4 时 `max-height:none`。窗口 resize 走 `scheduleModelListHeightSync()`（:4872）防抖。
- 保存：`billingRateInputs()`（:4668，只取右栏费率卡里的 `input[data-model][data-rate]`）+ `catalogRateOverrides()`（:4880）+ `catalogPeakPricingPayload()`（:4899）→ payload 里的 `CATALOG_PRICING` → 后端 `apply_catalog_pricing_payload`（`config_payload.rs:162`）写入 `[billing]`。
- 峰谷：`catalogPeakValley()`（:4963）只读 `catalogState.pricing.peak_valley`（由后端 / 拉取同步），`isPeakBillingTime()`（:5136）判定；**前端已无峰谷倍数 / 时区 / 时段输入**，也不再随保存回写这些字段。
- 用量页成本：`costForTokens()`（:5068）→ `ratesForTokens()`（:5078）→ 未定价返回 `null` → `formatCostOrUnpriced()` 显示「未定价」；`sumCosts()` 聚合；`normalizeRateInput()`（:4987）容错。

## 3. 模型列表 / 目录 UI 现状

`index.html` 的「计费模式费率设置」区块（:460-469）按模板页 `deepseek_html_*.html` 改成两栏形态（无区块标题行、无上游测试按钮、无峰谷输入）：

```html
<div class="billing-split mt-12">
  <div class="billing-catalog">             <!-- 左栏：更新按钮 + 模型卡片列表 -->
    <button id="catalogRefreshButton" class="btn btn-secondary btn-icon-label">
      <svg class="btn-svg" viewBox="0 0 16 16">…</svg><span id="catalogRefreshLabel">…</span>
    </button>
    <div class="billing-model-list" id="billingModelList"></div>
  </div>
  <div class="billing-card-panel" id="billingCardPanel"></div>  <!-- 右栏：选中模型费率卡 -->
</div>
```

现状要点：
- 模型卡片是 `<button class="billing-model-card">`（`display_name` + `short_display_name` 徽标 + **模型 id / slug** 作为描述行），选中项加 `.is-selected`（主色描边 + 光晕），点击 → `selectBillingModel()`（:4845）。
- 卡片描述行渲染的是 `model.slug`（如 `deepseek-v4-flash`），由拉取到的目录数据驱动，不在前端写死。
- 拉取结果为空时左栏渲染 `.billing-model-empty`（`catalogEmpty` + `catalogEmptyHint`）。
- **没有目录 URL 的可视化配置入口**（只能改 `config.toml` 的 `[catalog] source_url` 或环境变量）。
- UI 没有模型选择器；模型选择发生在 Codex 客户端侧（经 `codex_app` 注入的 `model_catalog_json`）。UI 只消费模型列表用于定价卡渲染。
- 目录模型信息（`aliases` / `alias_patterns` / `upstream_slug` / `pricing_group`）已从 `/api/catalog` 下发，但 UI 目前没有展示这些字段。

## 4. 定价 UI 现状

- 左栏 `.billing-model-list`：每模型一张卡片（按 `catalogModels()` 顺序），可见 4 行，超出滚动。
- 右栏 `#billingCardPanel`：`.billing-rate-card`，标题 = `display_name`，标题下**只声明单位**（`billingUnit`，如「单位：CNY / 每百万 Tokens」），右上角是徽标；下面 `cached_input` / `cache_miss_input` / `output` 三个数字输入（右侧单位取 `catalogState.currency`）。
- 峰谷没有本地输入控件：峰谷开关 / 倍数 / 时区 / 时段一律来自 `catalogState.pricing.peak_valley`（拉取模型时同步），前端仅用于用量页成本估算。
- 兜底常量：`FALLBACK_BILLING_RATES`（:34，全 0）与 `FALLBACK_PEAK_VALLEY`（:35-44）仅在 `catalogState.pricing` 缺失时使用，且语义是「后端未下发时的显示回退」。

## 5. 语言包 keys（模型列表 / 费率区块，9 个语言文件需同步）

`en_us` 参考值：

```text
billingGroupPriced   = "Group priced"
billingUnpriced      = "Unpriced"
billingUnit          = "Unit: CNY / 1M tokens"        # 费率卡标题下的单位说明
catalogEmpty         = "No models fetched yet"
catalogEmptyHint     = "Use the button above to fetch the list from upstream"
catalogFetchAdded    = "Updated"
catalogFetchFailed   = "Update failed"
catalogFetchModels   = "Update model list"
catalogFetchUpToDate = "Already up to date"
catalogFetching      = "Updating..."
catalogModelCount    = "{count} models"
```

改动文案或新增 key 时，必须同步全部 9 个语言文件（缺失 key 会回退到英文默认）。`catalogModelCount` / `billingUnpriced` / `billingGroupPriced` 目前只有 `en_us` / `zh_cn` 有译文，其余语言回退英文，可按需补齐。峰谷 / 上游测试 / 目录状态行的旧 key（`billingPeakMultiplier`、`billingPeakWindows`、`billingTimezone`、`billingPeakValleyMode`、`billingPeakValleyHint`、`usageDisplay`、`billingMode`、`catalogSection`、`catalogRefresh`、`catalogStatusHint`、`catalogSourceLabel`、`catalogRevisionLabel`、`catalogUpstreamTest/Ok/Failed/Testing`）已随控件一起删除，不要再引用。

## 6. 样式

- `apps/ui/public/styles/features/config/billing.css`：`.billing-split`（`280px minmax(0,1fr)`，≤860px 单列堆叠）、`.billing-catalog`、`.billing-model-list`、`.billing-model-card`、`.billing-model-head` / `.billing-model-name` / `.billing-model-desc`、`.billing-model-empty`、`.billing-model-badge`（含 `.is-warn`）、`.billing-card-panel`、`.billing-card-badges`。
- 模型卡片外观与**工具卡片**（`.tool-card`）保持一致：`background: var(--hover-bg)` + `box-shadow: var(--shadow-sm)`，`:hover` 时 `border-color: var(--text-muted)` + `box-shadow: var(--shadow-md)`；选中项 `.billing-model-card.is-selected` 用主色描边 + 光晕。改卡片观感时请同步参照 `styles/features/config/tools.css`，不要另起一套。
- 费率卡与字段沿用既有组件类：`.billing-rate-card` / `.billing-card-header` / `.billing-model-meta`（描述复用 `.muted`）/ `.billing-row` / `.billing-field` / `.billing-prefix` / `.billing-suffix`。
- `apps/ui/public/styles/layout/app-shell.css:120-144`：`--page-max-width: 1040px` + `.page-container`（`flex` 纵向列，`flex:1`，`min-height:0`）；`.workspace .page-container` 统一设 `max-width: var(--page-max-width)` 与 `margin: 0 auto`，**只有日志台**用 `.workspace.view-logs .page-container { max-width: none; margin: 0 }` 保持全宽（避免日志行被压缩）。因此仪表盘 / 用量 / 配置 / 关于在**默认 1280×720 下正常铺满、窗口变宽时内容居中且不再无限拉伸**。调宽上限只改 `--page-max-width` 一处即可。
- `apps/ui/public/styles/components/buttons.css`：`.btn-svg`（内联 `<svg>` 图标，`currentColor` 描边，14px；「更新模型列表」用模板页的 4 段旋转箭头 SVG）、`.btn-icon-only`（24×24 无边框纯图标按钮：`border: none`，背景 `color-mix(in srgb, currentColor 12%, transparent)` 自动跟随药丸状态色，`:hover` 加深到 24%，含 `.is-danger` 危险态与 `:focus-visible`；`app.js:renderButtons()` 用 `title` / `aria-label` 承载可访问名称，没有可见文字）、`.btn-icon-play` / `.btn-icon-refresh` / `.btn-icon-stop`（mask 引用 `assets/icons/*.svg`）。当前映射：启动 → `play.svg`，重启 → `refresh.svg`，停止 → `stop.svg`。
- `apps/ui/public/styles/layout/sidebar.css`：`.status-indicator`（状态药丸：`display: flex` + `margin-bottom: 24px`，`.status-text` 用 `flex: 1` + 省略号截断）、`.status-actions`（`margin-left: auto`，2 个图标按钮**嵌在药丸内部**，不再另行挤压容器宽度）。控制按钮从页面头部（原 `.header-actions`）移到侧栏药丸内，因此**五个页面控制按钮位置完全一致**；旧的 `.status-row` 已删除。
- 控制按钮行为（`app.js:bind()` :347-349、`renderButtons()` :1426-1436）：侧栏只留 2 个按钮 —— 「启动 / 重启」合并按钮 `#startButton`（未运行点击 POST `/api/start`，运行中点击 POST `/api/restart`，图标在 `play.svg` / `refresh.svg` 间切换，`aria-label` 同步「启动 / 重启」，`latestStarting` / `busy` 时禁用）与「停止」按钮 `#stopButton`（POST `/api/stop`，未运行且未启动中时禁用）。
- 样式分层：`foundation/theme.css`（主题变量）、`foundation/base.css`、`layout/app-shell.css` + `layout/sidebar.css`、`components/*.css`、`pages/*.css`。新 UI 应尽量复用组件类（`btn` / `btn-secondary` / `btn-icon-label` / `btn-icon-update` / `btn-icon-heartbeat` / `mt-12` 等），不要只在页面级 CSS 里堆私有类。
- 深色主题一致性：新类应跟随 `theme.css` 的 CSS 变量（`--bg-panel` / `--border-color` / `--text-muted` / `--primary` / `--warn-badge-bg` 等），避免写死颜色。

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

1. **目录 URL 可视化配置**：增加「远端清单 URL」输入（映射到 `CODESEEX_CATALOG_URL` / `[catalog] source_url`），并提示「off」可关闭远程拉取。
2. **模型目录信息展示**：在模型卡片或费率卡里展示 `aliases` / `alias_patterns` / `upstream_slug` / `pricing_group`，帮助用户理解「客户端模型名 → 上游模型名」的映射（数据已下发，只缺渲染）。
3. **未定价/组定价的友好提示**：未定价（`billingUnpriced`）与分组回退（`billingGroupPriced`）目前是文字徽标，可加悬停说明；用量页成本「未定价」同理。
4. **峰谷只读展示**：峰谷已由拉取数据驱动且无本地控件，可考虑在费率卡或用量页以只读文案/时间轴展示当前生效的峰谷时段，方便核对。
5. **费率编辑体验**：数字输入的校验（>0）、保存成功/失败反馈、与后端 `[billing]` 覆盖值的同步提示。
6. **拉取动效对齐模板**：模板页对新拉取到的模型卡片有 `is-new` 脉冲动画（`@keyframes newPulse`），当前实现只有按钮文案闪变，可补齐。
7. **多语言与无障碍**：新增 key 全语言同步；模型卡片是原生 `<button>`，注意保留 `:focus-visible` 与 `aria-pressed`。

## 9. 验证方法

- 语法：`node --check apps/ui/public/app.js`；JSON：`node -e "JSON.parse(require('fs').readFileSync('apps/ui/public/lang/zh_cn.json','utf8'))"`。
- 视觉：用本地静态 stub 伺服 `apps/ui/public`（mock `/api/config` 注入 `CATALOG`），headless 浏览器分别在 1600×900 与 1280×720 下截图五个页面，核对仪表盘 / 用量 / 配置 / 关于内容居中且宽度上限生效、日志台保持全宽、模型卡片为「名称 + 标签 + model id」、费率卡标题下为单位说明。
- 功能：按 §7 重建并启动桌面客户端 → 设置页「计费模式费率设置」区块点「更新模型列表」→ 观察按钮文案闪变、模型卡片选中态与费率卡；Usage 页成本列与未定价显示。
- 后端数据对照：`GET http://127.0.0.1:8787/api/catalog` 与 `GET /api/config` 里 `CATALOG` / `CATALOG_STATUS` 的值应与 UI 渲染一致。

## 10. 红线（不要改）

- 不在前端写死模型/定价/峰谷时段；`FALLBACK_BILLING_RATES` 只能全 0 作显示回退，峰谷只读 `catalogState.pricing.peak_valley`。
- 不绕过后端直接拉远程清单；UI 只走后端 manager API。
- 不改 `model-catalog.json` 契约；不动 Codex 注入脚本的既有注入格式。
- 不在本主题外顺手重构其它页面。
