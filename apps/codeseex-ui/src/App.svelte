<script lang="ts">
  import { onMount } from "svelte";

  const API_BASE = "http://127.0.0.1:8787";

  type ViewId = "overview" | "config" | "tools" | "logs" | "about";
  type ConfigValue = string | string[] | null | undefined;
  type ConfigState = Record<string, ConfigValue>;

  type EventRecord = {
    id: number;
    level: string;
    type: string;
    message: string;
    detail?: unknown;
    ts: string;
  };

  type RuntimeTurn = {
    id: string;
    model: string;
    requested_model: string;
    completed_at: string;
    cached_input_tokens: number;
    cache_miss_input_tokens: number;
    output_tokens: number;
    total_tokens: number;
    request_ms: number;
  };

  type RuntimeSummary = {
    status?: string;
    active_requests?: number;
    request_count?: number;
    failed_request_count?: number;
    total_cached_input_tokens?: number;
    total_cache_miss_input_tokens?: number;
    total_output_tokens?: number;
    average_ms?: number;
    last_turn?: RuntimeTurn | null;
    turn_history?: RuntimeTurn[];
  };

  type Status = {
    ok: boolean;
    running?: boolean;
    base_url?: string;
    catalog_path?: string;
    models?: string[];
    data_dir?: string;
    upstream?: { base_url: string; official_v1_compat: boolean };
    runtime?: RuntimeSummary;
    events?: EventRecord[];
  };

  type ToolLabel = {
    id: string;
    label: string;
  };

  type ToolRecord = {
    id: string;
    name: string;
    description: string;
    enabled: boolean;
    configurable: boolean;
    system?: boolean;
    source?: string;
    labels?: ToolLabel[];
  };

  type AdapterState = {
    ready?: boolean;
    status?: string;
    catalog_path?: string;
    catalog_mode?: string;
    models?: string[];
    toml_snippet?: string;
    warning?: string;
    error?: string;
  };

  type AppInfo = {
    name?: string;
    product_name?: string;
    version?: string;
    description?: string;
    license?: string;
    repository?: string;
    urls?: Record<string, string>;
  };

  type UpdateCheck = {
    ok?: boolean;
    has_update?: boolean;
    latest_version?: string;
    current_version?: string;
    url?: string;
    error?: string;
  };

  const views: { id: ViewId; label: string; kicker: string }[] = [
    { id: "overview", label: "概览", kicker: "运行状态" },
    { id: "config", label: "系统配置", kicker: "代理与 Codex" },
    { id: "tools", label: "工具", kicker: "系统 / 内置 / 社区" },
    { id: "logs", label: "日志", kicker: "请求与用量" },
    { id: "about", label: "关于", kicker: "版本与更新" }
  ];

  let activeView: ViewId = "overview";
  let status: Status | null = null;
  let config: ConfigState | null = null;
  let tools: ToolRecord[] = [];
  let events: EventRecord[] = [];
  let adapter: AdapterState | null = null;
  let appInfo: AppInfo | null = null;
  let updateCheck: UpdateCheck | null = null;
  let loading = false;
  let error = "";
  let saveMessage = "";
  let copiedToml = false;

  onMount(() => {
    void refreshAll();
  });

  async function apiJson<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${API_BASE}${path}`, {
      headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
      ...init
    });
    const text = await response.text();
    const data = text ? JSON.parse(text) : {};
    if (!response.ok) {
      const message = data?.error?.message ?? data?.error ?? response.statusText;
      throw new Error(String(message));
    }
    return data as T;
  }

  async function refreshAll() {
    loading = true;
    error = "";
    try {
      const [nextStatus, nextConfig, nextTools, nextEvents, nextAdapter, nextInfo] =
        await Promise.all([
          apiJson<Status>("/api/status"),
          apiJson<ConfigState>("/api/config"),
          apiJson<{ tools: ToolRecord[] }>("/api/tools"),
          apiJson<{ events: EventRecord[] }>("/api/events?limit=80"),
          apiJson<AdapterState>("/api/codex-adapter"),
          apiJson<AppInfo>("/api/app-info")
        ]);
      status = nextStatus;
      config = nextConfig;
      tools = nextTools.tools ?? [];
      events = nextEvents.events ?? nextStatus.events ?? [];
      adapter = nextAdapter;
      appInfo = nextInfo;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      loading = false;
    }
  }

  async function saveConfig(showMessage = true) {
    if (!config) return;
    saveMessage = "";
    try {
      await apiJson("/api/config", {
        method: "POST",
        body: JSON.stringify(config)
      });
      if (showMessage) saveMessage = "已保存";
      await refreshAll();
    } catch (cause) {
      saveMessage = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function refreshAdapter() {
    adapter = await apiJson<AdapterState>("/api/codex-adapter/generate", { method: "POST" });
  }

  async function copyToml() {
    const text = adapter?.toml_snippet ?? "";
    if (!text) return;
    copiedToml = false;
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const textarea = document.createElement("textarea");
      textarea.value = text;
      textarea.style.position = "fixed";
      textarea.style.opacity = "0";
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand("copy");
      textarea.remove();
    }
    copiedToml = true;
    window.setTimeout(() => {
      copiedToml = false;
    }, 1800);
  }

  async function checkUpdate() {
    updateCheck = await apiJson<UpdateCheck>("/api/update-check");
  }

  async function checkBalance() {
    try {
      const balance = await apiJson<unknown>("/api/deepseek/balance");
      events = [
        {
          id: Date.now(),
          level: "info",
          type: "balance_check",
          message: "Balance query completed.",
          detail: balance,
          ts: new Date().toISOString()
        },
        ...events
      ];
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    }
  }

  function cfg(key: string, fallback = "") {
    const value = config?.[key];
    if (Array.isArray(value)) return value.join(",");
    return typeof value === "string" ? value : fallback;
  }

  function updateConfig(key: string, value: ConfigValue) {
    config = { ...(config ?? {}), [key]: value };
  }

  function inputValue(event: Event) {
    return (event.currentTarget as HTMLInputElement).value;
  }

  function selectValue(event: Event) {
    return (event.currentTarget as HTMLSelectElement).value;
  }

  function checkedValue(event: Event) {
    return (event.currentTarget as HTMLInputElement).checked;
  }

  async function setToolEnabled(tool: ToolRecord, enabled: boolean) {
    if (!tool.configurable) return;
    const current = enabledToolIds();
    const next = enabled
      ? Array.from(new Set([...current, tool.id]))
      : current.filter((id) => id !== tool.id);
    updateConfig("ENABLED_TOOLS", next);
    await saveConfig(false);
  }

  function enabledToolIds() {
    const configured = config?.ENABLED_TOOLS;
    if (Array.isArray(configured)) return configured.filter((value): value is string => typeof value === "string");
    return tools.filter((tool) => tool.enabled && tool.configurable).map((tool) => tool.id);
  }

  function formatNumber(value: number | undefined) {
    return typeof value === "number" ? value.toLocaleString() : "0";
  }

  function formatMs(value: number | undefined) {
    return typeof value === "number" && value > 0 ? `${value} ms` : "-";
  }

  function cacheHitRate(runtime: RuntimeSummary | undefined) {
    const cached = runtime?.total_cached_input_tokens ?? 0;
    const miss = runtime?.total_cache_miss_input_tokens ?? 0;
    const total = cached + miss;
    if (!total) return "0%";
    return `${Math.round((cached / total) * 1000) / 10}%`;
  }

  function detailText(detail: unknown) {
    if (detail === null || detail === undefined) return "";
    return typeof detail === "string" ? detail : JSON.stringify(detail, null, 2);
  }
</script>

<main class="app-shell">
  <aside class="sidebar">
    <div class="brand">
      <span class="brand-mark">C</span>
      <div>
        <strong>CodeSeeX</strong>
        <small>Next rewrite</small>
      </div>
    </div>

    <nav class="nav-list" aria-label="Primary">
      {#each views as view}
        <button
          class:active={activeView === view.id}
          onclick={() => (activeView = view.id)}
          type="button"
        >
          <span>{view.label}</span>
          <small>{view.kicker}</small>
        </button>
      {/each}
    </nav>
  </aside>

  <section class="workspace">
    <header class="topbar">
      <div>
        <p class="eyebrow">Tauri / Rust migration</p>
        <h1>{views.find((view) => view.id === activeView)?.label}</h1>
      </div>
      <div class="top-actions">
        {#if loading}
          <span class="pill">刷新中</span>
        {/if}
        <button class="ghost" onclick={refreshAll} type="button">刷新</button>
      </div>
    </header>

    {#if error}
      <section class="notice error">{error}</section>
    {/if}

    {#if activeView === "overview"}
      <section class="grid cards-4">
        <article class="metric-card">
          <span>代理状态</span>
          <strong>{status?.running ? "运行中" : "等待中"}</strong>
          <small>{status?.base_url ?? "-"}</small>
        </article>
        <article class="metric-card">
          <span>请求数</span>
          <strong>{formatNumber(status?.runtime?.request_count)}</strong>
          <small>失败 {formatNumber(status?.runtime?.failed_request_count)}</small>
        </article>
        <article class="metric-card">
          <span>缓存命中率</span>
          <strong>{cacheHitRate(status?.runtime)}</strong>
          <small>输入缓存统计</small>
        </article>
        <article class="metric-card">
          <span>平均耗时</span>
          <strong>{formatMs(status?.runtime?.average_ms)}</strong>
          <small>SQLite runtime summary</small>
        </article>
      </section>

      <section class="panel">
        <div class="panel-head">
          <div>
            <p class="label">运行详情</p>
            <h2>代理闭环</h2>
          </div>
          <button class="ghost" onclick={checkBalance} type="button">查询余额</button>
        </div>
        <dl class="details">
          <dt>数据目录</dt>
          <dd>{status?.data_dir ?? "-"}</dd>
          <dt>Catalog</dt>
          <dd>{status?.catalog_path ?? "-"}</dd>
          <dt>模型</dt>
          <dd>{status?.models?.join(", ") ?? "-"}</dd>
          <dt>上游</dt>
          <dd>{status?.upstream?.base_url ?? "-"}</dd>
        </dl>
      </section>
    {:else if activeView === "config"}
      <section class="panel">
        <div class="panel-head">
          <div>
            <p class="label">系统配置</p>
            <h2>代理与模型</h2>
          </div>
          <button onclick={() => saveConfig()} type="button">保存配置</button>
        </div>

        <div class="form-grid">
          <label>
            <span>代理端口</span>
            <input value={cfg("PROXY_PORT", "8787")} oninput={(event) => updateConfig("PROXY_PORT", inputValue(event))} />
          </label>
          <label>
            <span>自定义上游地址</span>
            <input
              placeholder="https://api.deepseek.com/"
              value={cfg("DEEPSEEK_BASE_URL")}
              oninput={(event) => updateConfig("DEEPSEEK_BASE_URL", inputValue(event))}
            />
          </label>
          <label>
            <span>真实上游模型</span>
            <select value={cfg("UPSTREAM_MODEL_OVERRIDE", "default")} onchange={(event) => updateConfig("UPSTREAM_MODEL_OVERRIDE", selectValue(event))}>
              <option value="default">默认</option>
              <option value="deepseek-v4-flash">Flash</option>
              <option value="deepseek-v4-pro">Pro</option>
            </select>
          </label>
          <label>
            <span>思考模式</span>
            <select value={cfg("DEEPSEEK_THINKING", "auto")} onchange={(event) => updateConfig("DEEPSEEK_THINKING", selectValue(event))}>
              <option value="auto">自动跟随</option>
              <option value="enabled">强制开启</option>
              <option value="disabled">强制关闭</option>
            </select>
          </label>
          <label>
            <span>采样温度</span>
            <select value={cfg("DEEPSEEK_TEMPERATURE_PRESET", "default")} onchange={(event) => updateConfig("DEEPSEEK_TEMPERATURE_PRESET", selectValue(event))}>
              <option value="default">默认</option>
              <option value="strict">严谨</option>
              <option value="balanced">均衡</option>
              <option value="general">通用</option>
              <option value="creative">创意</option>
            </select>
          </label>
          <label>
            <span>Catalog 模式</span>
            <select value={cfg("CATALOG_MODE", "default")} onchange={(event) => updateConfig("CATALOG_MODE", selectValue(event))}>
              <option value="default">默认</option>
              <option value="dynamic">自动</option>
              <option value="builtin">内置</option>
            </select>
          </label>
        </div>

        <div class="switch-row">
          <label class="switch-line">
            <input
              type="checkbox"
              checked={cfg("DEEPSEEK_OFFICIAL_V1_COMPAT", "true") === "true"}
              onchange={(event) => updateConfig("DEEPSEEK_OFFICIAL_V1_COMPAT", checkedValue(event))}
            />
            <span>DeepSeek 官方接口兼容 `/v1/chat/completions`</span>
          </label>
          <label class="switch-line">
            <input
              type="checkbox"
              checked={cfg("AUTO_START", "false") === "true"}
              onchange={(event) => updateConfig("AUTO_START", checkedValue(event))}
            />
            <span>开机自启动到托盘</span>
          </label>
        </div>

        {#if saveMessage}
          <p class="inline-status">{saveMessage}</p>
        {/if}
      </section>

      <section class="panel">
        <div class="panel-head">
          <div>
            <p class="label">Codex</p>
            <h2>config.toml 配置</h2>
          </div>
          <div class="copy-actions">
            {#if copiedToml}<span>已复制</span>{/if}
            <button class="ghost" onclick={copyToml} type="button">复制</button>
            <button class="ghost" onclick={refreshAdapter} type="button">重新生成</button>
          </div>
        </div>
        {#if adapter?.warning}
          <p class="notice">{adapter.warning}</p>
        {/if}
        <pre class="code-block">{adapter?.toml_snippet ?? "加载中..."}</pre>
      </section>
    {:else if activeView === "tools"}
      <section class="tool-grid">
        {#each tools as tool}
          <article class="tool-card">
            <div class="tool-head">
              <div class="tool-icon">{tool.name.slice(0, 1)}</div>
              <div>
                <h2>{tool.name}</h2>
                <div class="tags">
                  {#each tool.labels ?? [] as label}
                    <span>{label.label}</span>
                  {/each}
                </div>
              </div>
            </div>
            <p>{tool.description}</p>
            <div class="tool-foot">
              <small>{tool.system ? "系统工具" : tool.source ?? "built-in"}</small>
              {#if tool.configurable}
                <label class="toggle">
                  <input
                    type="checkbox"
                    checked={tool.enabled}
                    onchange={(event) => setToolEnabled(tool, checkedValue(event))}
                  />
                  <span></span>
                </label>
              {:else}
                <span class="locked">无开关</span>
              {/if}
            </div>
          </article>
        {/each}
      </section>
    {:else if activeView === "logs"}
      <section class="panel">
        <div class="panel-head">
          <div>
            <p class="label">最近请求</p>
            <h2>用量明细</h2>
          </div>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>时间</th>
                <th>模型</th>
                <th>缓存命中</th>
                <th>缓存未命中</th>
                <th>输出</th>
                <th>耗时</th>
              </tr>
            </thead>
            <tbody>
              {#each status?.runtime?.turn_history ?? [] as turn}
                <tr>
                  <td>{turn.completed_at}</td>
                  <td>{turn.model}</td>
                  <td>{formatNumber(turn.cached_input_tokens)}</td>
                  <td>{formatNumber(turn.cache_miss_input_tokens)}</td>
                  <td>{formatNumber(turn.output_tokens)}</td>
                  <td>{formatMs(turn.request_ms)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </section>

      <section class="event-list">
        {#each events as event}
          <article class:warn={event.level !== "info"}>
            <div>
              <strong>{event.type}</strong>
              <span>{event.ts}</span>
            </div>
            <p>{event.message}</p>
            {#if detailText(event.detail)}
              <pre>{detailText(event.detail)}</pre>
            {/if}
          </article>
        {/each}
      </section>
    {:else if activeView === "about"}
      <section class="panel about-card">
        <p class="label">关于</p>
        <h2>{appInfo?.product_name ?? appInfo?.name ?? "CodeSeeX"}</h2>
        <p>{appInfo?.description ?? "Local Codex and DeepSeek bridge."}</p>
        <dl class="details">
          <dt>版本</dt>
          <dd>{appInfo?.version ?? "-"}</dd>
          <dt>许可证</dt>
          <dd>{appInfo?.license ?? "-"}</dd>
          <dt>仓库</dt>
          <dd>
            {#if appInfo?.repository}
              <a href={appInfo.repository} target="_blank" rel="noreferrer">{appInfo.repository}</a>
            {:else}
              -
            {/if}
          </dd>
        </dl>
        <div class="about-actions">
          <button onclick={checkUpdate} type="button">检查更新</button>
          {#if updateCheck}
            {#if updateCheck.has_update}
              <span>发现更新 <a href={updateCheck.url} target="_blank" rel="noreferrer">{updateCheck.latest_version}</a></span>
            {:else if updateCheck.ok}
              <span>已是最新版本</span>
            {:else}
              <span>暂时无法访问：{updateCheck.error}</span>
            {/if}
          {/if}
        </div>
      </section>
    {/if}
  </section>
</main>
