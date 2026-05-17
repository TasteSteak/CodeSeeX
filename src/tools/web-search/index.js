const { makeId } = require("../../shared/http");

function matchesInputTool(tool) {
  const type = String((tool && tool.type) || "").toLowerCase();
  return type === "web_search" || type === "web_search_preview";
}

function registerInputTool(tool, state) {
  if (!state.byName.has("web_search")) {
    state.upstreamTools.push(modelTool(tool));
  }
  state.byName.set("web_search", { kind: "hosted_web_search", nativeTool: tool });
  state.byName.set("web_search_preview", { kind: "hosted_web_search", nativeTool: tool });
}

function modelTool(tool = {}) {
  return {
    type: "function",
    function: {
      name: "web_search",
      description: tool.description || [
        "Search the web for current information.",
        "Use mode=\"search\" to collect candidate results with ids, titles, URLs, snippets, scores, and auto-opened previews for top-ranked pages.",
        "Then use mode=\"open\" with selected open_ids or open_urls when page content is needed.",
        "Avoid repeated equivalent searches; prefer opening the most relevant candidates and then answer from the gathered evidence.",
      ].join(" "),
      parameters: {
        type: "object",
        properties: {
          mode: { type: "string", enum: ["search", "open"] },
          query: { type: "string" },
          queries: { type: "array", items: { type: "string" } },
          open_urls: { type: "array", items: { type: "string" } },
          open_ids: { type: "array", items: { type: "string" } },
          search_context_size: { type: "string" },
          external_web_access: { type: "boolean" },
        },
        additionalProperties: true,
      },
    },
  };
}

function matchesChatTool(toolCall, context) {
  return isHostedWebSearchName(context.getToolName(toolCall), context);
}

function responseItemFromChatTool(toolCall, context, helpers) {
  const parsed = helpers.parseJson(context.getToolArguments(toolCall)) || {};
  const action = normalizeWebSearchAction(parsed);
  return {
    id: makeId("ws"),
    type: "web_search_call",
    call_id: toolCall.id || makeId("call"),
    status: "completed",
    action,
  };
}

function matchesResponseItem(item) {
  return Boolean(item && item.type === "web_search_call");
}

function emitOutputEvents(item, emit) {
  emit("response.web_search_call.in_progress", {
    type: "response.web_search_call.in_progress",
    item_id: item.id,
  });
  const eventName = item && item.action && item.action.type === "open"
    ? "response.web_search_call.opening"
    : "response.web_search_call.searching";
  emit(eventName, {
    type: eventName,
    item_id: item.id,
    action: item.action || null,
  });
  emit("response.web_search_call.completed", {
    type: "response.web_search_call.completed",
    item_id: item.id,
  });
}

function chatToolCallFromResponseItem(item) {
  const action = (item && item.action) || {};
  const args = normalizeWebSearchArguments(action);
  return {
    id: item.call_id || item.id || makeId("call"),
    type: "function",
    function: {
      name: "web_search",
      arguments: JSON.stringify(args),
    },
  };
}

function normalizeWebSearchAction(parsed) {
  const searchQueries = normalizeSearchQueries(parsed);
  const openUrls = normalizeOpenTargets(parsed.open_urls || parsed.urls || parsed.url);
  const openIds = normalizeOpenTargets(parsed.open_ids || parsed.ids || parsed.id);
  const explicitMode = String((parsed && parsed.mode) || "").trim().toLowerCase();
  const directUrls = !openUrls.length && !openIds.length && searchQueries.length === 1
    ? normalizeOpenTargets(extractDirectUrl(searchQueries[0]))
    : [];
  const shouldOpen = explicitMode === "open" || openUrls.length > 0 || openIds.length > 0 || directUrls.length > 0;

  if (shouldOpen) {
    const urls = openUrls.length > 0 ? openUrls : directUrls;
    const action = { type: "open" };
    if (urls.length > 0) action.urls = urls;
    if (openIds.length > 0) action.ids = openIds;
    if (searchQueries.length > 0 && !(urls.length > 0 && searchQueries.length === 1 && urls[0] === extractDirectUrl(searchQueries[0]))) {
      action.query = searchQueries.join("\n");
    }
    return action;
  }

  const action = { type: "search", query: searchQueries.join("\n") };
  if (searchQueries.length > 1) action.queries = searchQueries;
  return action;
}

function normalizeWebSearchArguments(action) {
  if (!action || typeof action !== "object") return { mode: "search", query: "" };
  if (action.type === "open") {
    const args = { mode: "open" };
    if (Array.isArray(action.urls) && action.urls.length > 0) args.open_urls = action.urls;
    if (Array.isArray(action.ids) && action.ids.length > 0) args.open_ids = action.ids;
    if (typeof action.query === "string" && action.query.trim()) args.query = action.query;
    return args;
  }
  const args = { mode: "search" };
  if (Array.isArray(action.queries) && action.queries.length > 0) args.queries = action.queries;
  else args.query = action.query || "";
  return args;
}

function normalizeOpenTargets(value) {
  const raw = Array.isArray(value) ? value : (value ? [value] : []);
  const seen = new Set();
  const output = [];
  for (const entry of raw) {
    const text = String(entry || "").trim();
    if (!text) continue;
    const key = text.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    output.push(text);
  }
  return output;
}

function extractDirectUrl(value) {
  const text = String(value || "").trim();
  if (!text) return "";
  const explicit = text.match(/https?:\/\/[^\s"'<>]+/i);
  if (explicit) return trimUrlPunctuation(explicit[0]);
  if (/^[a-z0-9.-]+\.[a-z]{2,}(?:\/[^\s]*)?$/i.test(text)) return "https://" + trimUrlPunctuation(text);
  return "";
}

function trimUrlPunctuation(value) {
  return String(value || "").trim().replace(/[),.;]+$/, "");
}

function isHostedWebSearchName(name, context) {
  const entry = context && context.byName ? context.byName.get(name) : null;
  return Boolean(entry && entry.kind === "hosted_web_search");
}

function normalizeSearchQueries(parsed) {
  if (!parsed || typeof parsed !== "object") return [];
  if (Array.isArray(parsed.queries)) return parsed.queries.map(cleanQuery).filter(Boolean);
  if (Array.isArray(parsed.search_query)) {
    return parsed.search_query
      .map((query) => typeof query === "string" ? cleanQuery(query) : cleanQuery(query && (query.q || query.query)))
      .filter(Boolean);
  }
  if (typeof parsed.query === "string") return [cleanQuery(parsed.query)].filter(Boolean);
  if (typeof parsed.q === "string") return [cleanQuery(parsed.q)].filter(Boolean);
  return [];
}

function cleanQuery(value) {
  return String(value || "").trim();
}

module.exports = {
  chatToolCallFromResponseItem,
  emitOutputEvents,
  matchesChatTool,
  matchesInputTool,
  matchesResponseItem,
  modelTool,
  normalizeSearchQueries,
  registerInputTool,
  responseItemFromChatTool,
};
