use codeseex_core::catalog::{
    app_server_model_list_from_catalog, build_codeseex_catalog, AppServerModelListParams,
};
use codeseex_core::models::MODEL_PRO;
use codeseex_core::AppConfig;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_CODEX_DEBUG_PORT: u16 = 9222;
const CDP_HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const CODEX_LAUNCH_INJECT_ATTEMPTS: usize = 60;
const CODEX_LAUNCH_INJECT_INTERVAL: Duration = Duration::from_millis(500);
const CODEX_PACKAGE_IDENTITIES: &[&str] = &["OpenAI.Codex", "OpenAI.CodexBeta"];
#[cfg(target_os = "windows")]
const WINDOWS_CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Deserialize)]
struct CdpTarget {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "type")]
    target_type: String,
    #[serde(default, rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CodexProcess {
    pid: u32,
    name: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexProcessJson {
    pid: Option<u32>,
    name: Option<String>,
    path: Option<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone)]
struct WindowsPackagedCodexApp {
    app_user_model_id: String,
    package_full_name: Option<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Deserialize)]
struct WindowsPackagedCodexAppJson {
    #[serde(default, rename = "appUserModelId")]
    app_user_model_id: Option<String>,
    #[serde(default, rename = "packageFullName")]
    package_full_name: Option<String>,
}

pub(crate) fn default_debug_port() -> u16 {
    std::env::var("CODESEEX_CODEX_DEBUG_PORT")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_CODEX_DEBUG_PORT)
}

pub(crate) fn debug_port_from_values(query: Option<&Value>, body: Option<&Value>) -> Option<u16> {
    body.and_then(debug_port_from_value)
        .or_else(|| query.and_then(debug_port_from_value))
}

fn debug_port_from_value(value: &Value) -> Option<u16> {
    value
        .get("debugPort")
        .or_else(|| value.get("debug_port"))
        .and_then(|value| {
            value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .or_else(|| value.as_str().and_then(|text| text.parse::<u16>().ok()))
        })
}

pub(crate) fn codex_model_catalog_value(config: &AppConfig) -> Value {
    let catalog = build_codeseex_catalog();
    let app_server = app_server_model_list_from_catalog(
        &catalog,
        AppServerModelListParams {
            cursor: None,
            limit: Some(500),
            include_hidden: Some(true),
        },
    );
    let models = app_server
        .data
        .iter()
        .map(|model| model.model.clone())
        .collect::<Vec<_>>();
    let default_model = app_server
        .data
        .iter()
        .find(|model| model.is_default)
        .or_else(|| {
            app_server
                .data
                .iter()
                .find(|model| model.model == MODEL_PRO)
        })
        .or_else(|| app_server.data.first())
        .map(|model| model.model.clone())
        .unwrap_or_else(|| MODEL_PRO.to_owned());

    json!({
        "status": if models.is_empty() { "not_configured" } else { "ok" },
        "path": config.catalog_path().to_string_lossy(),
        "model": default_model.clone(),
        "model_provider": "custom",
        "provider_name": "CodeSeeX",
        "default_model": default_model,
        "models": models,
        "sources": [{
            "id": "codeseex:builtin-catalog",
            "type": "model_catalog_json",
            "name": "CodeSeeX built-in catalog",
            "path": config.catalog_path().to_string_lossy(),
            "status": "ok",
            "models": app_server.data.len()
        }],
        "responses_api": {
            "status": "supported",
            "endpoint": format!("{}/responses", config.proxy_base_url()),
            "message": ""
        },
        "appServer": app_server
    })
}

pub(crate) fn renderer_inject_script(catalog: &Value) -> String {
    const TEMPLATE: &str = r#"
(async () => {
  const VERSION = "codeseex-model-catalog-unlock-v2";
  const catalog = __CODESEEX_MODEL_CATALOG_JSON__;
  const state = window.__codeseexModelCatalogUnlock = window.__codeseexModelCatalogUnlock || {};
  state.version = VERSION;
  state.catalog = catalog;
  state.failures = state.failures || [];
  state.modules = state.modules || {};

  function rememberFailure(error) {
    try {
      state.failures.push(String(error && (error.stack || error.message) || error));
      if (state.failures.length > 20) state.failures.splice(0, state.failures.length - 20);
    } catch {}
  }

  function assetLabel(url) {
    try {
      const parsed = new URL(String(url || ""), location.href);
      return parsed.pathname.split("/").filter(Boolean).pop() || parsed.href;
    } catch {
      return String(url || "").slice(0, 200);
    }
  }

  function unique(values) {
    const out = [];
    for (const value of values) {
      const text = String(value || "").trim();
      if (text && !out.includes(text)) out.push(text);
    }
    return out;
  }

  function appServerModels() {
    const data = catalog && catalog.appServer && Array.isArray(catalog.appServer.data) ? catalog.appServer.data : [];
    return data.filter((model) => model && typeof model === "object");
  }

  function modelNames() {
    return unique([
      catalog && catalog.default_model,
      catalog && catalog.model,
      ...(Array.isArray(catalog && catalog.models) ? catalog.models : []),
      ...appServerModels().map((model) => model.model || model.id || model.slug)
    ]);
  }

  function sourceForModel(name) {
    return appServerModels().find((model) => (model.model || model.id || model.slug) === name) || null;
  }

  function normalizeModelDisplayName(value) {
    return String(value || "").trim().replace(/^DeepSeek-V4\b/, "DeepSeek V4");
  }

  function displayNameForModel(name) {
    const source = sourceForModel(name);
    return normalizeModelDisplayName(source && (source.displayName || source.name || source.display_name)) || name;
  }

  function shortDisplayNameForModel(name, displayName) {
    const source = sourceForModel(name);
    const explicit = source && (source.shortDisplayName || source.shortName || source.short_display_name || source.compactDisplayName);
    if (explicit && String(explicit).trim()) return String(explicit).trim();
    if (name === "deepseek-v4-flash") return "Flash";
    if (name === "deepseek-v4-pro") return "Pro";
    const full = normalizeModelDisplayName(displayName || "");
    if (full.startsWith("DeepSeek V4 ")) return full.slice("DeepSeek V4 ".length).trim() || full;
    return full || name;
  }

  function reasoningEfforts() {
    return ["minimal", "low", "medium", "high", "xhigh"].map((reasoningEffort) => ({
      reasoningEffort,
      description: `${reasoningEffort} effort`
    }));
  }

  const shortDisplayDescriptorKeys = [
    "shortDisplayName",
    "short_display_name",
    "shortName",
    "compactDisplayName",
    "selectedDisplayName",
    "selectedLabel",
    "compactLabel"
  ];

  function setDescriptorField(descriptor, key, value) {
    if (descriptor[key] === value) return false;
    descriptor[key] = value;
    return true;
  }

  function setDescriptorFields(descriptor, keys, value) {
    let changed = false;
    for (const key of keys) {
      if (setDescriptorField(descriptor, key, value)) changed = true;
    }
    return changed;
  }

  function baseDescriptor(name) {
    const displayName = displayNameForModel(name);
    const shortDisplayName = shortDisplayNameForModel(name, displayName);
    return {
      id: name,
      model: name,
      slug: name,
      name,
      displayName,
      display_name: displayName,
      shortDisplayName,
      short_display_name: shortDisplayName,
      compactDisplayName: shortDisplayName,
      selectedDisplayName: shortDisplayName,
      selectedLabel: shortDisplayName,
      compactLabel: shortDisplayName,
      description: (catalog && (catalog.provider_name || catalog.model_provider)) || "CodeSeeX model",
      hidden: false,
      isDefault: ((catalog && (catalog.default_model || catalog.model)) || "") === name,
      defaultReasoningEffort: "medium",
      supportedReasoningEfforts: reasoningEfforts(),
      inputModalities: ["text", "image"],
      supportsPersonality: false
    };
  }

  function patchModelDescriptor(descriptor, name) {
    if (!descriptor || typeof descriptor !== "object") return false;
    const modelName = name || itemModelName(descriptor);
    if (!modelName) return false;
    let changed = false;
    const displayName = normalizeModelDisplayName(descriptor.displayName || descriptor.name || displayNameForModel(modelName));
    const shortDisplayName = shortDisplayNameForModel(modelName, descriptor.shortDisplayName || displayName);
    if (setDescriptorField(descriptor, "displayName", displayName)) changed = true;
    if (setDescriptorField(descriptor, "display_name", displayName)) changed = true;
    if (!descriptor.name) {
      descriptor.name = displayName;
      changed = true;
    }
    if (setDescriptorFields(descriptor, shortDisplayDescriptorKeys, shortDisplayName)) changed = true;
    return changed;
  }

  function descriptorFor(name) {
    const source = sourceForModel(name);
    const descriptor = { ...baseDescriptor(name), ...(source || {}) };
    descriptor.id = descriptor.id || name;
    descriptor.model = descriptor.model || name;
    descriptor.slug = descriptor.slug || name;
    descriptor.name = descriptor.name || descriptor.displayName || name;
    descriptor.displayName = normalizeModelDisplayName(descriptor.displayName || descriptor.name || name);
    patchModelDescriptor(descriptor, name);
    descriptor.hidden = false;
    descriptor.isDefault = !!descriptor.isDefault || ((catalog && (catalog.default_model || catalog.model)) || "") === name;
    descriptor.defaultReasoningEffort = descriptor.defaultReasoningEffort || "medium";
    if (!Array.isArray(descriptor.supportedReasoningEfforts) || descriptor.supportedReasoningEfforts.length === 0) {
      descriptor.supportedReasoningEfforts = reasoningEfforts();
    }
    if (!Array.isArray(descriptor.inputModalities) || descriptor.inputModalities.length === 0) {
      descriptor.inputModalities = ["text", "image"];
    }
    return descriptor;
  }

  function itemModelName(item) {
    if (!item || typeof item !== "object") return "";
    return String(item.model || item.id || item.slug || "").trim();
  }

  function modelArrayLooksPatchable(value, allowEmpty) {
    return Array.isArray(value)
      && (allowEmpty || value.length > 0)
      && value.every((item) => item && typeof item === "object" && (
        typeof item.model === "string"
        || typeof item.defaultReasoningEffort === "string"
        || Array.isArray(item.supportedReasoningEfforts)
        || Array.isArray(item.inputModalities)
      ));
  }

  function stringArrayLooksPatchable(value) {
    return Array.isArray(value) && value.every((item) => typeof item === "string");
  }

  function patchModelNameArray(models) {
    if (!stringArrayLooksPatchable(models)) return false;
    let changed = false;
    for (const name of modelNames()) {
      if (!models.includes(name)) {
        models.push(name);
        changed = true;
      }
    }
    return changed;
  }

  function patchModelArray(models, allowEmpty) {
    if (!modelArrayLooksPatchable(models, !!allowEmpty)) return false;
    const names = modelNames();
    if (!names.length) return false;
    let changed = false;
    const existing = new Map();
    for (const item of models) {
      const name = itemModelName(item);
      if (name) existing.set(name, item);
      if (names.includes(name) && item.hidden !== false) {
        item.hidden = false;
        changed = true;
      }
      if (names.includes(name) && patchModelDescriptor(item, name)) changed = true;
    }
    for (const name of names) {
      if (!existing.has(name)) {
        models.push(descriptorFor(name));
        changed = true;
      }
    }
    return changed;
  }

  function catalogModelArray() {
    return modelNames().map((name) => descriptorFor(name));
  }

  function defaultModelDescriptor() {
    const defaultName = (catalog && (catalog.default_model || catalog.model)) || modelNames()[0] || "";
    return defaultName ? descriptorFor(defaultName) : null;
  }

  function replaceModelArrayWithCatalog(container, key) {
    if (!container || typeof container !== "object") return false;
    if (!Array.isArray(container[key])) return false;
    container[key] = catalogModelArray();
    return true;
  }

  function replaceModelContainerWithCatalog(value) {
    if (!value || typeof value !== "object") return false;
    let changed = false;
    const defaultModel = defaultModelDescriptor();
    if (replaceModelArrayWithCatalog(value, "data")) changed = true;
    if (replaceModelArrayWithCatalog(value, "models")) changed = true;
    if (replaceModelArrayWithCatalog(value.result, "data")) changed = true;
    if (replaceModelArrayWithCatalog(value.result, "models")) changed = true;
    if (replaceModelArrayWithCatalog(value.message && value.message.result, "data")) changed = true;
    if (defaultModel && ("data" in value || "models" in value || "result" in value)) {
      value.defaultModel = defaultModel;
      value.model = defaultModel.model || defaultModel.id || defaultModel.slug;
      changed = true;
    }
    if ("nextCursor" in value) {
      value.nextCursor = null;
      changed = true;
    }
    if (value.result && typeof value.result === "object" && "nextCursor" in value.result) {
      value.result.nextCursor = null;
      changed = true;
    }
    return changed;
  }

  function patchModelContainer(value, allowEmpty) {
    if (!value || typeof value !== "object") return false;
    let changed = false;
    const patchEmpty = !!allowEmpty;
    if (patchModelArray(value.models, patchEmpty || "defaultModel" in value)) changed = true;
    if (patchModelNameArray(value.models)) changed = true;
    if (patchModelArray(value.data, patchEmpty)) changed = true;
    if (patchModelArray(value.result, patchEmpty)) changed = true;
    if (patchModelArray(value.result && value.result.data, patchEmpty)) changed = true;
    if (patchModelArray(value.result && value.result.models, patchEmpty)) changed = true;
    if (patchModelArray(value.message && value.message.result && value.message.result.data, patchEmpty)) changed = true;
    const names = modelNames();
    if (value.defaultModel == null && names.length > 0 && ("models" in value || "data" in value || "result" in value)) {
      value.defaultModel = descriptorFor(names[0]);
      changed = true;
    }
    if (typeof value.defaultModel === "string" && names.includes(value.defaultModel) && value.model == null) {
      value.model = value.defaultModel;
      changed = true;
    }
    return changed;
  }

  const appServerRequestPatchVersion = VERSION;
  const modulePromises = new Map();

  function codexAppAssetUrl(namePart) {
    const urls = [
      ...Array.from(document.scripts || []).map((script) => script.src),
      ...Array.from(document.querySelectorAll("link[href]") || []).map((link) => link.href),
      ...performance.getEntriesByType("resource").map((entry) => entry.name)
    ].filter(Boolean);
    const url = urls.find((url) => url.includes("/assets/") && url.includes(namePart) && url.split("?")[0].endsWith(".js")) || "";
    state.assetProbe = state.assetProbe || {};
    state.assetProbe[namePart] = url ? assetLabel(url) : "";
    return url;
  }

  async function loadCodexAppModule(cacheKey, resolveUrl) {
    if (!modulePromises.has(cacheKey)) {
      modulePromises.set(cacheKey, Promise.resolve().then(async () => {
        const url = await resolveUrl();
        if (!url) throw new Error(`Codex App asset not found: ${cacheKey}`);
        const module = await import(url);
        state.modules[cacheKey] = {
          loaded: true,
          url: assetLabel(url),
          exportKeys: Object.keys(module || {}).slice(0, 30)
        };
        return module;
      }).catch((error) => {
        modulePromises.delete(cacheKey);
        state.modules[cacheKey] = {
          loaded: false,
          error: String(error && (error.message || error) || error)
        };
        throw error;
      }));
    }
    return await modulePromises.get(cacheKey);
  }

  async function resolveHostConfigModuleUrl() {
    const directUrl = codexAppAssetUrl("use-host-config-");
    state.hostConfig = state.hostConfig || {};
    if (directUrl) {
      state.hostConfig.resolvedBy = "direct_asset";
      state.hostConfig.url = assetLabel(directUrl);
      return directUrl;
    }
    const modelQueriesUrl = codexAppAssetUrl("model-queries-");
    if (!modelQueriesUrl) throw new Error("Codex App model-queries asset not found");
    state.hostConfig.modelQueriesUrl = assetLabel(modelQueriesUrl);
    const response = await fetch(modelQueriesUrl, { cache: "force-cache" });
    if (!response.ok) throw new Error(`Codex App model-queries asset fetch failed: ${response.status}`);
    const source = await response.text();
    const match = source.match(/from\s*["']\.\/(use-host-config-[^"']+\.js)["']/);
    if (!match) throw new Error("Codex App model-queries host-config import not found");
    const resolved = new URL(match[1], modelQueriesUrl).toString();
    state.hostConfig.resolvedBy = "model_queries_import";
    state.hostConfig.importName = match[1];
    state.hostConfig.url = assetLabel(resolved);
    return resolved;
  }

  function appServerRequestMethod(method, params) {
    if (method === "send-cli-request-for-host" && params && params.method) return String(params.method);
    return String(method || "");
  }

  function patchAppServerModelResult(method, result) {
    if (method !== "list-models-for-host") return result;
    if (Array.isArray(result)) return catalogModelArray();
    if (!replaceModelContainerWithCatalog(result)) patchModelContainer(result, true);
    return result;
  }

  function patchAppServerClient(client) {
    if (!client || typeof client.sendRequest !== "function") return false;
    if (client.__codeseexModelRequestPatch === appServerRequestPatchVersion) return true;
    const original = client.__codeseexOriginalSendRequest || client.sendRequest.bind(client);
    client.__codeseexOriginalSendRequest = original;
    client.sendRequest = async function codeseexPatchedSendRequest(method, params, options) {
      const resolvedMethod = appServerRequestMethod(String(method || ""), params);
      try {
        const result = await original(method, params, options);
        return patchAppServerModelResult(resolvedMethod, result);
      } catch (error) {
        if (resolvedMethod === "list-models-for-host" && modelNames().length > 0) {
          rememberFailure(error);
          return patchAppServerModelResult(resolvedMethod, { data: [] });
        }
        throw error;
      }
    };
    client.__codeseexModelRequestPatch = appServerRequestPatchVersion;
    return true;
  }

  function patchModelAvailabilityValue(value) {
    if (!value || typeof value !== "object") return value;
    const names = modelNames();
    if (!names.length) return value;
    const available = Array.isArray(value.available_models) ? value.available_models : [];
    const merged = unique([...available, ...names]);
    const defaultName = (catalog && (catalog.default_model || catalog.model)) || names[0] || "";
    let changed = merged.length !== available.length;
    const next = { ...value, available_models: merged };
    if (defaultName && (!next.default_model || !names.includes(String(next.default_model)))) {
      next.default_model = defaultName;
      changed = true;
    }
    return changed ? next : value;
  }

  function patchDynamicConfigResult(name, result) {
    if (String(name || "") !== "107580212") return result;
    if (!result || typeof result !== "object") return result;
    const currentValue = result.value && typeof result.value === "object" ? result.value : {};
    const nextValue = patchModelAvailabilityValue(currentValue);
    if (nextValue === currentValue) return result;
    const clone = Object.create(Object.getPrototypeOf(result) || Object.prototype);
    Object.assign(clone, result, { value: nextValue });
    if (typeof result.get === "function") {
      clone.get = function codeseexPatchedDynamicConfigGet(key, fallback) {
        if (key === "available_models") return nextValue.available_models;
        if (key === "default_model") return nextValue.default_model;
        return result.get.call(this, key, fallback);
      };
    }
    state.dynamicConfigPatch = {
      installed: true,
      configKey: "107580212",
      models: modelNames(),
      patchVersion: VERSION
    };
    return clone;
  }

  function collectStatsigClients() {
    const global = window.__STATSIG__;
    const clients = [];
    const push = (value) => {
      if (value && typeof value === "object" && typeof value.getDynamicConfig === "function" && !clients.includes(value)) {
        clients.push(value);
      }
    };
    push(global);
    push(global && global.firstInstance);
    if (global && typeof global.instance === "function") {
      try { push(global.instance()); } catch {}
    }
    if (global && global.instances && typeof global.instances === "object") {
      for (const value of Object.values(global.instances)) push(value);
    }
    return clients;
  }

  function patchStatsigDynamicConfig() {
    const clients = collectStatsigClients();
    let patched = 0;
    for (const client of clients) {
      if (client.__codeseexDynamicConfigPatch === VERSION) {
        patched += 1;
        continue;
      }
      const original = client.__codeseexOriginalGetDynamicConfig || client.getDynamicConfig.bind(client);
      client.__codeseexOriginalGetDynamicConfig = original;
      client.getDynamicConfig = function codeseexPatchedGetDynamicConfig(name, options) {
        const result = original(name, options);
        if (result && typeof result.then === "function") {
          return result.then((value) => patchDynamicConfigResult(name, value));
        }
        return patchDynamicConfigResult(name, result);
      };
      client.__codeseexDynamicConfigPatch = VERSION;
      patched += 1;
    }
    state.dynamicConfigPatch = {
      installed: patched > 0,
      clients: patched,
      configKey: "107580212",
      models: modelNames(),
      patchVersion: VERSION
    };
    return patched > 0;
  }

  async function installAppServerPatch() {
    const diagnostic = {
      attempted: true,
      installed: false,
      cacheKey: "use-host-config:request-bridge",
      expectedExport: "Vt"
    };
    state.appServerPatch = diagnostic;
    try {
      const module = await loadCodexAppModule("use-host-config:request-bridge", resolveHostConfigModuleUrl);
      diagnostic.moduleLoaded = !!module;
      diagnostic.exportKeys = Object.keys(module || {}).slice(0, 30);
      const requestBridge = module && module.Vt;
      diagnostic.exportFound = !!requestBridge;
      diagnostic.hasSendRequest = !!(requestBridge && typeof requestBridge.sendRequest === "function");
      if (!requestBridge || typeof requestBridge.sendRequest !== "function") {
        throw new Error("Codex App use-host-config request bridge export Vt.sendRequest not found");
      }
      diagnostic.installed = patchAppServerClient(requestBridge);
      diagnostic.patchVersion = requestBridge.__codeseexModelRequestPatch || null;
    } catch (error) {
      diagnostic.error = String(error && (error.message || error) || error);
      rememberFailure(error);
    }
    return diagnostic;
  }

  function modelListResultLooksPatchable(result) {
    if (!result || typeof result !== "object") return false;
    return modelArrayLooksPatchable(result, true)
      || modelArrayLooksPatchable(result.data, true)
      || modelArrayLooksPatchable(result.models, true)
      || stringArrayLooksPatchable(result.models);
  }

  function patchMcpModelResponseData(data) {
    if (!data || data.type !== "mcp-response") return false;
    const message = data.message || data.response;
    const method = String(
      (message && message.method)
      || (message && message.request && message.request.method)
      || (message && message.params && message.params.method)
      || ""
    );
    if (method && method !== "model/list" && method !== "list-models-for-host") return false;
    if (!method && !modelListResultLooksPatchable(message && message.result)) return false;
    let changed = false;
    if (patchModelContainer(message, true)) changed = true;
    if (patchModelContainer(message && message.result, true)) changed = true;
    if (patchModelContainer(message && message.result && message.result.data, true)) changed = true;
    if (patchModelArray(message && message.result, true)) changed = true;
    if (patchModelArray(message && message.result && message.result.data, true)) changed = true;
    if (patchModelArray(message && message.result && message.result.models, true)) changed = true;
    return changed;
  }

  function installMessagePatch() {
    if (window.__codeseexModelMessagePatch === VERSION) return;
    window.addEventListener("message", (event) => {
      try {
        patchMcpModelResponseData(event && event.data);
      } catch (error) {
        rememberFailure(error);
      }
    }, true);
    window.__codeseexModelMessagePatch = VERSION;
    state.messagePatch = {
      installed: true,
      mode: "window_message_listener",
      patchVersion: VERSION
    };
  }

  function modelDisplayLabelPairs() {
    const pairs = [];
    for (const name of modelNames()) {
      const full = displayNameForModel(name);
      const short = shortDisplayNameForModel(name, full);
      for (const label of unique([full, normalizeModelDisplayName(full), String(full || "").replace(/^DeepSeek V4\b/, "DeepSeek-V4"), name])) {
        if (label && short && label !== short) pairs.push({ full: label, short });
      }
    }
    return pairs.sort((left, right) => right.full.length - left.full.length);
  }

  function isInsideDropdownMenu(element) {
    return !!(element && element.closest([
      "[role=\"menu\"]",
      "[role=\"menuitem\"]",
      "[role=\"listbox\"]",
      "[role=\"option\"]",
      "[data-radix-menu-content]",
      "[data-radix-popper-content-wrapper]"
    ].join(",")));
  }

  function isModelSelectorTriggerElement(element) {
    if (!element || isInsideDropdownMenu(element)) return false;
    const popup = element.getAttribute("aria-haspopup");
    return popup === "menu"
      || popup === "true"
      || element.hasAttribute("aria-expanded")
      || element.hasAttribute("data-state");
  }

  function replaceModelLabelInElement(element, pair) {
    if (!element || !pair) return 0;
    let replacements = 0;
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    for (const node of nodes) {
      const text = node.nodeValue || "";
      if (!text.includes(pair.full)) continue;
      node.nodeValue = text.replaceAll(pair.full, pair.short);
      replacements += 1;
    }
    if (replacements > 0) return replacements;

    const descendants = Array.from(element.querySelectorAll ? element.querySelectorAll("*") : [])
      .filter((item) => item && !isInsideDropdownMenu(item) && !["svg", "path"].includes(String(item.tagName || "").toLowerCase()))
      .reverse();
    for (const item of descendants) {
      const text = String(item.textContent || "");
      if (!text.includes(pair.full)) continue;
      item.textContent = text.replaceAll(pair.full, pair.short);
      return 1;
    }
    return 0;
  }

  function shortenSelectedModelButtonLabels(root) {
    const pairs = modelDisplayLabelPairs();
    const rootNode = root && root.nodeType ? root : document.body;
    if (!pairs.length || !rootNode) return;
    const triggers = [];
    if (rootNode.nodeType === Node.ELEMENT_NODE && rootNode.matches && rootNode.matches("button,[role=\"button\"]")) {
      triggers.push(rootNode);
    }
    if (rootNode.querySelectorAll) {
      triggers.push(...Array.from(rootNode.querySelectorAll("button,[role=\"button\"]")));
    }
    let replacements = 0;
    for (const trigger of triggers) {
      if (isInsideDropdownMenu(trigger)) continue;
      const triggerText = String(trigger.innerText || trigger.textContent || "");
      const matchingPairs = pairs.filter((pair) => triggerText.includes(pair.full));
      if (!matchingPairs.length && !isModelSelectorTriggerElement(trigger)) continue;
      for (const pair of matchingPairs) {
        replacements += replaceModelLabelInElement(trigger, pair);
      }
    }
    state.shortLabelPatch = {
      scannedTriggers: triggers.length,
      replacements,
      patchVersion: VERSION
    };
  }

  function rendererDiagnostic(reason) {
    return {
      version: VERSION,
      reason,
      models: modelNames(),
      assetProbe: state.assetProbe || {},
      hostConfig: state.hostConfig || {},
      modules: state.modules || {},
      appServerPatch: state.appServerPatch || { attempted: false, installed: false },
      dynamicConfigPatch: state.dynamicConfigPatch || { installed: false },
      messagePatch: state.messagePatch || { installed: false },
      shortLabelPatch: state.shortLabelPatch || { replacements: 0 },
      failures: (state.failures || []).slice(-8)
    };
  }

  async function refresh(reason) {
    try {
      shortenSelectedModelButtonLabels(document.body);
      await installAppServerPatch();
      patchStatsigDynamicConfig();
      shortenSelectedModelButtonLabels(document.body);
    } catch (error) {
      rememberFailure(error);
    }
    return rendererDiagnostic(reason || "refresh");
  }

  installMessagePatch();
  const initialDiagnostic = await refresh("initial");

  if (window.__codeseexModelCatalogObserver) {
    try { window.__codeseexModelCatalogObserver.disconnect(); } catch {}
  }
  let refreshTimer = 0;
  window.__codeseexModelCatalogObserver = new MutationObserver(() => {
    try { shortenSelectedModelButtonLabels(document.body); } catch {}
    clearTimeout(refreshTimer);
    refreshTimer = window.setTimeout(() => { void refresh("mutation"); }, 30);
  });
  window.__codeseexModelCatalogObserver.observe(document.documentElement || document.body, { childList: true, subtree: true });

  if (window.__codeseexModelCatalogClickHandler) {
    document.removeEventListener("click", window.__codeseexModelCatalogClickHandler, true);
  }
  window.__codeseexModelCatalogClickHandler = () => {
    window.setTimeout(() => {
      try { shortenSelectedModelButtonLabels(document.body); } catch {}
    }, 0);
  };
  document.addEventListener("click", window.__codeseexModelCatalogClickHandler, true);

  clearInterval(window.__codeseexModelCatalogInterval);
  let remaining = 40;
  window.__codeseexModelCatalogInterval = window.setInterval(() => {
    void refresh("timer");
    remaining -= 1;
    if (remaining <= 0) clearInterval(window.__codeseexModelCatalogInterval);
  }, 500);
  return initialDiagnostic;
})();
"#;

    let catalog_json = serde_json::to_string(catalog).unwrap_or_else(|_| "{}".to_owned());
    TEMPLATE.replace("__CODESEEX_MODEL_CATALOG_JSON__", &catalog_json)
}

pub(crate) async fn inject_model_catalog(debug_port: u16, catalog: Value) -> anyhow::Result<Value> {
    let target = pick_codex_target(&list_targets(debug_port).await?)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("selected Codex CDP target has no websocket URL"))?;
    let script = renderer_inject_script(&catalog);
    let renderer_state = inject_script(websocket_url, &script).await?;
    let app_server_patch_installed = renderer_state
        .pointer("/appServerPatch/installed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(json!({
        "ok": app_server_patch_installed,
        "status": if app_server_patch_installed { "injected" } else { "evaluated_without_app_server_patch" },
        "debug_port": debug_port,
        "target": {
            "id": target.id,
            "title": target.title,
            "url": target.url
        },
        "renderer_state": renderer_state,
        "models": catalog.get("models").cloned().unwrap_or_else(|| json!([]))
    }))
}

pub(crate) async fn launch_and_inject_model_catalog(
    debug_port: u16,
    catalog: Value,
) -> anyhow::Result<Value> {
    match inject_model_catalog(debug_port, catalog.clone()).await {
        Ok(mut injected) => {
            if let Some(object) = injected.as_object_mut() {
                object.insert(
                    "launch".to_owned(),
                    json!({ "mode": "existing_debug_port" }),
                );
                object.insert("attempt".to_owned(), json!(0));
            }
            return Ok(injected);
        }
        Err(initial_error) => {
            let running = running_codex_processes();
            if !running.is_empty() {
                anyhow::bail!(
                    "Codex is already running but remote debugging is not available on port {debug_port}. Fully quit Codex, including background processes, then launch it from CodeSeeX. Running processes: {}. Initial CDP check: {initial_error}",
                    codex_process_summary(&running)
                );
            }
        }
    }

    let launch = launch_codex_app(debug_port)?;
    let mut last_error = None;
    for attempt in 0..CODEX_LAUNCH_INJECT_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(CODEX_LAUNCH_INJECT_INTERVAL).await;
        }
        match inject_model_catalog(debug_port, catalog.clone()).await {
            Ok(mut injected) => {
                if let Some(object) = injected.as_object_mut() {
                    object.insert("launch".to_owned(), launch.clone());
                    object.insert("attempt".to_owned(), json!(attempt + 1));
                }
                if model_catalog_injection_effective(&injected) {
                    return Ok(injected);
                }
                last_error =
                    Some(serde_json::to_string(&injected).unwrap_or_else(|_| {
                        "renderer evaluated without app-server patch".to_owned()
                    }));
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    anyhow::bail!(
        "Codex was launched but the renderer could not be injected on debug port {debug_port}: {}",
        last_error.unwrap_or_else(|| "unknown CDP error".to_owned())
    )
}

fn model_catalog_injection_effective(value: &Value) -> bool {
    value.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn codex_process_summary(processes: &[CodexProcess]) -> String {
    processes
        .iter()
        .take(8)
        .map(|process| {
            if process.path.is_empty() {
                format!("{} pid {}", process.name, process.pid)
            } else {
                format!("{} pid {} ({})", process.name, process.pid, process.path)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn launch_codex_app(debug_port: u16) -> anyhow::Result<Value> {
    #[cfg(target_os = "windows")]
    {
        if let Some(executable) = find_codex_executable() {
            if let Some(app_user_model_id) = packaged_app_user_model_id_from_path(&executable) {
                return activate_packaged_codex_app(
                    &app_user_model_id,
                    debug_port,
                    Some(&executable),
                    None,
                );
            }
            return launch_codex_process(&executable, debug_port);
        }

        if let Some(app) = find_windows_packaged_codex_app() {
            return activate_packaged_codex_app(
                &app.app_user_model_id,
                debug_port,
                None,
                app.package_full_name.as_deref(),
            );
        }

        anyhow::bail!("Codex executable or packaged app entry was not found");
    }

    #[cfg(not(target_os = "windows"))]
    {
        let executable = find_codex_executable()
            .ok_or_else(|| anyhow::anyhow!("Codex executable was not found"))?;
        #[cfg(target_os = "macos")]
        if let Some(app_bundle) = macos_app_bundle_from_executable(&executable) {
            return launch_macos_codex_app_bundle(&app_bundle, debug_port, &executable);
        }

        launch_codex_process(&executable, debug_port)
    }
}

fn launch_codex_process(executable: &std::path::Path, debug_port: u16) -> anyhow::Result<Value> {
    #[cfg(target_os = "windows")]
    if let Some(app_user_model_id) = packaged_app_user_model_id_from_path(executable) {
        return activate_packaged_codex_app(&app_user_model_id, debug_port, Some(executable), None);
    }

    let arguments = codex_launch_arguments(debug_port);
    let mut command = Command::new(executable);
    command.args(&arguments);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let child = command.spawn()?;
    Ok(json!({
        "mode": "process",
        "path": executable.to_string_lossy(),
        "arguments": arguments,
        "pid": child.id(),
        "debug_port": debug_port
    }))
}

#[cfg(target_os = "macos")]
fn launch_macos_codex_app_bundle(
    app_bundle: &std::path::Path,
    debug_port: u16,
    executable: &std::path::Path,
) -> anyhow::Result<Value> {
    let arguments = codex_launch_arguments(debug_port);
    let mut command = Command::new("open");
    command
        .arg("-n")
        .arg(app_bundle)
        .arg("--args")
        .args(&arguments);
    let child = command.spawn()?;
    Ok(json!({
        "mode": "macos_open",
        "app_bundle": app_bundle.to_string_lossy(),
        "path": executable.to_string_lossy(),
        "arguments": arguments,
        "pid": child.id(),
        "debug_port": debug_port
    }))
}

#[cfg(target_os = "windows")]
fn activate_packaged_codex_app(
    app_user_model_id: &str,
    debug_port: u16,
    executable: Option<&std::path::Path>,
    package_full_name: Option<&str>,
) -> anyhow::Result<Value> {
    let arguments = codex_launch_arguments(debug_port);
    let argument_line = windows_command_line_arguments(&arguments);
    let script = r#"
$ErrorActionPreference = 'Stop'
$aumid = [string]$env:CODESEEX_CODEX_AUMID
$arguments = [string]$env:CODESEEX_CODEX_ARGS
$source = @'
using System;
using System.Runtime.InteropServices;

public enum ActivateOptions
{
    None = 0
}

[ComImport]
[Guid("2e941141-7f97-4756-ba1d-9decde894a3d")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IApplicationActivationManager
{
    [PreserveSig]
    int ActivateApplication(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        [MarshalAs(UnmanagedType.LPWStr)] string arguments,
        ActivateOptions options,
        out uint processId);

    [PreserveSig]
    int ActivateForFile(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        IntPtr itemArray,
        [MarshalAs(UnmanagedType.LPWStr)] string verb,
        out uint processId);

    [PreserveSig]
    int ActivateForProtocol(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        IntPtr itemArray,
        out uint processId);
}

[ComImport]
[Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
class ApplicationActivationManager
{
}

public static class CodeSeeXPackagedAppActivator
{
    public static uint Activate(string appUserModelId, string arguments)
    {
        var manager = (IApplicationActivationManager)new ApplicationActivationManager();
        uint processId;
        int hr = manager.ActivateApplication(appUserModelId, arguments ?? "", ActivateOptions.None, out processId);
        if (hr < 0)
        {
            Marshal.ThrowExceptionForHR(hr);
        }
        return processId;
    }
}
'@
Add-Type -TypeDefinition $source -Language CSharp
[CodeSeeXPackagedAppActivator]::Activate($aumid, $arguments)
"#;
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("CODESEEX_CODEX_AUMID", app_user_model_id)
        .env("CODESEEX_CODEX_ARGS", &argument_line);
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let output = command.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        anyhow::bail!(
            "Codex packaged app activation failed for {app_user_model_id}: {}",
            if stderr.is_empty() { stdout } else { stderr }
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid = stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().parse::<u32>().ok());
    let path = executable.map(|path| path.to_string_lossy().into_owned());
    Ok(json!({
        "mode": "packaged_activation",
        "app_user_model_id": app_user_model_id,
        "package_full_name": package_full_name,
        "arguments": argument_line,
        "path": path,
        "pid": pid,
        "debug_port": debug_port
    }))
}

fn codex_launch_arguments(debug_port: u16) -> Vec<String> {
    vec![
        format!("--remote-debugging-port={debug_port}"),
        format!("--remote-allow-origins=http://127.0.0.1:{debug_port}"),
    ]
}

#[cfg(target_os = "windows")]
fn windows_command_line_arguments(args: &[String]) -> String {
    args.iter()
        .map(|arg| windows_quote_command_line_argument(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "windows")]
fn windows_quote_command_line_argument(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'))
    {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn packaged_app_user_model_id_from_path(path: &std::path::Path) -> Option<String> {
    path.components()
        .rev()
        .filter_map(|component| component.as_os_str().to_str())
        .find_map(|name| {
            codex_package_parts(name)
                .map(|(identity, _version, publisher_id)| format!("{identity}_{publisher_id}!App"))
        })
}

fn codex_package_parts(package_name: &str) -> Option<(&str, &str, &str)> {
    for identity in CODEX_PACKAGE_IDENTITIES {
        let Some(rest) = package_name.strip_prefix(identity) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('_') else {
            continue;
        };
        let Some((version, rest)) = rest.split_once('_') else {
            continue;
        };
        let Some((_, publisher_id)) = rest.rsplit_once("__") else {
            continue;
        };
        if publisher_id.is_empty() {
            continue;
        }
        return Some((*identity, version, publisher_id));
    }
    None
}

#[cfg(target_os = "windows")]
fn find_windows_packaged_codex_app() -> Option<WindowsPackagedCodexApp> {
    for key in [
        "CODESEEX_CODEX_APP_AUMID",
        "CODEX_APP_AUMID",
        "CODESEEX_CODEX_AUMID",
    ] {
        let Some(value) = std::env::var(key)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        return Some(WindowsPackagedCodexApp {
            app_user_model_id: value,
            package_full_name: None,
        });
    }

    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$items = New-Object System.Collections.Generic.List[object]
Get-StartApps | Where-Object { $_.AppID -like 'OpenAI.Codex*!*' } | ForEach-Object {
  $items.Add([pscustomobject]@{
    appUserModelId = [string]$_.AppID
    packageFullName = $null
  })
}
Get-AppxPackage | Where-Object { $_.Name -eq 'OpenAI.Codex' -or $_.Name -eq 'OpenAI.CodexBeta' } | ForEach-Object {
  $items.Add([pscustomobject]@{
    appUserModelId = "$($_.PackageFamilyName)!App"
    packageFullName = [string]$_.PackageFullName
  })
}
$items | Sort-Object appUserModelId -Unique | ConvertTo-Json -Compress
"#;
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let Ok(output) = command.output() else {
        return None;
    };
    if !output.status.success() {
        return None;
    }
    parse_windows_packaged_codex_apps_json(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .next()
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_packaged_codex_apps_json(text: &str) -> Vec<WindowsPackagedCodexApp> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(items) = serde_json::from_str::<Vec<WindowsPackagedCodexAppJson>>(trimmed) {
        return items
            .into_iter()
            .filter_map(windows_packaged_codex_app_from_json)
            .collect();
    }
    serde_json::from_str::<WindowsPackagedCodexAppJson>(trimmed)
        .ok()
        .and_then(windows_packaged_codex_app_from_json)
        .into_iter()
        .collect()
}

#[cfg(any(target_os = "windows", test))]
fn windows_packaged_codex_app_from_json(
    value: WindowsPackagedCodexAppJson,
) -> Option<WindowsPackagedCodexApp> {
    let app_user_model_id = value
        .app_user_model_id
        .map(|value| value.trim().to_owned())
        .filter(|value| is_codex_app_user_model_id(value))?;
    Some(WindowsPackagedCodexApp {
        app_user_model_id,
        package_full_name: value.package_full_name.filter(|value| !value.is_empty()),
    })
}

#[cfg(any(target_os = "windows", test))]
fn is_codex_app_user_model_id(value: &str) -> bool {
    CODEX_PACKAGE_IDENTITIES
        .iter()
        .any(|identity| value.starts_with(&format!("{identity}_")) && value.contains('!'))
}

#[cfg(target_os = "windows")]
fn running_codex_processes() -> Vec<CodexProcess> {
    let script = r#"
Get-Process -ErrorAction SilentlyContinue |
  Where-Object { $_.ProcessName -ieq 'Codex' } |
  Select-Object @{Name='pid';Expression={$_.Id}},@{Name='name';Expression={[string]$_.ProcessName}},@{Name='path';Expression={if ($_.Path) {[string]$_.Path} else {''}}} |
  ConvertTo-Json -Compress
"#;
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_windows_codex_processes_json(&String::from_utf8_lossy(&output.stdout))
}

fn parse_windows_codex_processes_json(text: &str) -> Vec<CodexProcess> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(items) = serde_json::from_str::<Vec<CodexProcessJson>>(trimmed) {
        return items
            .into_iter()
            .filter_map(codex_process_from_json)
            .collect();
    }
    serde_json::from_str::<CodexProcessJson>(trimmed)
        .ok()
        .and_then(codex_process_from_json)
        .into_iter()
        .collect()
}

fn codex_process_from_json(value: CodexProcessJson) -> Option<CodexProcess> {
    Some(CodexProcess {
        pid: value.pid.filter(|pid| *pid > 0)?,
        name: value.name.unwrap_or_else(|| "Codex".to_owned()),
        path: value.path.unwrap_or_default(),
    })
}

#[cfg(unix)]
fn running_codex_processes_from_ps() -> Vec<CodexProcess> {
    let Ok(output) = Command::new("ps")
        .args(["-eo", "pid=,comm=,args="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_unix_codex_processes(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn running_codex_processes() -> Vec<CodexProcess> {
    running_codex_processes_from_ps()
}

#[cfg(all(
    not(target_os = "windows"),
    not(target_os = "macos"),
    not(target_os = "linux")
))]
fn running_codex_processes() -> Vec<CodexProcess> {
    Vec::new()
}

#[cfg(any(unix, test))]
fn parse_unix_codex_processes(text: &str) -> Vec<CodexProcess> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let (pid, rest) = trimmed.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (name, args) = rest
                .split_once(char::is_whitespace)
                .map(|(name, args)| (name, args.trim()))
                .unwrap_or((rest, ""));
            let base_name = std::path::Path::new(name)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(name);
            if !base_name.eq_ignore_ascii_case("codex") {
                return None;
            }
            Some(CodexProcess {
                pid: pid.parse::<u32>().ok()?,
                name: base_name.to_owned(),
                path: args.to_owned(),
            })
        })
        .collect()
}

fn find_codex_executable() -> Option<PathBuf> {
    for key in ["CODESEEX_CODEX_APP_EXE", "CODEX_APP_EXE", "CODEX_APP_PATH"] {
        if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
            if path.is_file() {
                return Some(path);
            }
            let exe = codex_executable_in_dir(&path);
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    platform_codex_executable_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

fn codex_executable_in_dir(path: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return path.join("Contents").join("MacOS").join("Codex");
    }
    #[cfg(target_os = "windows")]
    {
        return path.join("Codex.exe");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let lowercase = path.join("codex");
        if lowercase.is_file() {
            lowercase
        } else {
            path.join("Codex")
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_app_bundle_from_executable(path: &std::path::Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.extension().and_then(|value| value.to_str()) == Some("app"))
        .map(std::path::Path::to_path_buf)
}

fn platform_codex_executable_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "windows")]
    {
        append_windows_codex_candidates(&mut candidates);
    }
    #[cfg(target_os = "macos")]
    {
        append_macos_codex_candidates(&mut candidates, &PathBuf::from("/Applications"));
        if let Some(home) = dirs_next::home_dir() {
            append_macos_codex_candidates(&mut candidates, &home.join("Applications"));
            candidates.push(home.join(".local").join("bin").join("codex"));
        }
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs_next::home_dir() {
            candidates.push(home.join(".local").join("bin").join("codex"));
        }
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/usr/bin/codex"));
        candidates.push(PathBuf::from("/opt/Codex/codex"));
        candidates.push(PathBuf::from("/opt/codex/codex"));
    }
    dedup_paths_preserve_order(&mut candidates);
    candidates
}

fn dedup_paths_preserve_order(paths: &mut Vec<PathBuf>) {
    let mut seen = Vec::<PathBuf>::new();
    paths.retain(|path| {
        if seen.iter().any(|existing| existing == path) {
            false
        } else {
            seen.push(path.clone());
            true
        }
    });
}

#[cfg(target_os = "macos")]
fn append_macos_codex_candidates(candidates: &mut Vec<PathBuf>, root: &std::path::Path) {
    for name in ["Codex.app", "OpenAI Codex.app", "OpenAI.Codex.app"] {
        candidates.push(root.join(name).join("Contents").join("MacOS").join("Codex"));
    }
}

#[cfg(target_os = "windows")]
fn append_windows_codex_candidates(candidates: &mut Vec<PathBuf>) {
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        candidates.push(
            local
                .join("OpenAI")
                .join("Codex")
                .join("app")
                .join("Codex.exe"),
        );
        candidates.push(local.join("OpenAI").join("Codex").join("Codex.exe"));
        candidates.push(
            local
                .join("Programs")
                .join("OpenAI")
                .join("Codex")
                .join("Codex.exe"),
        );
    }
    append_windows_appx_candidates(candidates);
    dedup_paths_preserve_order(candidates);
}

#[cfg(target_os = "windows")]
fn append_windows_appx_candidates(candidates: &mut Vec<PathBuf>) {
    let mut roots = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
        roots.push(program_files.join("WindowsApps"));
    }
    if let Some(program_w6432) = std::env::var_os("ProgramW6432").map(PathBuf::from) {
        roots.push(program_w6432.join("WindowsApps"));
    }
    roots.push(PathBuf::from(r"C:\Program Files\WindowsApps"));
    roots.sort();
    roots.dedup();

    let mut appx = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !(name.starts_with("OpenAI.Codex_") || name.starts_with("OpenAI.CodexBeta_")) {
                continue;
            }
            let exe = path.join("app").join("Codex.exe");
            if exe.is_file() {
                appx.push(exe);
            }
        }
    }
    appx.sort();
    appx.reverse();
    candidates.extend(appx);
}

async fn list_targets(debug_port: u16) -> anyhow::Result<Vec<CdpTarget>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(CDP_HTTP_TIMEOUT)
        .build()?;
    let urls = [
        format!("http://127.0.0.1:{debug_port}/json"),
        format!("http://[::1]:{debug_port}/json"),
    ];
    let mut errors = Vec::new();
    for url in urls {
        match client.get(&url).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<Vec<CdpTarget>>().await {
                    Ok(targets) => return Ok(targets),
                    Err(error) => errors.push(format!("{url}: {error}")),
                },
                Err(error) => errors.push(format!("{url}: {error}")),
            },
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }
    anyhow::bail!(
        "failed to query Codex CDP targets on debug port {debug_port}: {}",
        errors.join("; ")
    )
}

fn pick_codex_target(targets: &[CdpTarget]) -> anyhow::Result<CdpTarget> {
    let mut first_page = None;
    for target in targets {
        if target.target_type != "page"
            || target
                .web_socket_debugger_url
                .as_deref()
                .unwrap_or_default()
                .is_empty()
        {
            continue;
        }
        first_page.get_or_insert_with(|| target.clone());
        let haystack = format!("{} {}", target.title, target.url).to_ascii_lowercase();
        if haystack.contains("codex") {
            return Ok(target.clone());
        }
    }
    first_page.ok_or_else(|| anyhow::anyhow!("no injectable Codex page target found"))
}

async fn inject_script(websocket_url: &str, script: &str) -> anyhow::Result<Value> {
    let (mut socket, _) = tokio_tungstenite::connect_async(websocket_url).await?;
    send_cdp_command(&mut socket, 1, "Runtime.enable", json!({})).await?;
    let response = send_cdp_command(
        &mut socket,
        2,
        "Runtime.evaluate",
        json!({
            "expression": script,
            "awaitPromise": true,
            "returnByValue": true
        }),
    )
    .await?;
    let _ = socket.close(None).await;
    if let Some(exception) = response.pointer("/result/exceptionDetails") {
        anyhow::bail!("Codex renderer injection threw an exception: {exception}");
    }
    Ok(response
        .pointer("/result/result/value")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "appServerPatch": {
                    "attempted": false,
                    "installed": false
                },
                "error": "Runtime.evaluate returned no by-value renderer diagnostic"
            })
        }))
}

async fn send_cdp_command(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    socket
        .send(Message::Text(
            json!({
                "id": id,
                "method": method,
                "params": params
            })
            .to_string(),
        ))
        .await?;

    while let Some(message) = socket.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => anyhow::bail!("CDP websocket closed before {method} response"),
            Message::Frame(_) => continue,
        };
        let value: Value = serde_json::from_str(&text)?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("CDP {method} failed: {error}");
        }
        return Ok(value);
    }
    anyhow::bail!("CDP websocket ended before {method} response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_script_embeds_catalog_and_app_server_shortcut() {
        let catalog = json!({
            "model": "deepseek-v4-pro",
            "default_model": "deepseek-v4-pro",
            "models": ["deepseek-v4-flash", "deepseek-v4-pro"],
            "appServer": {
                "data": [
                    {
                        "id": "deepseek-v4-flash",
                        "model": "deepseek-v4-flash",
                        "displayName": "DeepSeek V4 Flash",
                        "shortDisplayName": "Flash",
                        "hidden": false,
                        "isDefault": false
                    },
                    {
                        "id": "deepseek-v4-pro",
                        "model": "deepseek-v4-pro",
                        "displayName": "DeepSeek V4 Pro",
                        "shortDisplayName": "Pro",
                        "hidden": false,
                        "isDefault": true
                    }
                ]
            }
        });
        let script = renderer_inject_script(&catalog);
        assert!(script.contains("deepseek-v4-flash"));
        assert!(script.contains("deepseek-v4-pro"));
        assert!(script.contains("list-models-for-host"));
        assert!(script.contains("model-queries-"));
        assert!(script.contains("use-host-config:request-bridge"));
        assert!(script.contains("module && module.Vt"));
        assert!(script.contains("shortenSelectedModelButtonLabels"));
        assert!(script.contains("message.method"));
        assert!(script.contains("modelListResultLooksPatchable"));
        assert!(script.contains("replaceModelContainerWithCatalog"));
        assert!(script.contains("if (Array.isArray(result)) return catalogModelArray();"));
        assert!(script.contains("const initialDiagnostic = await refresh(\"initial\");"));
        assert!(script.contains("return initialDiagnostic;"));
        assert!(script.contains("assetProbe"));
        assert!(script.contains("appServerPatch"));
        assert!(script.contains("const result = await original(method, params, options);"));
        assert!(script.contains("return patchAppServerModelResult(resolvedMethod, result);"));
        assert!(script.contains("return patchAppServerModelResult(resolvedMethod, { data: [] });"));
        assert!(script.contains("button,[role=\\\"button\\\"]"));
        assert!(script.contains("replaceAll(pair.full, pair.short)"));
        assert!(script.contains("scannedTriggers"));
        assert!(script.contains("if (patchModelArray(value.data, patchEmpty))"));
        assert!(script.contains("patchStatsigDynamicConfig"));
        assert!(script.contains("patchModelAvailabilityValue"));
        assert!(script.contains("available_models"));
        assert!(script.contains("107580212"));
        assert!(script.contains("dynamicConfigPatch"));
        assert!(!script.contains("Response.prototype.json"));
        assert!(!script.contains("window.dispatchEvent ="));
        assert!(!script.contains("MODEL_DYNAMIC_CONFIG_NAMES"));
        assert!(!script.contains("patchReactModelState"));
        assert!(!script.contains("patchObjectGraph"));
        assert!(!script.contains("Object.values(module)"));
        assert!(!script.contains("app-server-manager-signals-"));
        assert!(!script.contains("value.pages"));
    }

    #[test]
    fn parses_windows_codex_process_json_shapes() {
        let single = parse_windows_codex_processes_json(
            r#"{"pid":2760,"name":"Codex","path":"C:\\Program Files\\WindowsApps\\OpenAI.Codex\\app\\Codex.exe"}"#,
        );
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].pid, 2760);
        assert_eq!(single[0].name, "Codex");

        let many = parse_windows_codex_processes_json(
            r#"[{"pid":2760,"name":"Codex","path":"a"},{"pid":16212,"name":"codex","path":"b"}]"#,
        );
        assert_eq!(many.len(), 2);
        assert_eq!(
            codex_process_summary(&many),
            "Codex pid 2760 (a); codex pid 16212 (b)"
        );
        assert!(parse_windows_codex_processes_json("").is_empty());
    }

    #[test]
    fn parses_unix_codex_process_list() {
        let processes = parse_unix_codex_processes(
            "123 Codex /Applications/Codex.app/Contents/MacOS/Codex --remote-debugging-port=9222\n456 bash bash -lc codex\n789 codex /usr/local/bin/codex\n",
        );
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid, 123);
        assert_eq!(processes[0].name, "Codex");
        assert_eq!(processes[1].pid, 789);
        assert_eq!(processes[1].path, "/usr/local/bin/codex");
    }

    #[test]
    fn derives_macos_app_bundle_from_executable() {
        let path = std::path::Path::new("/Applications/Codex.app/Contents/MacOS/Codex");
        assert_eq!(
            macos_app_bundle_from_executable(path)
                .as_deref()
                .and_then(std::path::Path::to_str),
            Some("/Applications/Codex.app")
        );
        assert!(
            macos_app_bundle_from_executable(std::path::Path::new("/usr/local/bin/codex"))
                .is_none()
        );
    }

    #[test]
    fn dedups_candidate_paths_without_reordering() {
        let mut paths = vec![
            PathBuf::from("local"),
            PathBuf::from("appx-new"),
            PathBuf::from("local"),
            PathBuf::from("appx-old"),
        ];
        dedup_paths_preserve_order(&mut paths);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("local"),
                PathBuf::from("appx-new"),
                PathBuf::from("appx-old")
            ]
        );
    }

    #[test]
    fn derives_packaged_codex_app_user_model_id() {
        let path = std::path::Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.616.6631.0_x64__2p2nqsd0c76g0\app\Codex.exe",
        );
        assert_eq!(
            packaged_app_user_model_id_from_path(path).as_deref(),
            Some("OpenAI.Codex_2p2nqsd0c76g0!App")
        );

        let beta = std::path::Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.CodexBeta_26.1.2.3_x64__publisher\app\Codex.exe",
        );
        assert_eq!(
            packaged_app_user_model_id_from_path(beta).as_deref(),
            Some("OpenAI.CodexBeta_publisher!App")
        );
    }

    #[test]
    fn parses_windows_packaged_codex_app_json_shapes() {
        let single = parse_windows_packaged_codex_apps_json(
            r#"{"appUserModelId":"OpenAI.Codex_2p2nqsd0c76g0!App","packageFullName":"OpenAI.Codex_26.616.6631.0_x64__2p2nqsd0c76g0"}"#,
        );
        assert_eq!(single.len(), 1);
        assert_eq!(
            single[0].app_user_model_id,
            "OpenAI.Codex_2p2nqsd0c76g0!App"
        );
        assert_eq!(
            single[0].package_full_name.as_deref(),
            Some("OpenAI.Codex_26.616.6631.0_x64__2p2nqsd0c76g0")
        );

        let many = parse_windows_packaged_codex_apps_json(
            r#"[{"appUserModelId":"OpenAI.CodexBeta_publisher!App"},{"appUserModelId":"Other.App_abc!App"}]"#,
        );
        assert_eq!(many.len(), 1);
        assert_eq!(many[0].app_user_model_id, "OpenAI.CodexBeta_publisher!App");
        assert!(parse_windows_packaged_codex_apps_json("").is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn quotes_packaged_activation_arguments() {
        assert_eq!(
            windows_command_line_arguments(&codex_launch_arguments(9222)),
            "--remote-debugging-port=9222 --remote-allow-origins=http://127.0.0.1:9222"
        );
        assert_eq!(
            windows_quote_command_line_argument(r#"C:\Path With Spaces\Codex"#),
            r#""C:\Path With Spaces\Codex""#
        );
    }
}
