# Community Tools

Community tools are local CodeSeeX extensions installed under:

```text
~/.codeseex-next/
  extension/
    tools/
      my-tool/
        manifest.json
        tool.js
        assets/
          icon.svg
```

`manifest.json` is required. Tool code is optional, but CodeSeeX only advertises an enabled community tool to the model when the manifest declares an explicit command executor.

## Manifest

```json
{
  "id": "my_tool",
  "name": "My Tool",
  "description": "Client-facing description.",
  "model": {
    "description": "Model-facing description in concise English.",
    "parameters": {
      "type": "object",
      "properties": {
        "query": { "type": "string" }
      },
      "required": ["query"],
      "additionalProperties": false
    }
  },
  "execution": {
    "type": "command",
    "command": "node",
    "args": ["tool.js"],
    "timeout_ms": 20000
  },
  "config": [
    {
      "key": "MY_TOOL_MODE",
      "type": "select",
      "label": "Mode",
      "defaultValue": "safe",
      "options": [
        { "value": "safe", "label": "Safe" },
        { "value": "fast", "label": "Fast" }
      ]
    }
  ]
}
```

## Execution Contract

CodeSeeX runs community tools as child processes, not inside the proxy process.

- No shell is used; `command` and `args` are passed directly to the OS.
- The process starts in the tool directory.
- Environment variables are minimized and do not intentionally include upstream API keys.
- Input is written to stdin as JSON.
- Stdout should return JSON; plain text is wrapped as `{ "ok": true, "text": "..." }`.
- Stdout and stderr are bounded, and execution times out at `timeout_ms`.

Input shape:

```json
{
  "tool": "my_tool",
  "arguments": { "query": "hello" },
  "raw_arguments": "{\"query\":\"hello\"}",
  "settings": { "MY_TOOL_MODE": "safe" },
  "workspace_root": "D:\\example\\workspace"
}
```

Expected stdout:

```json
{
  "ok": true,
  "tool": "my_tool",
  "result": "..."
}
```

## Safety Notes

Community tools are disabled by default. Enabling one means you trust the local command it runs. This executor avoids in-process plugin loading, but it is not a full OS sandbox.
