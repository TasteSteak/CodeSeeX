# CodeSeeX Tool Authoring

CodeSeeX discovers tools from two places:

- Built-in tools under `src/tools/<tool>/`
- Community tools under `~/.codeseex/extension/tools/<tool>/` by default

For community tools, installation should be lightweight: drop a folder into `~/.codeseex/extension/tools/` and restart CodeSeeX. In development, set `PROXY_DATA_DIR` if you want to test a separate runtime data directory.

## Folder Layout

```text
~/.codeseex/
  extension/
    tools/
      my-tool/
        manifest.json
        index.js
        assets/
          icon.svg
```

Only `manifest.json` is required for the tool to appear in the client. Add `index.js` only when the tool needs proxy/runtime behavior. Add `assets/icon.svg` or `assets/icon.png` when the tool needs a custom icon.

## manifest.json

```json
{
  "id": "my_tool",
  "name": "My Tool",
  "description": "Short client-facing description.",
  "kind": "tool",
  "version": "1",
  "enabled": true,
  "config": [
    {
      "key": "MY_TOOL_ENABLED",
      "type": "boolean",
      "label": "Enabled",
      "description": "Turn this tool on or off.",
      "defaultValue": "true"
    }
  ],
  "metadata": {
    "author": "Your Name"
  }
}
```

Supported top-level fields:

- `id`: Required stable identifier. Use lowercase letters, numbers, `_`, or `-`.
- `name`: Display name shown in the client. If omitted, CodeSeeX falls back to `id`.
- `description`: Display description shown in the client.
- `kind`: Optional category string. Defaults to `tool`.
- `version`: Optional version string. Defaults to `1`.
- `enabled`: Optional boolean. Defaults to `true`.
- `config`: Optional client configuration fields.
- `metadata`: Optional plain JSON metadata.

Tool `name` and `description` are always read directly from `manifest.json`. They should not use language translation keys.

## Icons

CodeSeeX auto-discovers one fixed icon file:

- `assets/icon.svg`
- `assets/icon.png`

No manifest icon path is required. SVG is preferred. If no icon file exists, the client falls back to the manifest icon text or the tool id initials.

## Config Fields

Config fields are optional and may use translation keys because they are UI controls, not tool identity.

Supported field types:

- `text`
- `textarea`
- `password`
- `number`
- `boolean`
- `select`
- `segmented`

Example:

```json
{
  "key": "MY_TOOL_MODE",
  "type": "select",
  "label": "Mode",
  "description": "Choose how the tool should behave.",
  "defaultValue": "safe",
  "options": [
    { "value": "safe", "label": "Safe" },
    { "value": "fast", "label": "Fast" }
  ]
}
```

## index.js

`index.js` is optional. Use it when the tool needs to register model-facing schemas, map tool calls, execute proxy behavior, or customize history replay.

```js
module.exports = {
  modelTool(config) {
    return {
      type: "function",
      function: {
        name: "my_tool",
        description: "Hardcoded English instruction for the model.",
        parameters: {
          type: "object",
          properties: {
            query: { type: "string" }
          },
          required: ["query"]
        }
      }
    };
  }
};
```

Model-facing descriptions should stay concise, explicit, and hardcoded in English. Client-facing names and descriptions belong in `manifest.json`.

## Safety Rules

- A malformed community tool should not break CodeSeeX startup.
- A missing or invalid `id` means the tool is ignored.
- Missing `name` falls back to `id`.
- Missing icons fall back gracefully.
- Remote icon URLs are ignored.
- Config saves are whitelist-based: only keys declared in tool manifests can be saved.
