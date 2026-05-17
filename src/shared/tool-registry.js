const { listToolManifests } = require("../tools");

const registeredTools = new Map();
const discoveredToolIds = new Set();

registerBuiltInTools();

function registerTool(manifest) {
  const normalized = normalizeToolManifest(manifest);
  if (!normalized) return null;
  registeredTools.set(normalized.id, normalized);
  return cloneTool(normalized);
}

function unregisterTool(id) {
  const normalizedId = normalizeId(id);
  discoveredToolIds.delete(normalizedId);
  return registeredTools.delete(normalizedId);
}

function listToolRegistry(options = {}) {
  refreshDiscoveredTools(options);
  return Array.from(registeredTools.values()).map(cloneTool);
}

function listPublicTools(config = {}, options = {}) {
  refreshDiscoveredTools(options);
  return Array.from(registeredTools.values()).map((tool) => {
    const copy = cloneTool(tool);
    copy.config = copy.config.map((field) => Object.assign({}, field, {
      value: resolveFieldValue(field, config),
    }));
    return copy;
  });
}

function listToolConfigKeys(options = {}) {
  refreshDiscoveredTools(options);
  const keys = new Set();
  for (const tool of registeredTools.values()) {
    for (const field of tool.config || []) {
      if (field && field.key) keys.add(field.key);
    }
  }
  return Array.from(keys);
}

function toolDefaultConfig(options = {}) {
  refreshDiscoveredTools(options);
  const defaults = {};
  for (const tool of registeredTools.values()) {
    for (const field of tool.config || []) {
      if (!field.key) continue;
      defaults[field.key] = field.defaultValue !== undefined ? String(field.defaultValue) : "";
    }
  }
  return defaults;
}

function sanitizeToolConfig(body = {}, options = {}) {
  const allowed = new Set(listToolConfigKeys(options));
  const output = {};
  for (const [key, value] of Object.entries(body || {})) {
    if (!allowed.has(key)) continue;
    output[key] = normalizeConfigValue(key, value);
  }
  return output;
}

function getRegisteredTool(id) {
  refreshDiscoveredTools();
  const tool = registeredTools.get(normalizeId(id));
  return tool ? cloneTool(tool) : null;
}

function normalizeToolManifest(manifest) {
  if (!manifest || typeof manifest !== "object") return null;
  const id = normalizeId(manifest.id);
  if (!id) return null;
  const source = normalizeToolSource(manifest.source);
  return {
    id,
    version: String(manifest.version || "1"),
    kind: String(manifest.kind || "tool"),
    source,
    enabled: manifest.enabled !== false,
    icon: manifest.icon ? String(manifest.icon) : id.slice(0, 2).toUpperCase(),
    iconPath: manifest.iconPath ? normalizeAssetPath(manifest.iconPath) : "",
    name: String(manifest.name || id),
    description: String(manifest.description || ""),
    labels: normalizeToolLabels(manifest.labels, source),
    config: normalizeConfigFields(manifest.config),
    metadata: clonePlainObject(manifest.metadata || {}),
  };
}

function normalizeToolSource(value) {
  const source = String(value || "community").trim().toLowerCase().replace(/[^a-z0-9_-]/g, "_");
  return source || "community";
}

function normalizeToolLabels(labels, source) {
  const output = [];
  const seen = new Set();
  const add = (label) => {
    const normalized = normalizeToolLabel(label);
    if (!normalized || seen.has(normalized.id)) return;
    seen.add(normalized.id);
    output.push(normalized);
  };

  if (source === "built-in") add({ id: "built_in", labelKey: "toolLabelBuiltIn", label: "Built-in" });
  for (const label of Array.isArray(labels) ? labels : []) add(label);
  return output;
}

function normalizeToolLabel(label) {
  const raw = label && typeof label === "object" ? label : { id: label, label };
  const id = normalizeId(raw.id || raw.key || raw.label);
  if (!id) return null;
  return {
    id,
    labelKey: raw.labelKey ? String(raw.labelKey) : "",
    label: raw.label ? String(raw.label) : id,
  };
}

function normalizeConfigFields(fields) {
  const output = [];
  const seen = new Set();
  for (const field of Array.isArray(fields) ? fields : []) {
    if (!field || typeof field !== "object") continue;
    const key = normalizeConfigKey(field.key);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    const type = normalizeFieldType(field.type);
    const normalized = {
      key,
      type,
      labelKey: field.labelKey ? String(field.labelKey) : "",
      label: field.label ? String(field.label) : key,
      descriptionKey: field.descriptionKey ? String(field.descriptionKey) : "",
      description: field.description ? String(field.description) : "",
      defaultValue: field.defaultValue !== undefined ? String(field.defaultValue) : defaultValueForType(type),
      options: normalizeOptions(field.options),
      sensitive: Boolean(field.sensitive),
      required: Boolean(field.required),
      placeholderKey: field.placeholderKey ? String(field.placeholderKey) : "",
      placeholder: field.placeholder ? String(field.placeholder) : "",
    };
    output.push(normalized);
  }
  return output;
}

function normalizeOptions(options) {
  const output = [];
  const seen = new Set();
  for (const option of Array.isArray(options) ? options : []) {
    const value = typeof option === "object" && option ? option.value : option;
    const normalizedValue = String(value || "");
    if (!normalizedValue || seen.has(normalizedValue)) continue;
    seen.add(normalizedValue);
    output.push({
      value: normalizedValue,
      labelKey: option && typeof option === "object" && option.labelKey ? String(option.labelKey) : "",
      label: option && typeof option === "object" && option.label ? String(option.label) : normalizedValue,
    });
  }
  return output;
}

function normalizeConfigValue(key, value) {
  const field = findConfigField(key);
  if (!field) return String(value);
  if (field.type === "boolean") return isTruthy(value) ? "true" : "false";
  const text = String(value);
  if ((field.type === "segmented" || field.type === "select") && field.options.length > 0) {
    return field.options.some((option) => option.value === text) ? text : field.defaultValue;
  }
  if (field.type === "number") {
    const parsed = Number(text);
    return Number.isFinite(parsed) ? String(parsed) : field.defaultValue;
  }
  return text;
}

function findConfigField(key) {
  for (const tool of registeredTools.values()) {
    for (const field of tool.config || []) {
      if (field.key === key) return field;
    }
  }
  return null;
}

function resolveFieldValue(field, config) {
  return config[field.key] !== undefined ? normalizeConfigValue(field.key, config[field.key]) : field.defaultValue;
}

function normalizeId(value) {
  return String(value || "").trim().toLowerCase().replace(/[^a-z0-9_-]/g, "_").slice(0, 64);
}

function normalizeConfigKey(value) {
  return String(value || "").trim().replace(/[^A-Z0-9_]/gi, "_").toUpperCase().slice(0, 96);
}

function normalizeFieldType(value) {
  const type = String(value || "text").trim().toLowerCase();
  if (["segmented", "select", "boolean", "number", "password", "textarea"].includes(type)) return type;
  return "text";
}

function normalizeAssetPath(value) {
  const text = String(value || "").trim().replace(/\\/g, "/");
  if (!text) return "";
  if (/^https?:\/\//i.test(text) || text.startsWith("data:")) return "";
  return text.startsWith("/") ? text : "/" + text.replace(/^\/+/, "");
}

function defaultValueForType(type) {
  if (type === "boolean") return "false";
  if (type === "number") return "0";
  return "";
}

function isTruthy(value) {
  return /^(1|true|yes|on|enabled)$/i.test(String(value || "").trim());
}

function cloneTool(tool) {
  return {
    id: tool.id,
    version: tool.version,
    kind: tool.kind,
    source: tool.source,
    enabled: tool.enabled,
    icon: tool.icon,
    iconPath: tool.iconPath,
    name: tool.name,
    description: tool.description,
    labels: (tool.labels || []).map(cloneLabel),
    config: (tool.config || []).map(cloneField),
    metadata: clonePlainObject(tool.metadata || {}),
  };
}

function cloneLabel(label) {
  return {
    id: label.id,
    labelKey: label.labelKey,
    label: label.label,
  };
}

function cloneField(field) {
  return {
    key: field.key,
    type: field.type,
    labelKey: field.labelKey,
    label: field.label,
    descriptionKey: field.descriptionKey,
    description: field.description,
    defaultValue: field.defaultValue,
    value: field.value,
    options: (field.options || []).map((option) => ({
      value: option.value,
      labelKey: option.labelKey,
      label: option.label,
    })),
    sensitive: field.sensitive,
    required: field.required,
    placeholderKey: field.placeholderKey,
    placeholder: field.placeholder,
  };
}

function clonePlainObject(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return JSON.parse(JSON.stringify(value));
}

function registerBuiltInTools() {
  refreshDiscoveredTools({ includeCommunity: false });
}

function refreshDiscoveredTools(options = {}) {
  const manifests = listToolManifests(options);
  for (const manifest of manifests) {
    const normalized = normalizeToolManifest(manifest);
    if (!normalized) continue;
    const existing = registeredTools.get(normalized.id);
    if (existing && !discoveredToolIds.has(normalized.id)) continue;
    registeredTools.set(normalized.id, normalized);
    discoveredToolIds.add(normalized.id);
  }
}

module.exports = {
  getRegisteredTool,
  listPublicTools,
  listToolConfigKeys,
  listToolRegistry,
  registerTool,
  sanitizeToolConfig,
  toolDefaultConfig,
  unregisterTool,
};
