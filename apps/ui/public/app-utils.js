function formatCost(value) {
  const amount = Number(value) || 0;
  return "CNY " + amount.toFixed(4);
}

function formatDecimal(value) {
  const amount = Number(value) || 0;
  return amount.toFixed(Math.abs(amount) < 0.01 && amount !== 0 ? 6 : 2);
}

function formatNumber(value) {
  try {
    return new Intl.NumberFormat(formatLocaleId(uiLanguage)).format(Number(value) || 0);
  } catch {
    return String(Number(value) || 0);
  }
}

function formatTimeOnly(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "--:--:--" : date.toTimeString().split(" ")[0];
}

function formatDuration(value) {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms <= 0) return "-";
  return ms < 1000 ? Math.round(ms) + " ms" : (ms / 1000).toFixed(ms < 10000 ? 1 : 0) + " s";
}

function average(values) {
  return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : 0;
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatLocaleId(value) {
  return normalizeLanguageId(value).replace(/_/g, "-");
}

function stableStringify(value) {
  try {
    return JSON.stringify(value || null);
  } catch {
    return String(value || "");
  }
}

function noteSlow(label, durationMs) {
  if (!isDevDiagnosticsEnabled()) return;
  if (!Number.isFinite(durationMs) || durationMs < SLOW_RENDER_MS) return;
  console.debug("[CodeSeeX perf] " + label + " took " + Math.round(durationMs) + "ms");
}

function isDevDiagnosticsEnabled() {
  try {
    return location.search.includes("debug=1") || localStorage.getItem("codeseex_perf_debug") === "1";
  } catch {
    return false;
  }
}
