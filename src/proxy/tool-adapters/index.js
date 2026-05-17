const { listToolRegistry } = require("../../shared/tool-registry");
const { listToolAdapters } = require("../../tools");

function registerInputToolAdapter(tool, state, options = {}) {
  const adapter = adapters(options).find((item) => item.matchesInputTool(tool));
  if (!adapter) return false;
  adapter.registerInputTool(tool, state, options);
  return true;
}

function responseItemFromAdaptedChatTool(toolCall, context, helpers) {
  const adapter = adapters(context).find((item) => (
    typeof item.matchesChatTool === "function"
    && typeof item.responseItemFromChatTool === "function"
    && item.matchesChatTool(toolCall, context)
  ));
  return adapter ? adapter.responseItemFromChatTool(toolCall, context, helpers) : null;
}

function chatToolCallFromAdaptedResponseItem(item, helpers) {
  const adapter = adapters().find((entry) => (
    typeof entry.matchesResponseItem === "function"
    && typeof entry.chatToolCallFromResponseItem === "function"
    && entry.matchesResponseItem(item)
  ));
  return adapter ? adapter.chatToolCallFromResponseItem(item, helpers) : null;
}

function emitAdaptedOutputEvents(item, emit) {
  const adapter = adapters().find((entry) => (
    typeof entry.matchesResponseItem === "function"
    && entry.matchesResponseItem(item)
  ));
  if (!adapter || typeof adapter.emitOutputEvents !== "function") return;
  adapter.emitOutputEvents(item, emit);
}

function adapters(options = {}) {
  return listToolAdapters({
    rootDir: options.rootDir,
    extensionDir: options.extensionDir,
    communityToolCodeEnabled: options.communityToolCodeEnabled,
  });
}

module.exports = {
  chatToolCallFromAdaptedResponseItem,
  emitAdaptedOutputEvents,
  listToolRegistry,
  registerInputToolAdapter,
  responseItemFromAdaptedChatTool,
};
