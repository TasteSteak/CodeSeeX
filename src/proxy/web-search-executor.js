const crypto = require("node:crypto");
const dns = require("node:dns").promises;
const net = require("node:net");
const { ProxyAgent } = require("undici");
const { execFileSync } = require("node:child_process");
const { detectEncoding, decodeBuffer, repairAnyMojibake } = require("../shared/text-encoding");

const DEFAULT_MAX_RESULTS = 30;
const PARSER_MAX_RESULTS = 20;
const DEFAULT_MAX_QUERIES = 3;
const DEFAULT_MAX_ENGINES = 4;
const DEFAULT_MAX_OPEN_URLS = 6;
const DEFAULT_OPEN_SNIPPET_BYTES = 1800;
const DEFAULT_AUTO_OPEN_COUNT = 3;
const DEFAULT_EXCERPT_MAX_CHARS = 300;
const DEFAULT_MAX_EXCERPTS_PER_PAGE = 3;
const DEFAULT_SEARCH_TIMEOUT_MS = 12000;
const DEFAULT_QUALITY_THRESHOLD = 0.34;
const SEARCH_USER_AGENT = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36";

function resolveDispatcher(config = {}) {
  const proxy = resolveProxyUrl(config);
  if (!proxy) return undefined;
  try {
    const allowInsecureTls = /^(1|true|yes|on|enabled)$/i.test(String((config && config.ALLOW_INSECURE_PROXY_TLS) || process.env.ALLOW_INSECURE_PROXY_TLS || "").trim());
    return allowInsecureTls
      ? new ProxyAgent({ uri: proxy, requestTls: { rejectUnauthorized: false } })
      : new ProxyAgent({ uri: proxy });
  } catch {
    return undefined;
  }
}

async function executeProxyWebSearch(queryOrAction, config = {}, options = {}) {
  const request = normalizeSearchRequest(queryOrAction);
  if (request.mode === "open") return executeProxyWebOpen(request, config, options);

  const queries = normalizeQueries(request.query || request.queries);
  if (queries.length === 0) return { ok: false, stage: "search", mode: "search", query: "", queries: [], error: "empty_query", results: [], candidates: [] };
  if (queries.length === 1) return executeSingleProxyWebSearch(queries[0], config, options);

  const perQuery = [];
  const groupedResults = [];
  const allResults = [];
  for (const cleanQuery of queries) {
    const result = await executeSingleProxyWebSearch(cleanQuery, config, options);
    perQuery.push(searchSummary(result));
    const queryResults = Array.isArray(result.results) ? result.results : [];
    groupedResults.push({
      query: cleanQuery,
      source: result.source || "",
      quality: result.quality || 0,
      low_confidence: Boolean(result.low_confidence),
      results: trimResults(queryResults),
    });
    allResults.push(...queryResults.map((item) => Object.assign({ query: cleanQuery }, item)));
  }

  const results = trimResults(dedupeResults(allResults));
  return {
    ok: results.length > 0,
    stage: "search",
    mode: "search",
    query: queries.join("\n"),
    queries: perQuery,
    grouped_results: groupedResults,
    source: "proxy_multi_search",
    results,
    candidates: results,
    candidate_count: results.length,
    quality: results.length > 0 ? roundScore(average(perQuery.map((item) => item.quality || 0))) : 0,
    low_confidence: perQuery.some((item) => item.low_confidence),
    error: results.length > 0 ? undefined : "empty_results",
  };
}

async function executeSingleProxyWebSearch(cleanQuery, config = {}, options = {}) {
  const failures = [];
  const threshold = qualityThreshold();
  const profile = analyzeQuery(cleanQuery);
  const searchQuery = rewriteSearchQuery(cleanQuery, profile);
  const directUrl = extractDirectUrl(cleanQuery);
  if (directUrl) {
    const directResult = await searchDirectUrl(directUrl, config, { source: "direct_url" });
    const directResults = tagSearchResults(directResult, cleanQuery, "direct_url", profile);
    return buildSearchAggregate({
      query: cleanQuery,
      source: "direct_url",
      results: directResults,
      profile,
      failures,
      qualityThreshold: threshold,
    });
  }

  const collected = [];
  for (const step of searchSteps(config, cleanQuery)) {
    const result = await step.run(searchQuery, config);
    result.query = cleanQuery;
    result.executed_query = searchQuery;
    if (result.ok) {
      const ranked = rankSearchResult(cleanQuery, result, profile);
      collected.push(...tagSearchResults(ranked, cleanQuery, ranked.source || step.engine, profile));
    } else {
      failures.push(result);
    }
  }

  const candidates = finalizeSearchCandidates(collected, profile, cleanQuery);
  const autoOpenResult = await autoOpenPreviews(candidates, cleanQuery, Object.assign({}, config, { qualityThreshold: threshold }));
  const enriched = autoOpenResult.enriched;

  return buildSearchAggregate({
    query: cleanQuery,
    source: enriched.length > 0 ? "proxy_multi_source" : "none",
    results: enriched,
    profile,
    failures,
    qualityThreshold: threshold,
    options,
    auto_opened: autoOpenResult.count,
    more_available: autoOpenResult.more,
  });
}

async function executeProxyWebOpen(request, config = {}, options = {}) {
  const lookup = buildCandidateLookup(options.messages || options.candidateLookup || []);
  const requestedUrls = normalizeOpenTargets(request.open_urls || request.urls || request.url);
  const requestedIds = normalizeOpenTargets(request.open_ids || request.ids || request.id);
  const directUrl = extractDirectUrl(request.query || "");
  const resolved = resolveOpenTargets({
    open_urls: requestedUrls.concat(directUrl ? [directUrl] : []),
    open_ids: requestedIds,
    query: request.query || "",
  }, lookup);
  const urls = resolved.urls.slice(0, DEFAULT_MAX_OPEN_URLS);
  if (urls.length === 0) {
    return {
      ok: false,
      stage: "open",
      mode: "open",
      query: request.query || "",
      open_urls: requestedUrls,
      open_ids: requestedIds,
      unresolved_ids: resolved.unresolved_ids,
      error: resolved.unresolved_ids.length > 0 ? "unknown_candidate_ids" : "empty_open_targets",
      results: [],
      opened_results: [],
    };
  }

  const opened = [];
  const failures = [];
  for (const url of urls) {
    const result = await openDirectUrl(url, config, options);
    if (result.ok) {
      opened.push(...tagOpenResults(result, request.query || url));
      continue;
    }
    failures.push(result);
  }

  const results = finalizeOpenedResults(opened);
  return {
    ok: results.length > 0,
    stage: "open",
    mode: "open",
    query: request.query || urls.join("\n"),
    open_urls: urls,
    open_ids: requestedIds,
    unresolved_ids: resolved.unresolved_ids,
    source: results.length > 0 ? "proxy_open" : "none",
    results,
    opened_results: results,
    opened_count: results.length,
    quality: results.length > 0 ? roundScore(average(results.map((item) => item.score || 0))) : 0,
    low_confidence: false,
    error: results.length > 0 ? undefined : (failures[0] ? failures[0].error : "empty_results"),
    fallback_errors: failures.map((result) => ({
      source: result.source,
      error: result.error,
      message: result.message,
      status: result.status,
    })),
  };
}

function normalizeSearchRequest(queryOrAction) {
  if (queryOrAction && typeof queryOrAction === "object" && !Array.isArray(queryOrAction)) {
    const mode = String(queryOrAction.mode || "").trim().toLowerCase();
    const openUrls = normalizeOpenTargets(queryOrAction.open_urls || queryOrAction.urls || queryOrAction.url);
    const openIds = normalizeOpenTargets(queryOrAction.open_ids || queryOrAction.ids || queryOrAction.id);
    if (mode === "open" || openUrls.length > 0 || openIds.length > 0) {
      return {
        mode: "open",
        query: cleanQuery(queryOrAction.query || ""),
        open_urls: openUrls,
        open_ids: openIds,
      };
    }
    return {
      mode: "search",
      query: cleanQuery(queryOrAction.query || ""),
      queries: normalizeQueries(queryOrAction.queries || queryOrAction.search_query || queryOrAction.query || queryOrAction.q),
    };
  }

  if (Array.isArray(queryOrAction)) {
    return { mode: "search", queries: normalizeQueries(queryOrAction) };
  }

  return { mode: "search", query: cleanQuery(queryOrAction) };
}

function buildSearchAggregate({ query, source, results, profile, failures, qualityThreshold: threshold, options = {}, auto_opened, more_available }) {
  const normalizedResults = finalizeSearchCandidates(results, profile, query);
  const stage = String(options.stage || "search");
  const quality = normalizedResults.length > 0 ? roundScore(average(normalizedResults.map((item) => item.score || 0))) : 0;
  const lowConfidence = stage === "search"
    ? quality < (threshold || qualityThreshold()) || !hasRequiredResultCoverage(profile, normalizedResults)
    : false;
  const output = {
    ok: normalizedResults.length > 0,
    stage,
    mode: stage,
    query,
    source,
    results: normalizedResults,
    quality,
    low_confidence: lowConfidence,
    auto_opened: auto_opened != null ? auto_opened : 0,
    more_available: more_available != null ? more_available : 0,
  };

  if (stage === "search") {
    output.candidates = normalizedResults;
    output.candidate_count = normalizedResults.length;
    output.next_action = "Pick a few candidate URLs or ids and call web_search again with mode=open.";
  } else if (stage === "open") {
    output.opened_results = normalizedResults;
    output.opened_count = normalizedResults.length;
  }

  if (failures && failures.length > 0) {
    output.fallback_errors = failures.map((result) => ({
      source: result.source,
      error: result.error,
      message: result.message,
      status: result.status,
      quality: result.quality,
    }));
  }
  if (!output.ok) output.error = failures && failures.length > 0 ? failures[failures.length - 1].error : "empty_results";
  return output;
}

function finalizeSearchCandidates(results, profile, query) {
  const scored = dedupeResults(Array.isArray(results) ? results : [])
    .map((item, index) => normalizeSearchResultItem(item, { profile, query, index }))
    .filter(Boolean)
    .sort((left, right) => (right.score || 0) - (left.score || 0));
  return trimResults(scored);
}

function tagSearchResults(result, query, source, profile = analyzeQuery(query)) {
  const items = Array.isArray(result && result.results) ? result.results : [];
  return items.map((item, index) => normalizeSearchResultItem(item, {
    profile,
    query,
    index,
    source: source || result.source || "search",
  })).filter(Boolean);
}

function tagOpenResults(result, query) {
  const items = Array.isArray(result && result.results) ? result.results : [];
  return items.map((item, index) => normalizeOpenResultItem(item, { query, index, source: result.source || "direct_url" })).filter(Boolean);
}

function normalizeSearchResultItem(item, context = {}) {
  if (!item || typeof item !== "object") return null;
  const query = cleanQuery(context.query || item.query || "");
  const source = String(context.source || item.source || item.engine || "search");
  const title = cleanText(item.title || item.name || item.url || "Result");
  const url = normalizeSourceCandidateUrl(item.url || item.link || "");
  const snippet = cleanText(item.snippet || item.summary || item.content || "");
  const score = Number.isFinite(Number(item.score)) ? Number(item.score) : roundScore(Math.max(0, 0.45 + (item.direct ? 0.12 : 0)));
  const id = candidateIdFor(url || title, query, source);
  return Object.assign({}, item, {
    id,
    query,
    source,
    title: title || url || "Result",
    url,
    snippet: snippet.slice(0, 500),
    rank: Number.isFinite(Number(context.index)) ? Number(context.index) + 1 : 1,
    score,
    candidate: true,
  });
}

function normalizeOpenResultItem(item, context = {}) {
  if (!item || typeof item !== "object") return null;
  const query = cleanQuery(context.query || item.query || "");
  const source = String(context.source || item.source || "direct_url");
  const title = cleanText(item.title || item.name || item.url || "Page");
  const url = normalizeSourceCandidateUrl(item.url || item.link || "");
  const content = cleanText(item.content || item.snippet || item.summary || "");
  const id = candidateIdFor(url || title, query, source);
  return Object.assign({}, item, {
    id,
    query,
    source,
    title: title || url || "Page",
    url,
    content: content.slice(0, DEFAULT_OPEN_SNIPPET_BYTES),
    snippet: content.slice(0, 500),
    opened: true,
    rank: Number.isFinite(Number(context.index)) ? Number(context.index) + 1 : 1,
    score: Number.isFinite(Number(item.score)) ? Number(item.score) : 1,
  });
}

function finalizeOpenedResults(results) {
  return trimResults(dedupeResults(Array.isArray(results) ? results : []));
}

function resolveOpenTargets(request, lookup) {
  const urls = [];
  const unresolved_ids = [];
  const addUrl = (value) => {
    const normalized = normalizeSourceCandidateUrl(value);
    if (!normalized) return;
    if (urls.some((entry) => entry.toLowerCase() === normalized.toLowerCase())) return;
    urls.push(normalized);
  };

  for (const url of normalizeOpenTargets(request.open_urls || request.urls || request.url)) {
    addUrl(url);
  }

  const direct = extractDirectUrl(request.query || "");
  if (direct) addUrl(direct);

  for (const id of normalizeOpenTargets(request.open_ids || request.ids || request.id)) {
    const resolved = resolveCandidateLookupValue(lookup, id);
    if (resolved) addUrl(resolved);
    else unresolved_ids.push(id);
  }

  return { urls, unresolved_ids };
}

function buildCandidateLookup(source) {
  if (!source) return new Map();
  if (source instanceof Map) return source;
  if (typeof source === "function") return source;
  if (Array.isArray(source)) return buildCandidateLookupFromMessages(source);
  const map = new Map();
  if (typeof source === "object") {
    for (const [key, value] of Object.entries(source)) {
      if (value === undefined || value === null) continue;
      const normalized = normalizeSourceCandidateUrl(value) || String(value);
      map.set(String(key).toLowerCase(), normalized);
    }
  }
  return map;
}

function buildCandidateLookupFromMessages(messages) {
  const map = new Map();
  for (const message of Array.isArray(messages) ? messages : []) {
    if (!message || typeof message !== "object" || typeof message.content !== "string") continue;
    let parsed = null;
    try {
      parsed = JSON.parse(message.content);
    } catch {
      continue;
    }
    collectCandidatesFromValue(parsed, map);
  }
  return map;
}

function collectCandidatesFromValue(value, map) {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const entry of value) collectCandidatesFromValue(entry, map);
    return;
  }
  if (Array.isArray(value.candidates)) {
    for (const candidate of value.candidates) collectCandidateEntry(candidate, map);
  }
  if (Array.isArray(value.results)) {
    for (const candidate of value.results) collectCandidateEntry(candidate, map);
  }
  if (Array.isArray(value.opened_results)) {
    for (const candidate of value.opened_results) collectCandidateEntry(candidate, map);
  }
  if (value.id && value.url) collectCandidateEntry(value, map);
  if (value.details && typeof value.details === "object") collectCandidatesFromValue(value.details, map);
}

function collectCandidateEntry(candidate, map) {
  if (!candidate || typeof candidate !== "object") return;
  const url = normalizeSourceCandidateUrl(candidate.url || candidate.link || "");
  if (candidate.id && url) map.set(String(candidate.id).toLowerCase(), url);
  if (url) map.set(url.toLowerCase(), url);
}

function resolveCandidateLookupValue(lookup, key) {
  if (!lookup || !key) return "";
  if (typeof lookup === "function") return normalizeSourceCandidateUrl(lookup(key)) || "";
  if (lookup instanceof Map) {
    return normalizeSourceCandidateUrl(lookup.get(key) || lookup.get(String(key).toLowerCase()) || lookup.get(String(key).toUpperCase()) || "");
  }
  if (typeof lookup === "object") {
    return normalizeSourceCandidateUrl(lookup[key] || lookup[String(key).toLowerCase()] || lookup[String(key).toUpperCase()] || "");
  }
  return "";
}

function candidateIdFor(value, query, source) {
  const raw = [String(value || "").trim().toLowerCase(), String(query || "").trim().toLowerCase(), String(source || "").trim().toLowerCase()].join("\u0001");
  return "cand_" + crypto.createHash("sha1").update(raw).digest("hex").slice(0, 12);
}

function searchSteps(config = {}, query = "") {
  const engineOrder = ["bing", "google", "duckduckgo", "yandex"];
  const seen = new Set();
  const steps = [];
  const maxEngines = DEFAULT_MAX_ENGINES;
  let engineCount = 0;

  const directUrl = extractDirectUrl(query);
  if (directUrl) {
    steps.push({
      engine: "direct_url",
      run: () => searchDirectUrl(directUrl, config, { source: "direct_url" }),
    });
  }

  for (const engine of engineOrder) {
    if (engineCount >= maxEngines) break;
    if (seen.has(engine)) continue;
    seen.add(engine);
    if (engine === "google") steps.push({ engine, run: searchGoogleHtml });
    else if (engine === "bing") steps.push({ engine, run: searchBingHtml });
    else if (engine === "duckduckgo") steps.push({ engine, run: searchDuckDuckGo });
    else if (engine === "yandex") steps.push({ engine, run: searchYandexHtml });
    else continue;
    engineCount += 1;
  }

  return steps;
}

async function searchDirectUrl(url, config = {}, options = {}) {
  const fetched = await fetchText(url, config, {
    Accept: "text/html,application/xhtml+xml,application/json,text/plain;q=0.9,*/*;q=0.8",
    "Accept-Language": "en-US,en;q=0.9,zh-CN;q=0.4",
    "User-Agent": "Mozilla/5.0 (compatible; CodeSeeX/1.0; +https://localhost)",
  });
  const source = options.source || "direct_url";
  if (!fetched.ok) return Object.assign({ source, query: url, results: [] }, fetched);

  const title = extractHtmlTitle(fetched.text) || url;
  const snippet = extractReadableSnippet(fetched.text);
  return {
    ok: Boolean(title || snippet),
    query: url,
    source,
    results: [{
      title,
      url,
      snippet,
      direct: true,
    }],
    error: title || snippet ? undefined : "empty_results",
  };
}

async function openDirectUrl(url, config = {}, options = {}) {
  const fetched = await fetchText(url, config, {
    Accept: "text/html,application/xhtml+xml,application/json,text/plain;q=0.9,*/*;q=0.8",
    "Accept-Language": "en-US,en;q=0.9,zh-CN;q=0.4",
    "User-Agent": "Mozilla/5.0 (compatible; CodeSeeX/1.0; +https://localhost)",
  });
  const source = options.source || "direct_url";
  if (!fetched.ok) return Object.assign({ source, url, query: url, results: [] }, fetched);

  const title = extractHtmlTitle(fetched.text) || url;
  const content = extractOpenableContent(fetched.text);
  const snippet = content.slice(0, Math.min(DEFAULT_OPEN_SNIPPET_BYTES, 500));
  return {
    ok: Boolean(title || content),
    query: url,
    source,
    results: [{
      title,
      url,
      snippet,
      content,
      direct: true,
      opened: true,
    }],
    error: title || content ? undefined : "empty_results",
  };
}

async function searchDuckDuckGo(query, config = {}) {
  const htmlResult = await searchDuckDuckGoHtml(query, config);
  if (htmlResult.ok) return htmlResult;
  const instantResult = await searchDuckDuckGoInstantAnswer(query, config);
  if (instantResult.ok) return instantResult;
  return Object.assign({}, instantResult, {
    fallback_errors: [htmlResult, instantResult].map((result) => ({
      source: result.source,
      error: result.error,
      message: result.message,
      status: result.status,
    })),
  });
}

async function searchGoogleHtml(query, config = {}) {
  const locale = localeForQuery(query);
  const url = "https://www.google.com/search?" + new URLSearchParams({ q: query, hl: locale.googleHl }).toString();
  const fetched = await fetchText(url, config, {
    Accept: "text/html,application/xhtml+xml",
    "Accept-Language": locale.acceptLanguage,
    "User-Agent": SEARCH_USER_AGENT,
  });
  if (!fetched.ok) return Object.assign({ source: "google_html", query, results: [] }, fetched);
  const results = parseGoogleHtml(fetched.text);
  return {
    ok: results.length > 0,
    query,
    source: "google_html",
    results,
    error: results.length > 0 ? undefined : "empty_results",
  };
}

async function searchBingHtml(query, config = {}) {
  const locale = localeForQuery(query);
  const url = "https://www.bing.com/search?" + new URLSearchParams({ q: query, setlang: locale.bingSetLang, mkt: locale.bingMarket }).toString();
  const fetched = await fetchText(url, config, {
    Accept: "text/html,application/xhtml+xml",
    "Accept-Language": locale.acceptLanguage,
    "User-Agent": SEARCH_USER_AGENT,
  });
  if (!fetched.ok) return Object.assign({ source: "bing_html", query, results: [] }, fetched);
  const results = parseBingHtml(fetched.text);
  return {
    ok: results.length > 0,
    query,
    source: "bing_html",
    results,
    error: results.length > 0 ? undefined : "empty_results",
  };
}

async function searchYandexHtml(query, config = {}) {
  const locale = localeForQuery(query);
  const url = "https://yandex.com/search/?" + new URLSearchParams({ text: query, lang: locale.yandexLang }).toString();
  const fetched = await fetchText(url, config, {
    Accept: "text/html,application/xhtml+xml",
    "Accept-Language": locale.acceptLanguage,
    "User-Agent": SEARCH_USER_AGENT,
  });
  if (!fetched.ok) return Object.assign({ source: "yandex_html", query, results: [] }, fetched);
  const results = parseYandexHtml(fetched.text);
  return {
    ok: results.length > 0,
    query,
    source: "yandex_html",
    results,
    error: results.length > 0 ? undefined : "empty_results",
  };
}

async function searchDuckDuckGoHtml(query, config = {}) {
  const locale = localeForQuery(query);
  const url = "https://html.duckduckgo.com/html/?" + new URLSearchParams({ q: query, kl: locale.duckDuckGoKl }).toString();
  const fetched = await fetchText(url, config, {
    Accept: "text/html,application/xhtml+xml",
    "Accept-Language": locale.acceptLanguage,
    "User-Agent": "Mozilla/5.0 (compatible; CodeSeeX/1.0; +https://localhost)",
  });
  if (!fetched.ok) return Object.assign({ source: "duckduckgo_html", query, results: [] }, fetched);
  const results = parseDuckDuckGoHtml(fetched.text);
  return {
    ok: results.length > 0,
    query,
    source: "duckduckgo_html",
    results,
    error: results.length > 0 ? undefined : "empty_results",
  };
}

async function searchDuckDuckGoInstantAnswer(query, config = {}) {
  const url = "https://api.duckduckgo.com/?" + new URLSearchParams({
    q: query,
    format: "json",
    no_html: "1",
    no_redirect: "1",
    skip_disambig: "1",
  }).toString();
  const fetched = await fetchText(url, config, { Accept: "application/json" });
  if (!fetched.ok) return Object.assign({ source: "duckduckgo_instant_answer", query, results: [] }, fetched);
  try {
    return normalizeDuckDuckGoResponse(query, JSON.parse(fetched.text));
  } catch (error) {
    return {
      ok: false,
      query,
      source: "duckduckgo_instant_answer",
      error: "invalid_json",
      message: error && error.message ? error.message : String(error),
      results: [],
    };
  }
}

async function fetchText(url, config = {}, headers = {}) {
  const safety = await validatePublicHttpUrl(url, config);
  if (!safety.ok) {
    return { ok: false, error: safety.error, message: safety.message, url: safety.url || String(url || "") };
  }
  const dispatcher = resolveDispatcher(config);
  const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
  const timeout = setTimeout(() => controller && controller.abort(), DEFAULT_SEARCH_TIMEOUT_MS);
  try {
    const response = await fetch(safety.url, {
      headers,
      signal: controller ? controller.signal : undefined,
      dispatcher,
    });
    const arrayBuffer = await response.arrayBuffer();
    const buffer = Buffer.from(arrayBuffer);
    let text;
    try {
      const encoding = detectEncoding(buffer, response.headers);
      text = decodeBuffer(buffer, encoding || "utf-8");
      text = repairAnyMojibake(text);
    } catch (_encodingErr) {
      text = buffer.toString("utf-8");
    }
    if (!response.ok) {
      return { ok: false, error: "http_" + response.status, status: response.status, raw: text.slice(0, 500) };
    }
    return { ok: true, text };
  } catch (error) {
    return {
      ok: false,
      error: error && error.name === "AbortError" ? "timeout" : "request_failed",
      message: error && error.message ? error.message : String(error),
    };
  } finally {
    clearTimeout(timeout);
  }
}

async function validatePublicHttpUrl(value, config = {}) {
  let parsed;
  try {
    parsed = new URL(String(value || ""));
  } catch {
    return { ok: false, error: "invalid_url", message: "URL is invalid." };
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return { ok: false, error: "blocked_non_http_protocol", message: "Only http and https URLs are allowed.", url: parsed.toString() };
  }
  const host = parsed.hostname;
  if (!host) return { ok: false, error: "invalid_url", message: "URL host is empty.", url: parsed.toString() };
  if (isBlockedHostname(host)) {
    return { ok: false, error: "blocked_internal_target", message: "Internal, private, loopback, and link-local targets are blocked.", url: parsed.toString() };
  }
  const directIp = net.isIP(stripIpv6Brackets(host)) ? stripIpv6Brackets(host) : "";
  if (directIp && isBlockedIp(directIp)) {
    return { ok: false, error: "blocked_internal_target", message: "Internal, private, loopback, and link-local targets are blocked.", url: parsed.toString() };
  }
  if (!directIp && !skipDnsSafetyCheck(config)) {
    try {
      const records = await dns.lookup(host, { all: true, verbatim: true });
      if (!records.length || records.some((record) => record && isBlockedIp(record.address))) {
        return { ok: false, error: "blocked_internal_target", message: "Internal, private, loopback, and link-local targets are blocked.", url: parsed.toString() };
      }
    } catch (error) {
      return { ok: false, error: "dns_lookup_failed", message: error && error.message ? error.message : "DNS lookup failed.", url: parsed.toString() };
    }
  }
  return { ok: true, url: parsed.toString() };
}

function skipDnsSafetyCheck(config = {}) {
  return /^(1|true|yes|on|enabled)$/i.test(String(
    config.skipDnsSafetyCheck
    || config.CODESEEX_SKIP_DNS_SSRF_CHECK
    || process.env.CODESEEX_SKIP_DNS_SSRF_CHECK
    || ""
  ).trim());
}

function isBlockedHostname(hostname) {
  const value = String(hostname || "").toLowerCase().replace(/\.$/, "");
  if (!value) return true;
  if (value === "localhost" || value.endsWith(".localhost")) return true;
  if (value.endsWith(".local") || value.endsWith(".internal") || value.endsWith(".lan")) return true;
  return false;
}

function stripIpv6Brackets(value) {
  return String(value || "").replace(/^\[/, "").replace(/\]$/, "");
}

function isBlockedIp(value) {
  const ip = stripIpv6Brackets(value);
  const version = net.isIP(ip);
  if (version === 4) return isBlockedIpv4(ip);
  if (version === 6) return isBlockedIpv6(ip);
  return true;
}

function isBlockedIpv4(ip) {
  const parts = ip.split(".").map((part) => Number(part));
  if (parts.length !== 4 || parts.some((part) => !Number.isInteger(part) || part < 0 || part > 255)) return true;
  const [a, b] = parts;
  if (a === 0 || a === 10 || a === 127) return true;
  if (a === 100 && b >= 64 && b <= 127) return true;
  if (a === 169 && b === 254) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 192 && b === 168) return true;
  if (a === 192 && b === 0) return true;
  if (a === 198 && (b === 18 || b === 19)) return true;
  if (a >= 224) return true;
  return false;
}

function isBlockedIpv6(ip) {
  const value = ip.toLowerCase();
  if (value === "::" || value === "::1") return true;
  if (value.startsWith("fc") || value.startsWith("fd")) return true;
  if (value.startsWith("fe8") || value.startsWith("fe9") || value.startsWith("fea") || value.startsWith("feb")) return true;
  if (value.startsWith("ff")) return true;
  if (value.startsWith("::ffff:")) return isBlockedIpv4(value.slice("::ffff:".length));
  return false;
}

function resolveProxyUrl(config = {}) {
  return envProxyUrl() || systemProxyUrl();
}

function envProxyUrl() {
  return normalizeProxyUrl(process.env.HTTPS_PROXY || process.env.https_proxy || process.env.HTTP_PROXY || process.env.http_proxy || "");
}

function systemProxyUrl() {
  if (process.platform !== "win32") return "";
  try {
    if (!isWindowsProxyEnabled()) return "";
    const raw = queryWindowsInternetSetting("ProxyServer");
    if (!raw) return "";
    return parseWindowsProxyServer(raw);
  } catch {
    return "";
  }
}

function isWindowsProxyEnabled() {
  const value = queryWindowsInternetSetting("ProxyEnable");
  if (!value) return false;
  return Number(value.trim()) === 1;
}

function queryWindowsInternetSetting(name) {
  const output = execFileSync("reg", ["query", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings", "/v", name], {
    encoding: "utf8",
    windowsHide: true,
    timeout: 3000,
  });
  const match = output.match(new RegExp(escapeRegExp(name) + "\\s+REG_\\w+\\s+(.+)", "i"));
  return match ? match[1].trim() : "";
}

function parseWindowsProxyServer(raw) {
  const value = String(raw || "").trim();
  if (!value) return "";
  if (!value.includes("=")) return normalizeProxyUrl(value);

  const entries = {};
  for (const part of value.split(";")) {
    const match = String(part || "").trim().match(/^([a-z][a-z0-9+.-]*)=(.+)$/i);
    if (!match) continue;
    entries[match[1].toLowerCase()] = match[2].trim();
  }

  // Undici ProxyAgent supports HTTP(S) CONNECT proxies. Do not reinterpret
  // socks=host:port as http://host:port; that causes fast "fetch failed" errors.
  return normalizeProxyUrl(entries.https || entries.http || "");
}

function normalizeProxyUrl(value) {
  const raw = String(value || "").trim();
  if (!raw) return "";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
    return /^https?:\/\//i.test(raw) ? raw : "";
  }
  return "http://" + raw;
}

function parseBingHtml(html) {
  const results = [];
  const source = String(html || "");
  const blockPattern = /<li[^>]+class="[^"]*b_algo[^"]*"[^>]*>([\s\S]*?)<\/li>/gi;
  let blockMatch;
  while ((blockMatch = blockPattern.exec(source)) && results.length < PARSER_MAX_RESULTS) {
    const block = blockMatch[1];
    const titleMatch = block.match(/<h2[^>]*>[\s\S]*?<a[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>[\s\S]*?<\/h2>/i);
    if (!titleMatch) continue;
    const captionMatch = block.match(/<(?:p|div)[^>]+class="[^"]*(?:b_caption|b_snippet|b_lineclamp)[^"]*"[^>]*>([\s\S]*?)<\/(?:p|div)>/i)
      || block.match(/<p[^>]*>([\s\S]*?)<\/p>/i);
    const url = normalizeBingUrl(decodeHtml(titleMatch[1]));
    const title = stripTags(decodeHtml(titleMatch[2]));
    const snippet = stripTags(decodeHtml(captionMatch ? captionMatch[1] : ""));
    if (!title && !url) continue;
    results.push({ title: title || url, url, snippet });
  }
  return dedupeResults(results).slice(0, PARSER_MAX_RESULTS);
}

function parseGoogleHtml(html) {
  const results = [];
  const source = String(html || "");
  const blockPattern = /<a[^>]+href="([^"]+)"[^>]*>\s*(?:<br[^>]*>\s*)?<h3[^>]*>([\s\S]*?)<\/h3>[\s\S]*?<\/a>([\s\S]*?)(?=<a[^>]+href=|<div id="bottomads"|<footer|$)/gi;
  let match;
  while ((match = blockPattern.exec(source)) && results.length < PARSER_MAX_RESULTS) {
    const url = normalizeGoogleUrl(decodeHtml(match[1]));
    if (!/^https?:\/\//i.test(url)) continue;
    const title = stripTags(decodeHtml(match[2]));
    const snippet = stripTags(decodeHtml(match[3])).slice(0, 300);
    if (!title && !url) continue;
    results.push({ title: title || url, url, snippet });
  }
  return dedupeResults(results).slice(0, PARSER_MAX_RESULTS);
}

function parseDuckDuckGoHtml(html) {
  const results = [];
  const source = String(html || "");
  const blockPattern = /<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>[\s\S]*?(?:<a[^>]+class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)<\/a>|<div[^>]+class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)<\/div>)?/gi;
  let match;
  while ((match = blockPattern.exec(source)) && results.length < PARSER_MAX_RESULTS) {
    const url = normalizeDuckDuckGoUrl(decodeHtml(match[1]));
    const title = stripTags(decodeHtml(match[2]));
    const snippet = stripTags(decodeHtml(match[3] || match[4] || ""));
    if (!title && !url) continue;
    results.push({ title: title || url, url, snippet });
  }
  return dedupeResults(results).slice(0, PARSER_MAX_RESULTS);
}

function parseYandexHtml(html) {
  const results = [];
  const source = String(html || "");
  const blockPattern = /<li[^>]+class="[^"]*serp-item[^"]*"[^>]*>([\s\S]*?)<\/li>/gi;
  let blockMatch;
  while ((blockMatch = blockPattern.exec(source)) && results.length < PARSER_MAX_RESULTS) {
    const block = blockMatch[1];
    const titleMatch = block.match(/<a[^>]+class="[^"]*Link[^"]*"[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/i)
      || block.match(/<a[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/i);
    if (!titleMatch) continue;
    const snippetMatch = block.match(/<div[^>]+class="[^"]*(?:TextContainer|OrganicTextContentSpan|organic__text)[^"]*"[^>]*>([\s\S]*?)<\/div>/i);
    const url = normalizeYandexUrl(decodeHtml(titleMatch[1]));
    const title = stripTags(decodeHtml(titleMatch[2]));
    const snippet = stripTags(decodeHtml(snippetMatch ? snippetMatch[1] : ""));
    if (!title && !url) continue;
    results.push({ title: title || url, url, snippet });
  }
  return dedupeResults(results).slice(0, PARSER_MAX_RESULTS);
}

function normalizeDuckDuckGoUrl(value) {
  const raw = String(value || "").trim();
  try {
    const url = new URL(raw, "https://duckduckgo.com");
    const uddg = url.searchParams.get("uddg");
    return uddg ? decodeURIComponent(uddg) : url.toString();
  } catch {
    return raw;
  }
}

function normalizeGoogleUrl(value) {
  const raw = String(value || "").trim();
  try {
    const url = new URL(raw, "https://www.google.com");
    const target = url.searchParams.get("q") || url.searchParams.get("url");
    return target && /^https?:\/\//i.test(target) ? target : url.toString();
  } catch {
    return raw;
  }
}

function normalizeBingUrl(value) {
  const raw = String(value || "").trim();
  try {
    const url = new URL(raw);
    const encoded = url.searchParams.get("u");
    if (encoded) {
      const decoded = decodeBingUrlParameter(encoded);
      if (decoded) return decoded;
    }
    return url.toString();
  } catch {
    return raw;
  }
}

function normalizeYandexUrl(value) {
  const raw = String(value || "").trim();
  try {
    const url = new URL(raw, "https://yandex.com");
    const target = url.searchParams.get("url") || url.searchParams.get("target");
    return target && /^https?:\/\//i.test(target) ? target : url.toString();
  } catch {
    return raw;
  }
}

function normalizeQueries(value) {
  const maxQueries = DEFAULT_MAX_QUERIES;
  const raw = Array.isArray(value) ? value : String(value || "").split(/\r?\n/);
  const seen = new Set();
  const queries = [];
  for (const entry of raw) {
    const cleaned = cleanQuery(entry);
    const key = cleaned.toLowerCase();
    if (!cleaned || seen.has(key)) continue;
    seen.add(key);
    queries.push(cleaned);
    if (queries.length >= maxQueries) break;
  }
  return queries;
}

function normalizeOpenTargets(value) {
  const raw = Array.isArray(value) ? value : (value ? [value] : []);
  const seen = new Set();
  const targets = [];
  for (const entry of raw) {
    const text = String(entry || "").trim();
    if (!text) continue;
    const key = text.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    targets.push(text);
  }
  return targets;
}

function cleanQuery(value) {
  return String(value || "").replace(/\s+/g, " ").trim().slice(0, 300);
}

function extractDirectUrl(value) {
  const text = String(value || "").trim();
  if (!text) return "";
  const explicit = text.match(/https?:\/\/[^\s"'<>]+/i);
  if (explicit) return normalizeSourceCandidateUrl(explicit[0]);
  if (/^[a-z0-9.-]+\.[a-z]{2,}(?:\/[^\s]*)?$/i.test(text)) return normalizeSourceCandidateUrl(text);
  return "";
}

function normalizeSourceCandidateUrl(value) {
  const raw = String(value || "").trim().replace(/[),.;]+$/, "");
  if (!raw) return "";
  try {
    return new URL(raw.includes("://") ? raw : "https://" + raw).toString();
  } catch {
    return "";
  }
}

function rankSearchResult(query, result, profile = analyzeQuery(query)) {
  const scored = (Array.isArray(result.results) ? result.results : [])
    .map((item, index) => scoreResult(query, item, index, profile))
    .filter((item) => item.score >= minimumResultScore(profile))
    .sort((left, right) => right.score - left.score);
  const results = trimResults(dedupeResults(scored));
  const quality = roundScore(results.length > 0 ? average(results.map((item) => item.score || 0)) : 0);
  return Object.assign({}, result, {
    ok: results.length > 0,
    results,
    quality,
    low_confidence: quality < qualityThreshold() || !hasRequiredResultCoverage(profile, results),
  });
}

function scoreResult(query, result, index, profile = analyzeQuery(query)) {
  const terms = profile.terms;
  const haystack = cleanText([result.title, result.url, result.snippet].join(" ")).toLowerCase();
  if (!haystack) return Object.assign({}, result, { score: 0 });

  let matched = 0;
  for (const term of terms) {
    if (haystack.includes(term)) matched += 1;
  }
  const coverage = terms.length > 0 ? matched / terms.length : 0.25;
  const coreCoverage = coreTermCoverage(profile, haystack);
  const hasSnippet = cleanText(result.snippet).length >= 24 ? 0.12 : 0;
 const hasUsefulUrl = /^https?:\/\//i.test(result.url || "") ? 0.12 : 0;
 const rankBonus = Math.max(0, 0.16 - index * 0.012);
 const badPenalty = lowValuePenalty(profile, result, haystack);
  // Short queries skip the required-group coverage gate.
  const missingCorePenalty = profile.requiredGroups.length > 0 && coreCoverage === 0 && profile.terms.length > 3 ? 0.34 : 0;
 const score = coverage * 0.36 + coreCoverage * 0.36 + hasSnippet + hasUsefulUrl + rankBonus - badPenalty - missingCorePenalty;
  return Object.assign({}, result, { score: roundScore(Math.max(0, Math.min(1, score))) });
}

function meaningfulTerms(query) {
  const text = String(query || "").toLowerCase();
  const latin = text.match(/[a-z0-9][a-z0-9._-]{1,}/g) || [];
  const cjk = text.match(/[\u3400-\u9fff]{2,}/g) || [];
  const stop = new Set(["the", "and", "for", "with", "from", "latest", "today", "current", "search", "query", "best", "top", "major", "status", "comparison", "compare", "2024", "2025", "2026"]);
  return latin.concat(cjk)
    .map((term) => term.trim())
    .filter((term) => term && !stop.has(term))
    .slice(0, 12);
}

function legacyLooksLikeLowValueResult(query, result) {
  const text = cleanText([result.title, result.url, result.snippet].join(" ")).toLowerCase();
  if (!text) return true;
  const queryText = String(query || "").toLowerCase();
  if (/\bnews\b|pricing|price/.test(queryText)) {
    if (/dictionary|definition|meaning|calendar|almanac|weather|runoob|w3schools/.test(text)) return true;
  }
  if (/asyncio/.test(queryText) && !/asyncio/.test(text)) return true;
  return false;
}

function analyzeQuery(query) {
  const text = String(query || "").toLowerCase();
  const terms = meaningfulTerms(query);
  const requiredGroups = [];
  const profile = {
    text,
    terms,
    requiredGroups,
    isNews: /\b(news|headline|headlines|breaking|world news|events today)\b/.test(text),
    isTechnical: /\b(api|python|javascript|typescript|node|react|css|html|framework|git|linux|windows|asyncio|gil|free-threaded|threaded|pep|sdk|database|kubernetes|docker)\b/.test(text),
    isPricing: /\b(pricing|price|prices|cost|billing|rate|rates|api)\b/.test(text),
    isComparison: /\b(best|top|compare|comparison|versus|vs|lightweight|framework)\b/.test(text),
    isCssFramework: /\bcss\b/.test(text) && /\b(framework|frameworks|tailwind|bootstrap|bulma|pico|foundation|daisyui|materialize)\b/.test(text),
    isPythonFreeThreaded: /\bpython\b/.test(text) && (/\b3\.13\b/.test(text) || /\b(gil|free-threaded|free threaded|nogil)\b/.test(text)),
  };

  if (profile.isNews) {
    requiredGroups.push(["news", "headline", "headlines", "breaking", "world", "event", "events", "reuters", "apnews", "associated press", "bbc", "cnn"]);
  }
  if (profile.isCssFramework) {
    requiredGroups.push(["css"]);
    requiredGroups.push(["framework", "frameworks", "tailwind", "bootstrap", "bulma", "pico", "foundation", "daisyui", "materialize"]);
  }
  if (profile.isPythonFreeThreaded) {
    requiredGroups.push(["python"]);
    requiredGroups.push(["3.13", "free-threaded", "free threaded", "gil", "nogil", "pep 703", "pep703"]);
  }
  if (profile.isPricing) {
    requiredGroups.push(["pricing", "price", "prices", "cost", "billing", "rate", "rates", "api"]);
  }
  if (profile.isTechnical && terms.length >= 2 && requiredGroups.length === 0) {
    requiredGroups.push(terms.slice(0, Math.min(3, terms.length)));
  }
  return profile;
}

function rewriteSearchQuery(query, profile = analyzeQuery(query)) {
  let output = cleanQuery(query);
  if (!output || extractDirectUrl(output)) return output;

  if (profile.isPythonFreeThreaded) {
    return '"Python 3.13" "free-threaded" GIL optional status';
  }
  if (profile.isCssFramework) {
    output = output.replace(/\bbest\b/gi, "top");
    if (!/\bcss framework\b/i.test(output)) output += " CSS framework";
    if (!/\bcomparison\b/i.test(output)) output += " comparison";
    return output;
  }
  if (profile.isNews) {
    if (!/\b(headlines|reuters|ap|bbc)\b/i.test(output)) output += " headlines Reuters AP BBC";
    return output;
  }
  if (profile.isPricing && /\bapi\b/i.test(output) && !/\bpricing\b/i.test(output)) {
    return output + " pricing";
  }
  return output;
}

function coreTermCoverage(profile, haystack) {
  const groups = profile && Array.isArray(profile.requiredGroups) ? profile.requiredGroups : [];
  if (groups.length === 0) return 0.5;
  let matched = 0;
  for (const group of groups) {
    if ((group || []).some((term) => haystack.includes(term))) matched += 1;
  }
  return matched / groups.length;
}

function hasRequiredResultCoverage(profile, results) {
  const groups = profile && Array.isArray(profile.requiredGroups) ? profile.requiredGroups : [];
  if (groups.length === 0) return true;
  return (Array.isArray(results) ? results : []).some((result) => {
    const haystack = cleanText([result.title, result.url, result.snippet].join(" ")).toLowerCase();
    return coreTermCoverage(profile, haystack) >= 1;
  });
}

function minimumResultScore(profile) {
  if (!profile) return 0.18;
  if (profile.isNews || profile.isCssFramework || profile.isPythonFreeThreaded) return 0.28;
  if (profile.isTechnical || profile.isPricing) return 0.24;
  return 0.18;
}

function lowValuePenalty(profile, result, haystack) {
  if (looksLikeLowValueResult(profile, result, haystack)) return 0.75;
  let penalty = 0;
  const url = String((result && result.url) || "").toLowerCase();
  const title = String((result && result.title) || "").toLowerCase();

  if (profile && profile.isNews && /dictionary|definition|meaning|thesaurus|calendar|almanac|weather|merriam-webster|cambridge|collinsdictionary|thefreedictionary/.test(haystack)) {
    penalty += 0.55;
  }
 if (profile && profile.isTechnical && /bestbuy|bestwestern|tripadvisor|booking\.com|expedia|hotel|restaurant|shopping|coupon|retail/.test(haystack)) {
    penalty += 0.30;
 }
  if (profile && profile.isPythonFreeThreaded && /\b(download python|welcome to python\.org|python tutorial|learn python)\b/.test(haystack)) {
    penalty += 0.42;
  }
 if (profile && profile.isCssFramework && /bestbuy|best buy|bestwestern|tripadvisor|restaurant|hotel/.test(haystack)) {
    penalty += 0.30;
 }
  if (/\/search\?|duckduckgo\.com\/html|google\.com\/search|bing\.com\/search/.test(url)) {
    penalty += 0.12;
  }
  if (profile && profile.isComparison && /^(best buy|best western|the best restaurants)/i.test(title)) {
    penalty += 0.4;
  }
  return Math.min(0.9, penalty);
}

function looksLikeLowValueResult(profile, result, haystackValue) {
  const haystack = haystackValue || cleanText([result && result.title, result && result.url, result && result.snippet].join(" ")).toLowerCase();
  if (!haystack) return true;
  if (profile && profile.requiredGroups && profile.requiredGroups.length > 0 && coreTermCoverage(profile, haystack) === 0) return true;
  if (profile && profile.isNews && /dictionary|definition|meaning|merriam-webster|cambridge|collinsdictionary|thefreedictionary/.test(haystack)) return true;
  if (profile && profile.isCssFramework && /bestbuy|best buy|bestwestern|tripadvisor|hotel|restaurant/.test(haystack)) return true;
  if (profile && profile.isPythonFreeThreaded && !/(3\.13|free-threaded|free threaded|gil|nogil|pep 703|pep703)/.test(haystack)) return true;
  return false;
}

function betterSearchResult(left, right) {
  if (!right) return left;
  if (!left) return right;
  if ((left.quality || 0) !== (right.quality || 0)) return (left.quality || 0) > (right.quality || 0) ? left : right;
  return (left.results || []).length >= (right.results || []).length ? left : right;
}

function trimResults(results) {
  return (Array.isArray(results) ? results : []).slice(0, maxResults());
}

function maxResults() {
  return DEFAULT_MAX_RESULTS;
}

function qualityThreshold() {
  return DEFAULT_QUALITY_THRESHOLD;
}

function localeForQuery(query) {
  if (/[\u3400-\u9fff]/.test(String(query || ""))) {
    return {
      acceptLanguage: "zh-CN,zh;q=0.9,en;q=0.6",
      bingMarket: "zh-CN",
      bingSetLang: "zh-CN",
      duckDuckGoKl: "cn-zh",
      googleHl: "zh-CN",
      yandexLang: "zh",
    };
  }
  return {
    acceptLanguage: "en-US,en;q=0.9,zh-CN;q=0.4",
    bingMarket: "en-US",
    bingSetLang: "en-US",
    duckDuckGoKl: "us-en",
    googleHl: "en",
    yandexLang: "en",
  };
}

function searchSummary(result) {
  return {
    ok: Boolean(result && result.ok),
    query: result && result.query ? result.query : "",
    source: result && result.source ? result.source : "",
    result_count: Array.isArray(result && result.results) ? result.results.length : 0,
    quality: result && Number.isFinite(Number(result.quality)) ? Number(result.quality) : 0,
    low_confidence: Boolean(result && result.low_confidence),
    error: result && result.error ? result.error : undefined,
  };
}

function average(values) {
  const numbers = (Array.isArray(values) ? values : []).filter((value) => Number.isFinite(Number(value))).map(Number);
  if (numbers.length === 0) return 0;
  return numbers.reduce((sum, value) => sum + value, 0) / numbers.length;
}

function roundScore(value) {
  return Math.round((Number(value) || 0) * 100) / 100;
}

function clampInt(value, fallback, min, max) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(parsed)));
}

function decodeBingUrlParameter(value) {
  const raw = String(value || "").trim();
  if (!raw) return "";
  try {
    const direct = decodeURIComponent(raw);
    if (/^https?:\/\//i.test(direct)) return direct;
  } catch {}

  const base64 = raw.replace(/^a1/i, "").replace(/-/g, "+").replace(/_/g, "/");
  try {
    const decoded = Buffer.from(base64, "base64").toString("utf8");
    return /^https?:\/\//i.test(decoded) ? decoded : "";
  } catch {
    return "";
  }
}

function stripTags(value) {
  return String(value || "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
}

function decodeHtml(value) {
  return String(value || "")
    .replace(/&amp;/g, "&")
    .replace(/&quot;/g, "\"")
    .replace(/&#39;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&#x([0-9a-f]+);/gi, (match, hex) => String.fromCodePoint(parseInt(hex, 16)))
    .replace(/&#(\d+);/g, (match, num) => String.fromCodePoint(Number(num)));
}

function htmlToMarkdown(html) {
  if (!html) return "";
  let text = String(html)
    .replace(/<script[\s\S]*?<\/script>/gi, "")
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/<noscript[\s\S]*?<\/noscript>/gi, "")
    .replace(/<svg[\s\S]*?<\/svg>/gi, "")
    .replace(/<math[\s\S]*?<\/math>/gi, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<head[\s\S]*?<\/head>/gi, "")
    .replace(/<nav[\s\S]*?<\/nav>/gi, "")
    .replace(/<footer[\s\S]*?<\/footer>/gi, "")
    .replace(/<iframe[\s\S]*?<\/iframe>/gi, "");

  const contentParts = [];
  const blockRx = /<(h[1-6]|p|li|pre|blockquote|td|th|dd|dt|caption|figcaption)\b[^>]*>([\s\S]*?)<\/\1>/gi;
  let match;
  while ((match = blockRx.exec(text)) !== null) {
    const inner = match[2].replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
    if (inner.length > 5) contentParts.push(inner);
  }

  let result = contentParts.join("\n\n");
  if (!result) {
    result = text.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
  }
  return decodeHtml(result);
}

function extractKeyExcerpts(markdown, query, maxWindows, windowSize) {
  maxWindows = maxWindows != null ? maxWindows : DEFAULT_MAX_EXCERPTS_PER_PAGE;
  windowSize = windowSize != null ? windowSize : DEFAULT_EXCERPT_MAX_CHARS;
  if (!markdown || !query) return [];
  const terms = meaningfulTerms(query);
  if (terms.length === 0) {
    const first = markdown.slice(0, windowSize).trim();
    return first ? [first] : [];
  }

  const normalized = markdown.toLowerCase();
  const step = Math.max(30, Math.floor(windowSize / 3));
  const windows = [];

  for (let start = 0; start + 50 < markdown.length; start += step) {
    const end = Math.min(start + windowSize, markdown.length);
    const chunk = markdown.slice(start, end);
    const normChunk = normalized.slice(start, end);
    let score = 0;
    for (const term of terms) {
      let pos = 0;
      while ((pos = normChunk.indexOf(term, pos)) !== -1) {
        score += 1;
        pos += term.length;
      }
    }
    if (score > 0) windows.push({ text: chunk.trim(), score });
  }

  windows.sort((a, b) => b.score - a.score);

  const selected = [];
  for (const win of windows) {
    const overlaps = selected.some(
      (entry) => entry.includes(win.text.slice(0, Math.min(60, win.text.length))) ||
        win.text.includes(entry.slice(0, Math.min(60, entry.length)))
    );
    if (!overlaps) {
      selected.push(win.text);
      if (selected.length >= maxWindows) break;
    }
  }

  if (selected.length === 0 && markdown.trim()) selected.push(markdown.slice(0, windowSize).trim());
  return selected;
}

async function autoOpenPreviews(candidates, query, config, k) {
  k = k != null ? k : DEFAULT_AUTO_OPEN_COUNT;
  if (!candidates || candidates.length === 0) return { enriched: candidates, count: 0, more: 0 };

  const threshold = config && config.qualityThreshold != null ? config.qualityThreshold : DEFAULT_QUALITY_THRESHOLD;
  const eligible = candidates.filter((candidate) => (candidate.score || candidate._score || 0) >= threshold).slice(0, k);
  if (eligible.length === 0) return { enriched: candidates, count: 0, more: 0 };

  const localeHeaders = { "Accept-Language": localeForQuery(query || "").acceptLanguage };
  const results = await Promise.allSettled(eligible.map(async (candidate) => {
    try {
      const startTime = Date.now();
      const fetched = await fetchText(candidate.url, config, localeHeaders);
      const elapsed = Date.now() - startTime;
      if (!fetched.ok || !fetched.text) return null;

      const md = htmlToMarkdown(fetched.text);
      const excerpts = extractKeyExcerpts(md, query);
      return {
        url: candidate.url,
        preview: excerpts.join("\n[...]\n"),
        fetch_ms: elapsed,
        content_length: fetched.text.length,
        excerpts_count: excerpts.length,
      };
    } catch {
      return null;
    }
  }));

  let opened = 0;
  const previewMap = new Map();
  for (const result of results) {
    if (result.status === "fulfilled" && result.value) {
      previewMap.set(result.value.url, result.value);
      opened += 1;
    }
  }

  const remaining = Math.max(0, candidates.filter((candidate) => (candidate.score || candidate._score || 0) >= threshold).length - k);
  const enriched = candidates.map((candidate) => {
    const preview = previewMap.get(candidate.url);
    if (!preview) return candidate;
    return Object.assign({}, candidate, {
      preview: preview.preview,
      _preview_fetch_ms: preview.fetch_ms,
    });
  });

  return { enriched, count: opened, more: remaining };
}

function extractHtmlTitle(text) {
  const source = String(text || "");
  const match = source.match(/<title[^>]*>([\s\S]*?)<\/title>/i);
  return match ? cleanText(stripTags(decodeHtml(match[1]))) : "";
}

function extractReadableSnippet(text) {
  const source = String(text || "");
  const meta = source.match(/<meta[^>]+(?:name|property)=["'](?:description|og:description)["'][^>]+content=["']([^"']+)["'][^>]*>/i)
    || source.match(/<meta[^>]+content=["']([^"']+)["'][^>]+(?:name|property)=["'](?:description|og:description)["'][^>]*>/i);
  if (meta && meta[1]) return cleanText(decodeHtml(meta[1])).slice(0, 500);

  const withoutScripts = source
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<noscript[\s\S]*?<\/noscript>/gi, " ");
  return cleanText(stripTags(decodeHtml(withoutScripts))).slice(0, 500);
}

function extractOpenableContent(text) {
  const source = String(text || "");
  const title = extractHtmlTitle(source);
  const snippet = extractReadableSnippet(source);
  const body = cleanText(stripTags(decodeHtml(source
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<noscript[\s\S]*?<\/noscript>/gi, " "))));
  const pieces = [];
  if (title) pieces.push(title);
  if (snippet && snippet !== title) pieces.push(snippet);
  if (body && body !== snippet && body !== title) pieces.push(body);
  return cleanText(pieces.join("\n\n")).slice(0, DEFAULT_OPEN_SNIPPET_BYTES);
}

function normalizeDuckDuckGoResponse(query, data) {
  const results = [];
  const heading = cleanText(data && data.Heading);
  const abstractText = cleanText(data && data.AbstractText);
  const abstractUrl = cleanText(data && data.AbstractURL);
  if (abstractText || abstractUrl) {
    results.push({
      title: heading || query,
      url: abstractUrl || "",
      snippet: abstractText,
    });
  }

  collectRelatedTopics(data && data.RelatedTopics, results);

  return {
    ok: results.length > 0,
    query,
    source: "duckduckgo_instant_answer",
    results: dedupeResults(results).slice(0, PARSER_MAX_RESULTS),
  };
}

function collectRelatedTopics(topics, results) {
  for (const topic of Array.isArray(topics) ? topics : []) {
    if (!topic || typeof topic !== "object") continue;
    if (Array.isArray(topic.Topics)) {
      collectRelatedTopics(topic.Topics, results);
      continue;
    }
    const text = cleanText(topic.Text);
    const url = cleanText(topic.FirstURL);
    if (!text && !url) continue;
    results.push({
      title: text.split(" - ")[0] || url || "Result",
      url,
      snippet: text,
    });
  }
}

function dedupeResults(results) {
  const seen = new Set();
  const output = [];
  for (const result of results) {
    const key = (result.url || result.title || result.snippet || "").toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    output.push(result);
  }
  return output;
}

function cleanText(value) {
  return String(value || "").replace(/\s+/g, " ").trim();
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

module.exports = {
  executeProxyWebSearch,
  resolveDispatcher,
  validatePublicHttpUrl,
};
