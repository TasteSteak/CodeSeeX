# 前端提示类信息清单（CodeSeeX UI）

范围：`apps/ui/public/app.js`（约 5500 行）、`apps/ui/public/index.html`、`apps/ui/public/lang/*.json`。
目的：逐条决定每类提示「保留现状」还是「改造成 toast 弹窗」。

本轮只做盘点，不改动 `app.js` / `index.html` / 语言包。

## 优先级图例

| 优先级 | 含义 |
| --- | --- |
| 高 | 短时、跨场景、当前最容易被用户漏看的全局反馈，建议第一批接入 toast |
| 中 | 有明确价值但需要配合现有就地状态，建议第二批接入或「就地 + toast 双通道」 |
| 低 | 常驻状态、模态、空态或日志，本质不是瞬时提示，建议保留原样 |

## 建议列取值

`保留原样`：当前形态合适，不动。
`适合 toast`：建议改造成 toast（可保留原就地反馈作为兜底）。
`保留原样 + 追加 toast`：就地状态继续保留，同时补一条 toast 提升可见性。
`建议补 toast`：当前缺少可见反馈，属于体验缺口。

## A. 配置 / TOML / Codex 启动（Dashboard 与 Settings）

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| 配置自动保存：草稿 / 待保存 / 保存中 / 已保存 / 已保存需重启 | 内联状态文字 `#configSaveStatus`（muted，配置页标题右侧，常驻） | `configDraft` `configPending` `configSaving` `configSaved` `configSavedRestart` | 保留原样（常驻状态，非瞬时） | 低 | app.js:4574 `renderConfigSaveState`；index.html:288 |
| 配置保存失败、版本冲突（409，自动重试一次后仍失败） | 同 `#configSaveStatus`，`data-state=error` 并拼接服务端错误详情 | `configSaveError` `configSaveConflict` | 保留原样 + 追加 toast | 中 | app.js:645-723 |
| 部分改动需要重启代理 | 内联徽标 `#restartRequiredBadge` | `restartRequired` | 保留原样 | 低 | index.html:289 |
| 复制 CodeSeeX TOML 结果 | 闪变内联 `#configTomlCopyStatus`（默认 2.2s 后清空，失败加 warning 样式） | `copied` `copyFailed` | 适合 toast | 高 | app.js:1513-1526、2071 |
| Codex 适配器未就绪时点击复制 / 导入（前置校验拦截） | 闪变内联 warning（2.2s） | `codexAdapterMissing` | 适合 toast | 中 | app.js:1516、1530 |
| 导入到 CCS 的结果 | 闪变内联 warning（成功含目录丢失警告 5.2s / 失败 2.6s） | `ccsImportStartedCatalogWarning` `ccsImportFailed` | 适合 toast（长警告尤其适合） | 高 | app.js:1527-1542 |
| 启动 Codex 的结果 | 闪变内联（请求成功 2.4s；注入未完成警告 9s；启动失败） | `codexLaunchStarted` `codexLaunchStartedWithInjectionWarning` `codexLaunchFailed` | 适合 toast | 高 | app.js:1543-1573 |
| 刷新模型目录按钮标签 | 按钮文字闪变 `flashCatalogLabel`（1.6s 后恢复 Fetch models，成功可带 `+N`） | `catalogFetching` `catalogFetchAdded` `catalogFetchUpToDate` `catalogFetchFailed` | 保留原样（就地按钮反馈）；失败可追加 toast | 中 | app.js:592-627 |
| Codex 模型目录横幅 | 内联持久告示条 `#catalogNotice`（is-warning / is-error / is-repaired） | `catalogNoticeWarning` `catalogNoticeError` `catalogNoticeRepaired` `catalogNoticeRuntimeError` | 保留原样（面板内持久状态）；错误态可追加 toast | 中 | app.js:1825 `renderCatalogNotice`；index.html:181 |
| 计费模型列表为空 | 内联空态（标题 + 说明） | `catalogEmpty` `catalogEmptyHint` | 保留原样 | 低 | app.js:4824-4833 |

## B. 余额与充值

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| 余额检查中 / 可用 / 不可用 | 内联 stage 状态文字 `#balanceStatus` | `balanceLoading` `balanceAvailable` `balanceUnavailable` | 保留原样 | 低 | app.js:1326-1330、4113；index.html:148 |
| 余额检查失败 / 未配置 API Key | 同 `#balanceStatus`，error 态（红字） | `balanceFailed` `balanceNoApiKey` | 保留原样 + 追加 toast（失败态） | 中 | app.js:4113-4122 |
| 打开 DeepSeek 充值页失败 | 同 `#balanceStatus`，error 态 + 原始错误文本 | 无专用 key（`error.message`） | 适合 toast | 低 | app.js:4384-4391 |

## C. About / 更新 / 外链

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| About 状态行默认就绪 | 内联文字 `#aboutStatus` | `ready` | 保留原样 | 低 | index.html:594 |
| 检查更新中 | 同 `#aboutStatus` | `checkingUpdate` | 保留原样 | 低 | app.js:4326 |
| 有可用更新（含可安装 / 仅外链两种） | 同 `#aboutStatus`，html 渲染，版本号为外链 | `updateAvailablePrefix` `updateAvailable` `updateAvailableInstallable` | 保留原样（含链接与安装入口） | 低 | app.js:2098、2133 |
| 已是最新版本 | 同 `#aboutStatus` | `updateCurrent` | 保留原样 | 低 | app.js:2102 |
| 安装更新中 | 同 `#aboutStatus` | `installingUpdate` | 保留原样 | 低 | app.js:2098 |
| 更新检查失败 | 同 `#aboutStatus`，warning 态 + 错误详情 | `updateCheckFailed` | 保留原样 + 追加 toast | 中 | app.js:2104 |
| 更新安装失败 | 同 `#aboutStatus`，warning 态 | `updateInstallFailed` | 保留原样 + 追加 toast | 中 | app.js:2182 |
| 更新已取消 / 已安装即将重启 | 同 `#aboutStatus` | `updateCanceledTask` `updateInstalledRestarting` | 保留原样 | 低 | app.js:2184-2186 |
| 应用信息加载失败 | `#aboutStatus` warning + 原始错误 | 无专用 key（`error.message`） | 适合 toast | 中 | app.js:1310-1316 |
| 外链打开成功 | `#aboutStatus` | `openExternal` | 保留原样 | 低 | app.js:4373-4383 |
| 外链打开失败（Tauri 调用失败后回落 window.open 也失败） | `#aboutStatus` warning + 原始错误 | 无专用 key（`error.message`） | 适合 toast | 低 | app.js:4373-4383 |
| About 动作缺少对应 URL（官网 / 反馈 / 源码 / 许可） | `#aboutStatus` warning | `websiteUnavailable` `feedbackUnavailable` `sourceUnavailable` `licenseUnavailable` | 适合 toast | 中 | app.js:4155-4162 |
| 应用信息尚未加载完点击「了解更多」 | `#aboutStatus` warning | `appInfoLoading` | 保留原样 | 低 | app.js:4156 |
| 更新提示小红点（侧栏 About 项 / 更新按钮） | 内联小红点 | 无 key | 保留原样 | 低 | index.html:55；app.js:2092 |

## D. 模态弹窗

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| 更新下载进度（含后台下载） | 模态 `#updateModal` + 后台条 `#updateBackgroundStatus`，含进度条与分步状态 | `updateDownloadingTitle` `updateDownloadSubtitle` `updateDownloadTask` `updateStateRunning` `updateStateWaiting` `updateStateDone` `updateStateFailed` `updateFailedTitle` `updateFailedSubtitle` `updateFailedTask` | 保留原样（模态） | 低 | index.html:613；app.js:2194-2233 |
| 发行说明 | 模态 `#releaseNotesModal`，含加载中 / 不可用 / 无条目 / 离线回退 / 英语回退 | `releaseNotesLoading` `releaseNotesUnavailable` `releaseNotesNoEntries` `releaseNotesOfflineFallback` `releaseNotesLanguageFallback` `releaseNotesSourceBundled` `releaseNotesSourceCache` `releaseNotesSourceGitHub` | 保留原样（模态） | 低 | app.js:4218-4312；index.html:639 |
| CCS 导入 API Key 输入 | 模态 `#ccsKeyModal` + 内联警示条 | `ccsApiKeyTitle` `ccsApiKeyHint` `ccsApiKeyCatalogWarning` | 保留原样（模态） | 低 | index.html:656 |
| 故障排查 | 模态 `#troubleshootModal`：概览卡片 + 诊断网格 + 技术细节折叠 + 建议动作列表 | `troubleshootTitle` `troubleshootHint` `troubleshootProxyRunning` `troubleshootProxyStarting` `troubleshootProxyStopped` `troubleshootCodexConfig` `troubleshootRuntimeCatalog` `troubleshootTechnicalDetails` `troubleshootAction*` `verifyCodexRuntime` | 保留原样（模态）；运行时验证失败可追加 toast | 低 | app.js:1606-1823；index.html:671 |
| 忙碌遮罩（启动 / 停止 / 重启进程、通用处理中） | 全屏模态 `#loadingOverlay`（标题 + 详情） | `busyTitle` `busyDetail` `startingTitle` `startingDetail` `stoppingTitle` `stoppingDetail` `restartingTitle` `restartingDetail` | 保留原样（模态） | 低 | index.html:605；app.js:4596 |

## E. 日志页

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| 客户端读取失败（`/api/status`、`/api/usage`、`/api/events`） | 仅写入日志页合成条目（type `client_error`）+ 顶部状态 pill 变 `Manager unavailable` | `clientError` `unavailable` | 建议补 toast（失败对用户完全无即时反馈） | 高 | app.js:770-778、845-853、902-909 |
| 代理/请求级事件（进程错误、启动失败、请求失败、上下文压缩失败等） | 日志页条目（分类 + 级别 + 详情） | `processError` `proxyStartFailed` `requestFailed` `contextCompactionFailed` 等 | 保留原样（日志）；高严重级别可考虑 toast | 低 | app.js:3931 `userLogMessage` |
| 日志列表为空 | 内联空态日志行 | `noLogs` `noLogsDetail` | 保留原样 | 低 | app.js:3866 `emptyLogEntry` |
| 加载更早日志 | 内联分隔条 | `loadedOlderLogs` | 保留原样 | 低 | app.js:4082 `logDivider` |

## F. 用量页

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| 尚无已完成会话 | 内联空态 | `noRows` | 保留原样 | 低 | app.js:3020 `renderUsageRows` |
| 会话明细加载中的占位 | 内联骨架行 | `busyDetail` | 保留原样 | 低 | app.js:3123 `usageLoadingBody` |
| 会话明细加载失败 | 无任何可见反馈（静默返回，占位行保留） | 无 | 建议补 toast | 中 | app.js:3137-3158 `fetchUsageSessionDetail` |

## G. 全局外壳与其他

| 触发场景 | 当前展示方式 | i18n key | 建议 | 优先级 | 位置 |
| --- | --- | --- | --- | --- | --- |
| 顶部运行状态指示（运行 / 启动中 / 停止中 / 已停止 / 不可用） | 内联状态 pill `#statusPill` + `#running` 文字 | `running` `starting` `stopping` `stopped` `unavailable` | 保留原样 | 低 | index.html:39；app.js:1386-1391 |
| Dashboard 就绪阶段标签（端口、代理、Codex 目录、运行时目录、余额） | 内联 stage-state 标签 | `dashboardStatusReady` `dashboardStatusChecking` `dashboardStatusRunning` `dashboardStatusStopped` `dashboardPortPending` | 保留原样 | 低 | app.js:1401-1423 |
| 右键菜单「复制」选中文本 | 静默写入剪贴板，无任何反馈 | 无（可复用 `copied` / `copyFailed`） | 建议补 toast | 中 | app.js:557-561 `copySelectedText` |
| 右键菜单文案 | 上下文菜单项 | `contextCopy` `contextSelectAll` | 保留原样 | 低 | app.js:518 `updateContextMenuLabels` |

## 明确排除（非「提示类」反馈）

以下 key 是静态标签、选项文案或表单提示，本轮不计入清单：

`*Hint`（如 `reasoningSummaryHint` `proxyListenPortHint` `deepseekBaseUrlHint` 等）、
`logColumn*` / `logFilter*` / `logCategory*` / `logLevel*`、`usage*Stage` / `usage*Hint`（用量阶段标签）、
`temperature*` / `thinking*` / `theme*` / `closeBehavior*` / `networkProxyMode*` 等选项文案、
`secretConfigured` `clearSavedSecret` `catalogToml*`（技术细节字段值）、
以及模态里已经覆盖的按钮文案（`cancel` `close` `downloadInBackground` 等）。

## 附录：高优先级条目的建议 toast key

| 场景 | 建议 toast key | 语气 |
| --- | --- | --- |
| 复制 TOML 成功 / 失败 | `toast.copyTomlOk` / `toast.copyTomlFailed` | success / error |
| CCS 导入已打开 / 失败 | `toast.ccsImportOpened` / `toast.ccsImportFailed` | warn / error |
| 启动 Codex 成功 / 注入警告 / 失败 | `toast.codexLaunchOk` / `toast.codexLaunchInjectionWarning` / `toast.codexLaunchFailed` | success / warn / error |
| 客户端读取失败（status / usage / events） | `toast.clientReadFailed` | error |

接入时同一 toast 复用现有 key（`copied` `copyFailed` `ccsImport*` `codexLaunch*` `clientError`），
不为 toast 单独新增一份文案，保持语言包单一来源。
