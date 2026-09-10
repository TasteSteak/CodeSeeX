# 后端代理上下文：远程模型目录与定价拉取（CodeSeeX 0.7.1）

> 本文件是写给「后端代理」的上下文简报。后续代理在本仓库做**项目结构 / 代码优化**时，先读本文件；它聚焦「远程拉取模型列表与定价」相关的数据文件位置、数据流、代码接线与已知限制。与本主题无关的模块（Web Search、工具执行、Tauri 桌面壳等）保持不变，不要顺手重构。

## 1. 数据文件位置总表（最重要）

| 类别 | 路径 | 作用 | 备注 |
| --- | --- | --- | --- |
| 公开远端清单（发布源） | `docs/catalog/model-catalog.json` | 被远程拉取的源文件；默认 URL 直接指向它 | 进 git、进 `main` 后才对线上生效；含 `issued_at`/`min_app_version` |
| 内置保底（编译期） | `crates/core/assets/catalog.default.json` | `include_str!` 编进二进制，`embedded_catalog_document()` 读取 | 与公开清单只差 `issued_at`/`min_app_version` 两个键 |
| 私有 overlay（编译期） | `.private/model-catalog.seed.json` | 私有提示词 `base_instructions`/`model_messages` 来源；`crates/core/build.rs` 拷贝进 `OUT_DIR` | 编译必须存在；可用 `CODESEEX_MODEL_CATALOG_SEED` 覆盖路径；**绝不进入远程清单** |
| 运行时契约文件 | `<data_dir>/model-catalog.json` | Codex 客户端实际读取的文件（Codex 契约，`{ "models": [...] }`） | 由 `build_codeseex_catalog_from_document` 合并私有 overlay 后生成；磁盘契约保持兼容 |
| 运行时远程缓存 | `<data_dir>/cache/model-catalog.json` | 远程文档落地缓存（层 2） | 原子写；与公开清单只差 `issued_at`（`CatalogDocument` 不保存它） |
| 运行时日志 | `<data_dir>/logs/<yyyy-MM-dd>.jsonl` | `catalog_remote_refresh` / `request_failed` 等诊断事件 | 排障入口 |

- `<data_dir>` 默认 `~/.codeseex`，可用 `CODESEEX_DATA_DIR` 覆盖。
- **没有 `.meta.json` / ETag 持久化文件**：ETag 只存在 `CatalogServiceState`（进程内存）里，重启后首次刷新会全量下载，第二次起才走 304。这是已知限制（见 §9）。

## 2. 数据来源与配置

- 默认远端 URL：`https://raw.githubusercontent.com/TasteSteak/CodeSeeX/main/docs/catalog/model-catalog.json`（`crates/proxy/src/catalog_service.rs:20-22` 的常量拼接）。
- 覆盖方式（优先级从低到高）：`[catalog] source_url`（`config.toml`，`crates/core/src/config.rs:150-155`）→ `CODESEEX_CATALOG_URL` 环境变量。值为 `off`/`none`/`disabled`/`false` 表示关闭远程拉取（`catalog_service.rs:107-108`）。
- 开关：`[catalog] remote_enabled` / `CODESEEX_CATALOG_REMOTE`（默认开）。`[catalog] mode` 是旧字段，仅兼容解析、不参与逻辑。
- 拉取时机：进程启动延迟 3 秒拉一次；之后每 6 小时自动复查（`CATALOG_REFRESH_INTERVAL`，`catalog_service.rs:27`）；设置页「Update now」走 `POST /api/catalog/refresh`。单次 8 秒超时，两次拉取间隔 60 秒节流（`CATALOG_REQUEST_TIMEOUT`/`CATALOG_FETCH_THROTTLE`）。

## 3. 代码接线（文件 → 关键函数 → 行号）

### 远程拉取服务
- `crates/proxy/src/catalog_service.rs`
  - `remote_url(config)`（:100）：解析最终 URL。
  - `status(config)`（:117）/ `document_payload(config)`（:148）：`/api/catalog` 状态与模型+定价行。
  - `refresh(runtime, client)`（:182）：ETag → 校验 → 原子写缓存 → 激活 → 重写契约文件；304/unchanged 分支。
  - `activate(runtime, document, etag)`（:281）：写 `<data_dir>/cache/model-catalog.json`、`runtime.set_catalog_document`、用 `build_codeseex_catalog_from_document` 重写 `<data_dir>/model-catalog.json`。
  - `probe`/`cached_probe`（:337 起）：上游 `GET /v1/models` 可用性探测，10 分钟 TTL，`parse_model_ids` 宽容解析（`data[].id|model|name`、`models[]`、裸数组）。
  - `upstream_test(config, client)`（:373）：`POST /api/upstream/test`，报告凭据来源/URL/状态/脱敏错误。
  - `spawn_remote_refresh(state, store)`（:445）：3s 延迟 + 每 6h 循环；`spawn_pricing_sync`（:472）：把定价表推给 store。
  - `probe_diagnostics`（:454）：对「客户端白名单类」401 增加可解释提示。### 目录文档核心（数据模型 + 校验 + 合并）
- `crates/core/src/catalog.rs`
  - `CatalogDocument::from_json/from_value/to_value/to_json`（:677 起）；`merge_authoritative(overlay)`（:910）；`model_for_request`（:840）；`default_slug`（:832）；`pricing_group_for`（:874）；`upstream_slug_for`（:886）。
  - `embedded_catalog_document()`（:1011）：读内置资产。
  - `read_cached_catalog_document`（:1102）/ `write_cached_catalog_document`（:1107）：缓存读写（原子写 `*.json.tmp` + rename）。
  - `write_catalog_atomic`（:491）：写 Codex 契约文件；`catalog_file_is_compatible`（:503）兼容检查。
  - `apply_catalog_overrides`（:1121）：用户 `[models.*]` 覆盖（只补字段、不删模型）。
  - `build_codeseex_catalog_from_document`（:171）：目录文档 + 私有 seed 合并 → 生成 Codex 侧 `Catalog`。
  - `app_server_model_list_for_document`（:186）：模型列表给 `/api/models`。
  - `resolve_upstream_slug(config, requested)`（:1000）：出站模型名改写（目录优先，`UpstreamModelOverride` 兜底）。
  - 校验常量：`SUPPORTED_CATALOG_SCHEMA_VERSION = 1`（:664）、`MAX_CATALOG_DOCUMENT_BYTES = 256KB`（:666）。

### 定价数据模型
- `crates/core/src/pricing.rs`：`ModelRates`、`PeakValley`/`PeakWindow`、`PricingTable`
  - `rate_for(model, group)`（:360）：**精确 slug → 分组 → `None`**，未知模型返回 `None`（绝不静默套用其它模型价格）。
  - `period_for(completed_at)`（:375）：峰谷判定（`[from, to)` 左闭右开）；`estimate`（:379）成本估算；`with_override_value`（:408）用户覆盖。

### 配置与运行时
- `crates/core/src/config.rs`
  - `AppConfig.catalog_*` 字段（:27-32）：`catalog_source_url`、`catalog_remote_enabled`、`catalog_overrides`、`catalog_remote`。
  - `catalog_document()`（:404）：分层生效文档（内置 → 合并缓存/远程 → 用户覆盖）。
  - `catalog_cache_path()`（:399）= `<data_dir>/cache/model-catalog.json`；`catalog_path()`（:447）= `<data_dir>/model-catalog.json`。
  - `pricing_table()`（:414）、`catalog_revision()`（:419）、`catalog_source_label()`（:422）。
- `crates/proxy/src/runtime_config.rs`：`catalog: Arc<RwLock<Option<Arc<CatalogDocument>>>>`；`set_catalog_document`（:207）；`build_snapshot`（:76 起）；`config_signature` 含 `catalog` 段（:395-401）。

### 管理 API 与配置读写
- `crates/proxy/src/manager_api.rs`：`GET /api/catalog`（:65）、`POST /api/catalog/refresh`（:66-69）、`GET /api/upstream/probe`（:70）、`POST /api/upstream/test` + `/manager/upstream/test`（:75）、`POST /api/upstream/credential`（:76）。
- `crates/proxy/src/manager_service.rs`：`catalog_payload()`、`catalog_refresh()`、`upstream_probe()`、`upstream_test()`、`set_upstream_credential()`；`model_list()`（:300）；config payload 注入 `CODESEEX_CATALOG_URL`/`CODESEEX_CATALOG_REMOTE`/`CATALOG_PRICING`/`CATALOG_MODELS`/`CATALOG`/`CATALOG_STATUS`/`CATALOG_REVISION`/`CATALOG_SOURCE`/`UPSTREAM_API_KEY_CONFIGURED`（:641-689）；`catalog_pricing_override`（:1514）/`catalog_model_overrides`（:1522）。
- `crates/proxy/src/config_payload.rs`：读取 `CODESEEX_CATALOG_URL`/`CODESEEX_CATALOG_REMOTE`；`apply_catalog_pricing_payload`（:162）`CATALOG_PRICING`→`[billing]`、`apply_catalog_models_payload` `CATALOG_MODELS`→`[models.*]`；旧 `BILLING_*` 键保持读写做迁移。

### 模型列表入口（保持一致）
- `crates/proxy/src/server.rs:295` `models()`：`GET /v1/models` 用 `state.active_config().catalog_document()`（跟随生效层，不写死内置）。
- `crates/proxy/src/manager_service.rs:302`：`/api/models` 用 `app_server_model_list_for_document`。
- `crates/proxy/src/codex_app.rs:96/130/131`：Codex 注入脚本用 `config.catalog_document()` + `catalog_revision`。
- `crates/proxy/src/manager_api.rs:218` `inject_codex_app_model_catalog` / `codex_app.rs:1594` `inject_model_catalog`：Codex App 模型目录注入。

### 定价下沉到存储
- `crates/store/src/store.rs`：`Store::set_pricing_table`（:466）；`usage_billing_period(pricing, completed_at)`（:3918）；`UsageBillingBucket` 携带 `pricing_revision`/`rates`/`rate_source`；store 自身**没有**硬编码窗口/倍数。
- 注入点：`spawn_pricing_sync`（`catalog_service.rs:472`）与 `ManagerRuntime::open`（`manager_service.rs:130`）。
## 4. 数据结构

### 目录文档信封（公开清单 / 内置资产 / 缓存共用）
```json
{
  "schema_version": 1,
  "revision": "0.7.1.1",
  "issued_at": "2026-09-10T00:00:00Z",
  "min_app_version": "0.7.1",
  "provider_name": "DeepSeek",
  "default_model": "deepseek-v4-pro",
  "models": [ "/* 见下 */" ],
  "pricing": { "/* 见下 */" }
}
```

### 模型条目（Codex 原生字段 + 数据化字段）
```json
{
  "slug": "deepseek-v4-pro",
  "display_name": "DeepSeek V4 Pro",
  "description": "...",
  "context_window": 1000000,
  "max_context_window": 1000000,
  "effective_context_window_percent": 95,
  "priority": 2,
  "shell_type": "shell_command",
  "truncation_policy": { "mode": "tokens", "limit": 10000 },
  "input_modalities": ["text", "image"],
  "supported_reasoning_levels": [],
  "short_display_name": "Pro",
  "aliases": [],
  "alias_patterns": ["gpt-5*", "gpt5*"],
  "upstream_slug": "deepseek-v4-pro",
  "pricing_group": "pro",
  "is_default": true
}
```

### pricing 块
```json
{
  "revision": "0.7.1.1",
  "currency": "CNY",
  "unit": "per_1m_tokens",
  "peak_valley": {
    "enabled": true,
    "timezone": "Asia/Shanghai",
    "multiplier": 2.0,
    "windows": [{ "from": "09:00", "to": "12:00" }, { "from": "14:00", "to": "18:00" }]
  },
  "groups": {},
  "rates": {
    "deepseek-v4-pro":   { "cached_input": 0.025, "cache_miss_input": 3.0, "output": 6.0 },
    "deepseek-v4-flash": { "cached_input": 0.02,  "cache_miss_input": 1.0, "output": 2.0 }
  }
}
```

### 层与优先级
`用户覆盖 [models.*] > 远程文档（内存）/ 缓存（cache/model-catalog.json）> 内置资产`。
合并语义：`base.merge_authoritative(remote)`（remote 为权威、base 补缺失字段）；私有 seed 只按 slug 合并 `base_instructions`/`model_messages`，远程清单**永远改不到提示词**。

## 5. 拉取/激活流程

1. `refresh()` 取 `runtime.active_config()`；检查开关与 URL；60s 节流。
2. `GET <url>`，带 `If-None-Match: <etag>`（内存）、`User-Agent: CodeSeeX`、`Accept: application/json`；8s 超时。
3. `304 Not Modified` → 返回 `not_modified: true`；HTTP 错误 → `record_refresh_failure`（记 `last_failure`，不阻塞）。
4. `CatalogDocument::from_json` 校验（§6）；失败 → `catalog_remote_invalid`，保留旧文档。
5. 成功 → `activate()`：原子写 `cache/model-catalog.json` → `runtime.set_catalog_document`（触发 `CatalogRefresh` 变更事件）→ 用 `build_codeseex_catalog_from_document` 重写 `<data_dir>/model-catalog.json` → 记 `last_success` 与 ETag。
6. 若 revision 未变化：不激活，但**记住 ETag**（`catalog_service.rs:253-263`），避免每次全量重下。

## 6. 校验与拒收规则（拒收即保底）

- `schema_version` 必须为 `1`；文档 ≤ 256 KB；`revision` 非空；至少一个模型。
- slug 唯一且只含 `[A-Za-z0-9-_.]`；`display_name` 非空；`context_window > 0`；`effective_context_window_percent` ∈ [1,100]。
- `min_app_version` 不得高于当前版本（`version_at_least`，`catalog.rs:1081`）；必须能确定默认模型。
- 任一失败：只记 `last_failure`，继续用上一层文档，不阻塞代理、不改 transport、不删模型。

## 7. 管理 API 摘要

| 端点 | 方法 | 作用 |
| --- | --- | --- |
| `/api/catalog` | GET | 状态 + 解析后的模型/定价（`document_payload`） |
| `/api/catalog/refresh` | POST | 立即拉取（60s 节流） |
| `/api/upstream/probe` | GET | 上游 `/v1/models` 可用性探测（`listed/not_listed/unknown`） |
| `/api/upstream/test`、`/manager/upstream/test` | POST | 凭据来源 / 最终 URL / 状态码 / 脱敏错误 |
| `/api/upstream/credential` | POST | 保存或清除 OS 凭据库里的上游 API Key |
| `/v1/models`、`/api/models` | GET | 模型列表（生效目录） |

## 8. 测试与烟测

- 全量：`cargo test --workspace`。
- 定向：`cargo test -p codeseex-core --lib pricing::`、`cargo test -p codeseex-core --lib catalog::`、`cargo test -p codeseex-store --lib`、`cargo test -p codeseex-proxy --lib catalog_service::`、`cargo test -p codeseex-proxy --lib upstream::`。
- 端到端烟测做法（本轮已跑通）：起一个本地静态服务托管 `docs/catalog/model-catalog.json`，用 `CODESEEX_CATALOG_URL` 指向它，观察 `<data_dir>/cache/model-catalog.json` 落盘、`<data_dir>/model-catalog.json` 重写、第二次刷新拿到 304；把 URL 指向不可达地址或非法文档，验证只记 `last_failure` 且回退到上一层。
- 构建/运行环境：`CARGO_TARGET_DIR` 建议复用 `D:\DevTools\CodeSeeXNext\CargoTarget`。

## 9. 已知限制与优化方向（给后续代理）

1. **ETag 不持久化**：只在进程内存。可选优化：把 ETag 与 `last_success` 落到 `<data_dir>/cache/model-catalog.meta.json`（注意原子写与容错）。
2. **默认 URL 指向 `main`**：`docs/catalog/model-catalog.json` 未推送到 `main` 之前，线上拉取会 404 并回退到缓存/内置（预期行为，不是缺陷）。
3. **探测与真实流量身份不同**：`/api/upstream/probe`、`/api/upstream/test` 没有客户端请求可透传，对「按 Codex 客户端做白名单」的中转必然 401；已有 `probe_diagnostics`（`catalog_service.rs:454`）追加解释文案。若要更贴近真实链路，需谨慎评估是否在探测中合成身份（涉及语义正确性，不要擅自改）。
4. **`pricing.groups` 当前为空**：分组回退链路已实现（`rate_for` 第二分支、UI `catalogRateFor`），但没有实际数据；新增分组时后端与 UI 都会自动生效。
5. **未定价语义**：`rate_for` 返回 `None` 时 UI 显示「未定价」（`billingUnpriced`），绝不能回退到 Pro 价。
6. **逐请求费率快照不在 0.7.1 范围**：计费桶只携带 `pricing_revision` 与解析后的费率；历史用量复现需要额外设计。
7. **`<data_dir>/model-catalog.json` 磁盘契约不要改**：只能改生成来源，不能改结构（Codex 端依赖 `catalog_file_is_compatible`）。
8. 目录文档解析细节见 `docs/model-catalog-and-pricing-v0.7.1-design.md`（含 401 实测记录附录 A.6）。

## 10. 红线（不要改）

- 不要把私有提示词（`base_instructions`/`model_messages`）写进 `docs/catalog/model-catalog.json` 或任何远程清单。
- 不要让远程清单覆盖本地 overlay 的提示词字段。
- 不要在拉取失败时删除模型、切换 transport 或让代理启动失败。
- 不要把模型/定价重新写回 Rust 常量或前端常量。
- 不要在本主题之外顺手重构无关模块。
