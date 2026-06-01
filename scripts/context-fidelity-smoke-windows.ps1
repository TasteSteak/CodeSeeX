param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$DevRoot = "D:\DevTools\CodeSeeXNext",
  [string]$DataDir = "D:\DevTools\CodeSeeXNext\ContextSmokeData",
  [int]$ProxyPort = 8804,
  [int]$FakeUpstreamPort = 8892
)

$ErrorActionPreference = "Stop"

function Write-Step([string]$Message) {
  Write-Host "[context-smoke] $Message"
}

function Test-TcpPort([string]$HostName, [int]$Port, [int]$TimeoutMs = 200) {
  $client = [System.Net.Sockets.TcpClient]::new()
  try {
    $async = $client.BeginConnect($HostName, $Port, $null, $null)
    if (-not $async.AsyncWaitHandle.WaitOne($TimeoutMs)) {
      return $false
    }
    $client.EndConnect($async)
    return $true
  } catch {
    return $false
  } finally {
    $client.Close()
  }
}

function Wait-TcpPort([string]$HostName, [int]$Port, [int]$Retries, [int]$DelayMs, [scriptblock]$OnTick) {
  foreach ($index in 1..$Retries) {
    if (& $OnTick) {
      return $false
    }
    if (Test-TcpPort -HostName $HostName -Port $Port) {
      return $true
    }
    Start-Sleep -Milliseconds $DelayMs
  }
  return $false
}

$RepoRoot = Split-Path -Parent $PSScriptRoot
$LogDir = Join-Path $DevRoot "Logs"
$SmokeDir = Join-Path $DevRoot "Smoke"

$env:CARGO_HOME = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $DevRoot "Cargo" }
$env:CARGO_TARGET_DIR = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $DevRoot "CargoTarget" }
$env:npm_config_cache = if ($env:npm_config_cache) { $env:npm_config_cache } else { Join-Path $DevRoot "npm-cache" }
$env:TEMP = Join-Path $DevRoot "Temp"
$env:TMP = $env:TEMP

New-Item -ItemType Directory -Force -Path `
  $env:CARGO_HOME, `
  $env:CARGO_TARGET_DIR, `
  $env:npm_config_cache, `
  $env:TEMP, `
  $DataDir, `
  $LogDir, `
  $SmokeDir | Out-Null

if (-not $VsDevCmd) {
  $defaultVsDevCmd = Join-Path $DevRoot "VSBuildTools\Common7\Tools\VsDevCmd.bat"
  if (Test-Path $defaultVsDevCmd) {
    $VsDevCmd = $defaultVsDevCmd
  }
}

if (-not $VsDevCmd -or -not (Test-Path $VsDevCmd)) {
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  if (Test-Path $vswhere) {
    $installPath = & $vswhere -all -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1
    if ($installPath) {
      $candidate = Join-Path $installPath "Common7\Tools\VsDevCmd.bat"
      if (Test-Path $candidate) {
        $VsDevCmd = $candidate
      }
    }
  }
}

if (-not $VsDevCmd -or -not (Test-Path $VsDevCmd)) {
  throw "MSVC Build Tools not found. Set CODESEEX_VSDEVCMD or install Visual Studio Build Tools with VC.Tools.x86.x64."
}

$cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
if (-not (Test-Path $cargo)) {
  $cargo = "cargo"
}

$payloadFile = Join-Path $SmokeDir "context-smoke-upstream-payload.json"
$balanceFile = Join-Path $SmokeDir "context-smoke-balance-request.json"
$authFile = Join-Path $SmokeDir "context-smoke-auth.json"
$fakeServer = Join-Path $SmokeDir "context-smoke-upstream.js"
$fakeOut = Join-Path $LogDir "context-smoke-upstream.out.log"
$fakeErr = Join-Path $LogDir "context-smoke-upstream.err.log"
$proxyOut = Join-Path $LogDir "context-smoke-proxy.out.log"
$proxyErr = Join-Path $LogDir "context-smoke-proxy.err.log"
$dataUrlFixture = Join-Path $RepoRoot "fixtures\data-url-smoke.txt"
$applyPatchFixture = Join-Path $RepoRoot "fixtures\apply-patch-smoke.txt"

Remove-Item -LiteralPath $payloadFile, $balanceFile, $authFile, $fakeOut, $fakeErr, $proxyOut, $proxyErr -Force -ErrorAction SilentlyContinue
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($dataUrlFixture, "INLINE_DATA_URL=data:image/png;base64,AAAAAAAAAABBBBBBBBBB", $utf8NoBom)
[System.IO.File]::WriteAllText($applyPatchFixture, "before", $utf8NoBom)

@'
const http = require("http");
const fs = require("fs");

const payloadFile = process.env.PAYLOAD_FILE;
const balanceFile = process.env.BALANCE_FILE;
const port = Number(process.env.FAKE_UPSTREAM_PORT || "8892");

const server = http.createServer((req, res) => {
  if (req.method === "GET" && req.url === "/web-fixture") {
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end("<!doctype html><html><head><title>CodeSeeX Web Fixture</title><script>window.noise = true;</script></head><body><main><h1>CodeSeeX Web Fixture</h1><p>WEB_SEARCH_FIXTURE_OK text evidence.</p></main></body></html>");
    return;
  }

  if (req.method === "GET" && req.url === "/user/balance") {
    fs.writeFileSync(balanceFile, JSON.stringify({ path: req.url, authorization: req.headers.authorization || "" }), "utf8");
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      is_available: true,
      balance_infos: [
        {
          currency: "CNY",
          total_balance: "8.8",
          granted_balance: "1.2",
          topped_up_balance: "7.6"
        }
      ]
    }));
    return;
  }

  if (req.method === "POST" && (req.url === "/v1/chat/completions" || req.url === "/chat/completions")) {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      const parsed = JSON.parse(body);
      fs.writeFileSync(payloadFile, JSON.stringify({ path: req.url, body: parsed }), "utf8");
      if (!["deepseek-v4-pro", "deepseek-v4-flash"].includes(String(parsed.model || ""))) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { code: "unsupported_model", message: `unsupported model ${parsed.model}` } }));
        return;
      }
      const toolNames = (parsed.tools || []).map((tool) => tool && tool.function && tool.function.name).filter(Boolean);
      if (new Set(toolNames).size !== toolNames.length) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { code: "duplicate_tools", message: "Tool names must be unique." } }));
        return;
      }
      const messages = parsed.messages || [];
      const messageText = messages.map((message) => String(message.content || "")).join("\n");
      const lastUserText = [...messages].reverse().find((message) => message.role === "user")?.content || "";
      if (String(lastUserText).includes("force upstream failure")) {
        res.writeHead(500, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { code: "forced_failure", message: "forced smoke failure" } }));
        return;
      }
      function sendAssistant(content, id = "chatcmpl_context_smoke") {
        if (parsed.stream) {
          res.writeHead(200, { "content-type": "text/event-stream" });
          res.write(`data: ${JSON.stringify({ id, choices: [{ delta: { content } }], usage: { prompt_tokens: 15, completion_tokens: 3, total_tokens: 18 } })}\r\n\r\n`);
          res.end("data: [DONE]\r\n\r\n");
          return;
        }
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({
          id,
          object: "chat.completion",
          created: Math.floor(Date.now() / 1000),
          model: "deepseek-v4-pro",
          choices: [
            {
              index: 0,
              message: { role: "assistant", content },
              finish_reason: "stop"
            }
          ],
          usage: { prompt_tokens: 15, completion_tokens: 3, total_tokens: 18 }
        }));
      }
      function sendToolCall(callId, name, args) {
        if (parsed.stream) {
          res.writeHead(200, { "content-type": "text/event-stream" });
          res.write(`data: ${JSON.stringify({
            id: "chatcmpl_context_smoke_" + name + "_call",
            choices: [
              {
                delta: {
                  tool_calls: [
                    {
                      index: 0,
                      id: callId,
                      type: "function",
                      function: { name, arguments: JSON.stringify(args) }
                    }
                  ]
                },
                finish_reason: "tool_calls"
              }
            ],
            usage: { prompt_tokens: 12, completion_tokens: 1, total_tokens: 13 }
          })}\r\n\r\n`);
          res.end("data: [DONE]\r\n\r\n");
          return;
        }
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({
          id: `chatcmpl_context_smoke_${name}_call`,
          object: "chat.completion",
          created: Math.floor(Date.now() / 1000),
          model: "deepseek-v4-pro",
          choices: [
            {
              index: 0,
              message: {
                role: "assistant",
                content: null,
                tool_calls: [
                  {
                    id: callId,
                    type: "function",
                    function: { name, arguments: JSON.stringify(args) }
                  }
                ]
              },
              finish_reason: "tool_calls"
            }
          ],
          usage: { prompt_tokens: 12, completion_tokens: 1, total_tokens: 13 }
        }));
      }
      const toolMessages = messages;
      const externalResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_external_smoke_add");
      if (externalResult) {
        const ok = String(externalResult.content || "").includes("42");
        sendAssistant(ok ? "external-tool-result-ok" : "external-tool-result-missing", "chatcmpl_context_smoke_external_tool_final");
        return;
      }
      const communityResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_community_smoke");
      if (communityResult) {
        const content = String(communityResult.content || "");
        const ok = content.includes("community_smoke")
          && content.includes("\"sum\":42")
          && content.includes("\"mode\":\"fast\"");
        sendAssistant(ok ? "community-tool-ok" : "community-tool-missing-result", "chatcmpl_context_smoke_community_tool_final");
        return;
      }
      const listResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_list_directory");
      if (listResult) {
        const ok = String(listResult.content || "").includes("Cargo.toml");
        sendAssistant(ok ? "tool-loop-ok" : "tool-loop-missing-cargo", "chatcmpl_context_smoke_list_directory_final");
        return;
      }
      const readResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_read_file_range");
      if (readResult) {
        const ok = String(readResult.content || "").includes("[workspace]") || String(readResult.content || "").includes("members");
        sendAssistant(ok ? "read-tool-ok" : "read-tool-missing-content", "chatcmpl_context_smoke_read_file_range_final");
        return;
      }
      const dataUrlReadResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_data_url_read");
      if (dataUrlReadResult) {
        const content = String(dataUrlReadResult.content || "");
        const ok = content.includes("[inline-data-url omitted") && !content.includes("AAAAAAAAAABBBBBBBBBB");
        sendAssistant(ok ? "data-url-redacted-ok" : "data-url-leaked", "chatcmpl_context_smoke_data_url_read_final");
        return;
      }
      const searchResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_workspace_search");
      if (searchResult) {
        const ok = String(searchResult.content || "").includes("CodeSeeX Next");
        sendAssistant(ok ? "search-tool-ok" : "search-tool-missing-content", "chatcmpl_context_smoke_workspace_search_final");
        return;
      }
      const webSearchResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_web_search");
      if (webSearchResult) {
        const ok = String(webSearchResult.content || "").includes("WEB_SEARCH_FIXTURE_OK")
          && !String(webSearchResult.content || "").includes("window.noise");
        sendAssistant(ok ? "web-search-tool-ok" : "web-search-tool-missing-content", "chatcmpl_context_smoke_web_search_final");
        return;
      }
      const applyPatchResult = toolMessages.find((message) => message.role === "tool" && message.tool_call_id === "call_apply_patch");
      if (applyPatchResult) {
        const ok = String(applyPatchResult.content || "").includes("\"ok\":true")
          && String(applyPatchResult.content || "").includes("apply-patch-smoke.txt");
        sendAssistant(ok ? "apply-patch-tool-ok" : "apply-patch-tool-failed", "chatcmpl_context_smoke_apply_patch_final");
        return;
      }
      if (String(lastUserText).includes("call list_directory tool")) {
        sendToolCall("call_list_directory", "list_directory", { path: ".", depth: 0 });
        return;
      }
      if (String(lastUserText).includes("call disabled read tool")) {
        sendToolCall("call_disabled_read_file_range", "read_file_range", { path: "Cargo.toml", start: 1, count: 2 });
        return;
      }
      if (String(lastUserText).includes("call read_file_range tool")) {
        sendToolCall("call_read_file_range", "read_file_range", { path: "Cargo.toml", start: 1, count: 8 });
        return;
      }
      if (String(lastUserText).includes("call data_url read_file_range tool")) {
        sendToolCall("call_data_url_read", "read_file_range", { path: "fixtures/data-url-smoke.txt", start: 1, count: 2 });
        return;
      }
      if (String(lastUserText).includes("call workspace_search tool")) {
        sendToolCall("call_workspace_search", "workspace_search", { query: "CodeSeeX Next", path: "README.md", max_results: 5 });
        return;
      }
      if (String(lastUserText).includes("call web_search tool")) {
        sendToolCall("call_web_search", "web_search", { mode: "open", url: `http://127.0.0.1:${port}/web-fixture` });
        return;
      }
      if (String(lastUserText).includes("call apply_patch tool")) {
        const patch = [
          "*** Begin Patch",
          "*** Update File: fixtures/apply-patch-smoke.txt",
          "@@",
          "-before",
          "+after",
          "*** End Patch"
        ].join("\n");
        sendToolCall("call_apply_patch", "apply_patch", { patch });
        return;
      }
      if (String(lastUserText).includes("call external mcp smoke tool")) {
        const tools = parsed.tools || [];
        const hasExternalTool = tools.some((tool) => tool.function && tool.function.name === "smoke_add");
        if (!hasExternalTool) {
          sendAssistant("external-tool-not-forwarded", "chatcmpl_context_smoke_external_tool_missing");
          return;
        }
        sendToolCall("call_external_smoke_add", "smoke_add", { a: 21, b: 21 });
        return;
      }
      if (String(lastUserText).includes("call community smoke tool")) {
        const tools = parsed.tools || [];
        const hasCommunityTool = tools.some((tool) => tool.function && tool.function.name === "community_smoke");
        if (!hasCommunityTool) {
          sendAssistant("community-tool-not-forwarded", "chatcmpl_context_smoke_community_tool_missing");
          return;
        }
        sendToolCall("call_community_smoke", "community_smoke", { a: 21, b: 21 });
        return;
      }
      if (parsed.stream) {
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.write(`data: ${JSON.stringify({ choices: [{ delta: { content: "stream-" } }] })}\r\n\r\n`);
        res.write(`data: ${JSON.stringify({ choices: [{ delta: { content: "ok" } }], usage: { prompt_tokens: 11, completion_tokens: 2, total_tokens: 13 } })}\r\n\r\n`);
        res.end("data: [DONE]\r\n\r\n");
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({
        id: "chatcmpl_context_smoke",
        object: "chat.completion",
        created: Math.floor(Date.now() / 1000),
        model: "deepseek-v4-pro",
        choices: [
          {
            index: 0,
            message: { role: "assistant", content: "smoke-ok" },
            finish_reason: "stop"
          }
        ],
        usage: { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 }
      }));
    });
    return;
  }

  res.writeHead(404, { "content-type": "text/plain" });
  res.end(`not found ${req.method} ${req.url}`);
});

server.listen(port, "127.0.0.1", () => {
  console.log(`fake-upstream-ready:${port}`);
});

process.on("SIGTERM", () => server.close(() => process.exit(0)));
process.on("SIGINT", () => server.close(() => process.exit(0)));
'@ | Set-Content -LiteralPath $fakeServer -Encoding UTF8

$fake = $null
$proxy = $null

try {
  Write-Step "building proxy"
  $buildCommand = @(
    "set `"CARGO_HOME=$env:CARGO_HOME`"",
    "set `"CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR`"",
    "set `"TEMP=$env:TEMP`"",
    "set `"TMP=$env:TMP`"",
    "`"$VsDevCmd`" -arch=x64 >nul",
    "cd /d `"$RepoRoot`"",
    "`"$cargo`" build -p codeseex-proxy"
  ) -join " && "
  cmd /d /c $buildCommand
  if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
  }

  Write-Step "starting fake upstream on $FakeUpstreamPort"
  $env:PAYLOAD_FILE = $payloadFile
  $env:BALANCE_FILE = $balanceFile
  $env:FAKE_UPSTREAM_PORT = [string]$FakeUpstreamPort
  $fake = Start-Process -FilePath "node" -ArgumentList @($fakeServer) -PassThru -RedirectStandardOutput $fakeOut -RedirectStandardError $fakeErr -WindowStyle Hidden
  $fakeReady = Wait-TcpPort -HostName "127.0.0.1" -Port $FakeUpstreamPort -Retries 80 -DelayMs 100 -OnTick {
    if ($fake.HasExited) {
      throw "fake upstream exited early with code $($fake.ExitCode). stderr: $(Get-Content $fakeErr -Raw -ErrorAction SilentlyContinue)"
    }
    return $false
  }
  if (-not $fakeReady) {
    throw "fake upstream did not open port $FakeUpstreamPort"
  }

  Write-Step "starting proxy on $ProxyPort"
  $env:CODESEEX_DATA_DIR = $DataDir
  $env:CODESEEX_HOST = "127.0.0.1"
  $env:CODESEEX_PORT = [string]$ProxyPort
  $env:DEEPSEEK_BASE_URL = "http://127.0.0.1:$FakeUpstreamPort/v1"
  $env:DEEPSEEK_OFFICIAL_V1_COMPAT = "true"
  $env:DEEPSEEK_API_KEY = "test-key"
  [System.IO.File]::WriteAllText($authFile, (@{ OPENAI_API_KEY = "auth-json-test-key" } | ConvertTo-Json -Compress), $utf8NoBom)
  $env:CODEX_AUTH_JSON = $authFile
  $env:CODESEEX_WORKSPACE_ROOT = $RepoRoot
  $env:CODESEEX_WEB_SEARCH_ALLOW_PRIVATE = "true"
  $proxyExe = Join-Path $env:CARGO_TARGET_DIR "debug\codeseex-proxy.exe"
  $proxy = Start-Process -FilePath $proxyExe -PassThru -RedirectStandardOutput $proxyOut -RedirectStandardError $proxyErr -WindowStyle Hidden

  $proxyReady = $false
  foreach ($index in 1..80) {
    if ($proxy.HasExited) {
      throw "proxy exited early with code $($proxy.ExitCode). stderr: $(Get-Content $proxyErr -Raw -ErrorAction SilentlyContinue)"
    }
    try {
      Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/status" -TimeoutSec 1 | Out-Null
      $proxyReady = $true
      break
    } catch {
      Start-Sleep -Milliseconds 150
    }
  }
  if (-not $proxyReady) {
    throw "proxy did not become ready on $ProxyPort"
  }

  Write-Step "checking balance URL and auth.json key source"
  $balance = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/deepseek/balance" -TimeoutSec 15
  if (-not $balance.ok) {
    throw "balance query failed: $($balance | ConvertTo-Json -Compress)"
  }
  if (-not (Test-Path $balanceFile)) {
    throw "fake upstream did not receive balance request"
  }
  $balanceRequest = Get-Content $balanceFile -Raw | ConvertFrom-Json
  if ($balanceRequest.path -ne "/user/balance") {
    throw "balance URL should strip trailing /v1; got $($balanceRequest.path)"
  }
  if ($balanceRequest.authorization -ne "Bearer auth-json-test-key") {
    throw "balance query should use CODEX auth.json key; got $($balanceRequest.authorization)"
  }
  if ($balance.balance_infos[0].total_balance -ne "8.8") {
    throw "balance response was not normalized correctly"
  }

  Write-Step "checking catalog mode persistence"
  $catalogConfigBody = @{ CATALOG_MODE = "builtin" } | ConvertTo-Json -Depth 10
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/api/config" -Body $catalogConfigBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $catalogConfig = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/config" -TimeoutSec 15
  if ($catalogConfig.CATALOG_MODE -ne "builtin") {
    throw "catalog mode was not persisted; got $($catalogConfig.CATALOG_MODE)"
  }
  $adapterConfig = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/codex-adapter" -TimeoutSec 15
  if ($adapterConfig.catalog_mode -ne "builtin") {
    throw "catalog mode was not reflected by adapter endpoint; got $($adapterConfig.catalog_mode)"
  }

  Write-Step "checking tool registry and enabled tool persistence"
  $communityToolDir = Join-Path $DataDir "extension\tools\community-smoke"
  New-Item -ItemType Directory -Force -Path (Join-Path $communityToolDir "assets") | Out-Null
  [System.IO.File]::WriteAllText((Join-Path $communityToolDir "assets\icon.svg"), "<svg xmlns=`"http://www.w3.org/2000/svg`"></svg>", $utf8NoBom)
  $communityManifest = @{
    id = "community_smoke"
    name = "Community Smoke"
    description = "Community discovery smoke tool."
    model = @{
      description = "Add two integers and return the active community tool mode."
      parameters = @{
        type = "object"
        properties = @{
          a = @{ type = "integer" }
          b = @{ type = "integer" }
        }
        required = @("a", "b")
        additionalProperties = $false
      }
    }
    execution = @{
      type = "command"
      command = "node"
      args = @("tool.js")
      timeout_ms = 10000
    }
    config = @(
      @{
        key = "SMOKE_COMMUNITY_MODE"
        type = "select"
        label = "Mode"
        defaultValue = "safe"
        options = @(
          @{ value = "safe"; label = "Safe" }
          @{ value = "fast"; label = "Fast" }
        )
      }
    )
  } | ConvertTo-Json -Depth 20
  [System.IO.File]::WriteAllText((Join-Path $communityToolDir "manifest.json"), $communityManifest, $utf8NoBom)
  @'
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => input += chunk);
process.stdin.on("end", () => {
  const payload = JSON.parse(input || "{}");
  const args = payload.arguments || {};
  const settings = payload.settings || {};
  const sum = Number(args.a || 0) + Number(args.b || 0);
  process.stdout.write(JSON.stringify({
    ok: true,
    tool: "community_smoke",
    sum,
    mode: settings.SMOKE_COMMUNITY_MODE || "unset"
  }));
});
'@ | Set-Content -LiteralPath (Join-Path $communityToolDir "tool.js") -Encoding UTF8

  $toolsResponse = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/tools" -TimeoutSec 5
  $toolIds = @($toolsResponse.tools | ForEach-Object { $_.id })
  $applyPatchTool = @($toolsResponse.tools | Where-Object { $_.id -eq "apply_patch" } | Select-Object -First 1)[0]
  $webSearchTool = @($toolsResponse.tools | Where-Object { $_.id -eq "web_search" } | Select-Object -First 1)[0]
  $mcpServerTool = @($toolsResponse.tools | Where-Object { $_.id -eq "mcp_server" } | Select-Object -First 1)[0]
  $communityTool = @($toolsResponse.tools | Where-Object { $_.id -eq "community_smoke" } | Select-Object -First 1)[0]
  if (-not ($toolIds -contains "apply_patch") -or -not ($toolIds -contains "web_search")) {
    throw "tool registry did not expose expected system tools"
  }
  foreach ($systemTool in @($applyPatchTool, $webSearchTool, $mcpServerTool)) {
    if (-not $systemTool -or -not $systemTool.system -or $systemTool.configurable -or -not $systemTool.enabled) {
      throw "system tool registry entry was not non-configurable and always enabled"
    }
    $labelIds = @($systemTool.labels | ForEach-Object { $_.id })
    if (-not ($labelIds -contains "system") -or -not ($labelIds -contains "built_in")) {
      throw "system tool registry entry did not expose both system and built_in labels"
    }
  }
  if (-not $communityTool -or $communityTool.source -ne "community" -or $communityTool.enabled) {
    throw "community manifest was not discovered as a disabled community tool"
  }
  if (-not $communityTool.iconPath -or -not ($communityTool.config | Where-Object { $_.key -eq "SMOKE_COMMUNITY_MODE" })) {
    throw "community manifest icon/config metadata was not exposed safely"
  }
  Invoke-WebRequest -Method Get -Uri "http://127.0.0.1:$ProxyPort$($communityTool.iconPath)" -TimeoutSec 5 | Out-Null
  $communityDisabledBody = @{
    id = "resp_community_disabled_not_advertised"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "community tool should stay unadvertised" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $communityDisabledBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $communityDisabledPayload = (Get-Content $payloadFile -Raw | ConvertFrom-Json).body
  $communityDisabledAdvertised = @($communityDisabledPayload.tools | Where-Object { $_.function.name -eq "community_smoke" })
  if ($communityDisabledAdvertised.Count -gt 0) {
    throw "disabled community tool was advertised upstream"
  }
  $toolConfigBody = @{
    ENABLED_TOOLS = @("workspace_search", "list_directory", "read_file_range", "community_smoke")
    SMOKE_COMMUNITY_MODE = "fast"
  } | ConvertTo-Json -Depth 10
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/api/config" -Body $toolConfigBody -ContentType "application/json" -TimeoutSec 5 | Out-Null
  $savedConfig = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/config" -TimeoutSec 5
  $enabledToolIds = @($savedConfig.ENABLED_TOOLS)
  $enabledCommunityToolsResponse = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/tools" -TimeoutSec 5
  $enabledCommunityTool = @($enabledCommunityToolsResponse.tools | Where-Object { $_.id -eq "community_smoke" } | Select-Object -First 1)[0]
  $toolChecks = [ordered]@{
    tool_count = $toolsResponse.tools.Count
    has_apply_patch = $toolIds -contains "apply_patch"
    has_web_search = $toolIds -contains "web_search"
    has_mcp_server = $toolIds -contains "mcp_server"
    has_community_smoke = [bool]$communityTool
    community_default_disabled = -not [bool]$communityTool.enabled
    community_config_saved = $savedConfig.SMOKE_COMMUNITY_MODE -eq "fast"
    community_enabled_in_ui_registry = [bool]$enabledCommunityTool.enabled
    apply_patch_system = [bool]$applyPatchTool.system
    web_search_system = [bool]$webSearchTool.system
    mcp_server_system = [bool]$mcpServerTool.system
    saved_enabled_tools = $enabledToolIds
  }
  $toolChecks | ConvertTo-Json -Depth 10
  if (($enabledToolIds -contains "web_search") -or ($enabledToolIds -contains "apply_patch") -or ($enabledToolIds -contains "mcp_server")) {
    throw "system tools should not be persisted in ENABLED_TOOLS"
  }
  if (-not ($enabledToolIds -contains "workspace_search") -or -not ($enabledToolIds -contains "list_directory") -or -not ($enabledToolIds -contains "read_file_range") -or -not ($enabledToolIds -contains "community_smoke")) {
    throw "enabled tool ids were not persisted through api/config"
  }
  if (-not $toolChecks.community_config_saved -or -not $toolChecks.community_enabled_in_ui_registry) {
    throw "community tool config or enabled state was not persisted through api/config"
  }
  $communityToolBody = @{
    id = "resp_community_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call community smoke tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $communityToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $communityToolBody -ContentType "application/json" -TimeoutSec 15
  $communityToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=80" -TimeoutSec 5
  $communityToolResultEvent = @($communityToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "community_smoke" } | Select-Object -Last 1)[0]
  $communityToolExecutionChecks = [ordered]@{
    response_output = $communityToolResponse.output[0].content[0].text
    tool_result_seen = [bool]$communityToolResultEvent
    tool_result_ok = if ($communityToolResultEvent) { [bool]$communityToolResultEvent.detail.ok } else { $false }
    summary_has_mode = if ($communityToolResultEvent) { [string]$communityToolResultEvent.detail.summary -like "*fast*" } else { $false }
  }
  $communityToolExecutionChecks | ConvertTo-Json -Depth 10
  if ($communityToolExecutionChecks.response_output -ne "community-tool-ok" -or -not $communityToolExecutionChecks.tool_result_seen -or -not $communityToolExecutionChecks.tool_result_ok -or -not $communityToolExecutionChecks.summary_has_mode) {
    throw "community tool execution loop did not run through the isolated command executor"
  }
  $streamCommunityToolBody = @{
    id = "resp_stream_community_tool_loop"
    model = "deepseek-v4-pro"
    stream = $true
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call community smoke tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $streamCommunityToolRaw = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $streamCommunityToolBody -ContentType "application/json" -TimeoutSec 15
  $streamCommunityToolText = [string]$streamCommunityToolRaw.Content
  $streamCommunityChecks = [ordered]@{
    stream_has_function_call_item = $streamCommunityToolText.Contains("response.function_call_arguments.done")
    stream_has_final_text = $streamCommunityToolText.Contains("community-tool-ok")
  }
  $streamCommunityChecks | ConvertTo-Json -Depth 10
  if (-not $streamCommunityChecks.stream_has_function_call_item -or -not $streamCommunityChecks.stream_has_final_text) {
    throw "streaming community tool execution loop did not emit function call and final text"
  }
  Write-Step "checking unknown Codex model fallback and duplicate tool deconfliction"
  $fallbackModelBody = @{
    id = "resp_model_fallback"
    model = "gpt-5.4-mini"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "model fallback please" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $fallbackModelBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $fallbackPayload = (Get-Content $payloadFile -Raw | ConvertFrom-Json).body
  if ($fallbackPayload.model -ne "deepseek-v4-pro") {
    throw "unknown Codex model was not mapped to deepseek-v4-pro in default mode"
  }
  $duplicateApplyPatchToolBody = @{
    id = "resp_duplicate_apply_patch_tool"
    model = "deepseek-v4-pro"
    tools = @(
      @{
        type = "function"
        name = "apply_patch"
        description = "Codex-provided apply_patch declaration."
        parameters = @{ type = "object"; properties = @{} }
      }
    )
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "duplicate apply patch declaration should not break request" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $duplicateApplyPatchToolBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $duplicateToolPayload = (Get-Content $payloadFile -Raw | ConvertFrom-Json).body
  $duplicateToolNames = @($duplicateToolPayload.tools | ForEach-Object { $_.function.name })
  $applyPatchCount = @($duplicateToolNames | Where-Object { $_ -eq "apply_patch" }).Count
  if ($applyPatchCount -ne 1) {
    throw "apply_patch tool declaration was not de-duplicated before upstream request"
  }

  Write-Step "checking Responses stream shape and final-answer phase"
  $streamShapeBody = @{
    id = "resp_stream_shape_plain"
    model = "deepseek-v4-pro"
    stream = $true
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "plain streaming response shape" }) }
    )
  } | ConvertTo-Json -Depth 20
  $streamShapeRaw = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $streamShapeBody -ContentType "application/json" -TimeoutSec 15
  $streamShapeText = [string]$streamShapeRaw.Content
  $streamShapeEvents = @()
  foreach ($line in ($streamShapeText -split "`r?`n")) {
    if (-not $line.StartsWith("data: ")) {
      continue
    }
    $data = $line.Substring(6)
    if ($data -eq "[DONE]") {
      continue
    }
    $streamShapeEvents += ($data | ConvertFrom-Json)
  }
  $streamShapeAdded = @($streamShapeEvents | Where-Object { $_.type -eq "response.output_item.added" -and $_.item.type -eq "message" } | Select-Object -First 1)[0]
  $streamShapeDone = @($streamShapeEvents | Where-Object { $_.type -eq "response.output_item.done" -and $_.item.type -eq "message" } | Select-Object -First 1)[0]
  $streamShapeCompleted = @($streamShapeEvents | Where-Object { $_.type -eq "response.completed" })
  $streamShapeCompletedOutput = @($streamShapeCompleted[0].response.output)
  $streamShapePayload = (Get-Content $payloadFile -Raw | ConvertFrom-Json).body
  $streamShapeChecks = [ordered]@{
    completed_count = $streamShapeCompleted.Count
    output_count = $streamShapeCompletedOutput.Count
    added_phase = if ($streamShapeAdded) { $streamShapeAdded.item.phase } else { "" }
    done_phase = if ($streamShapeDone) { $streamShapeDone.item.phase } else { "" }
    completed_phase = if ($streamShapeCompletedOutput.Count -gt 0) { $streamShapeCompletedOutput[0].phase } else { "" }
    done_id = if ($streamShapeDone) { $streamShapeDone.item.id } else { "" }
    completed_id = if ($streamShapeCompletedOutput.Count -gt 0) { $streamShapeCompletedOutput[0].id } else { "" }
    final_text = if ($streamShapeCompletedOutput.Count -gt 0) { $streamShapeCompletedOutput[0].content[0].text } else { "" }
    upstream_requested_usage = [bool]$streamShapePayload.stream_options.include_usage
  }
  $streamShapeChecks | ConvertTo-Json -Depth 10
  if ($streamShapeChecks.completed_count -ne 1) {
    throw "streaming response did not emit exactly one response.completed event"
  }
  if ($streamShapeChecks.output_count -ne 1 -or $streamShapeChecks.done_phase -ne "final_answer" -or $streamShapeChecks.completed_phase -ne "final_answer") {
    throw "streaming final answer did not preserve final_answer phase"
  }
  if ($streamShapeChecks.done_id -ne $streamShapeChecks.completed_id) {
    throw "streaming completed response did not reuse the streamed message item id"
  }
  if ($streamShapeChecks.final_text -ne "stream-ok") {
    throw "streaming final response text was not reconstructed correctly"
  }
  if (-not $streamShapeChecks.upstream_requested_usage) {
    throw "streaming upstream payload did not request usage with stream_options.include_usage"
  }

  Write-Step "checking disabled built-in tool calls are blocked"
  $disabledConfigBody = @{ ENABLED_TOOLS = @("list_directory") } | ConvertTo-Json -Depth 10
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/api/config" -Body $disabledConfigBody -ContentType "application/json" -TimeoutSec 5 | Out-Null
  $disabledToolsResponse = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/tools" -TimeoutSec 5
  $disabledReadTool = @($disabledToolsResponse.tools | Where-Object { $_.id -eq "read_file_range" } | Select-Object -First 1)[0]
  if ($disabledReadTool.enabled) {
    throw "tool registry reported disabled read_file_range as enabled"
  }
  $disabledToolBody = @{
    id = "resp_disabled_tool"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call disabled read tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $disabledToolRejected = $false
  try {
    Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $disabledToolBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  } catch {
    $disabledToolRejected = $true
  }
  $restoreToolConfigBody = @{ ENABLED_TOOLS = @("workspace_search", "list_directory", "read_file_range") } | ConvertTo-Json -Depth 10
  if (-not $disabledToolRejected) {
    throw "disabled read_file_range tool call unexpectedly executed"
  }
  $disabledStreamToolBody = @{
    id = "resp_disabled_stream_tool"
    model = "deepseek-v4-pro"
    stream = $true
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call disabled read tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $disabledStreamRaw = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $disabledStreamToolBody -ContentType "application/json" -TimeoutSec 15
  $disabledStreamText = [string]$disabledStreamRaw.Content
  if (-not $disabledStreamText.Contains("response.failed") -or $disabledStreamText.Contains("response.function_call_arguments.done")) {
    throw "disabled streaming tool call was not rejected before emitting a function call item"
  }

  Write-Step "checking partial config save preserves tool enablement"
  $partialConfigBody = @{ UI_THEME = "dark" } | ConvertTo-Json -Depth 10
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/api/config" -Body $partialConfigBody -ContentType "application/json" -TimeoutSec 5 | Out-Null
  $partialConfig = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/config" -TimeoutSec 5
  $partialEnabledToolIds = @($partialConfig.ENABLED_TOOLS)
  if ($partialEnabledToolIds.Count -ne 1 -or -not ($partialEnabledToolIds -contains "list_directory")) {
    throw "partial config save did not preserve the disabled tool set"
  }
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/api/config" -Body $restoreToolConfigBody -ContentType "application/json" -TimeoutSec 5 | Out-Null

  Write-Step "sending parent Responses request with verified tool facts"
  $parentBody = @{
    id = "resp_fact_parent"
    model = "deepseek-v4-pro"
    input = @(
      @{ type = "function_call"; call_id = "call_1"; name = "list_files"; arguments = '{ "path": "." }' },
      @{ type = "function_call_output"; call_id = "call_1"; output = "Cargo.toml`nREADME.md" },
      @{ type = "function_call_output"; call_id = "call_image"; output = "screenshot=data:image/png;base64,AAAAAAAAAABBBBBBBBBB" },
      @{ role = "user"; content = @(@{ type = "input_text"; text = "what happened?" }) }
    )
  } | ConvertTo-Json -Depth 20

  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $parentBody -ContentType "application/json" -TimeoutSec 15 | Out-Null

  Write-Step "sending child Responses request with previous_response_id and instructions"
  $childBody = @{
    id = "resp_fact_child"
    previous_response_id = "resp_fact_parent"
    model = "deepseek-v4-pro"
    instructions = "You are a context fidelity smoke test."
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "what happened in previous turn?" }) }
    )
  } | ConvertTo-Json -Depth 20

  $response = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $childBody -ContentType "application/json" -TimeoutSec 15
  if (-not (Test-Path $payloadFile)) {
    throw "fake upstream did not receive a payload"
  }

  $received = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $payload = $received.body
  $events = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=20" -TimeoutSec 5
  $firstMessage = $payload.messages[0]
  $lastMessage = $payload.messages[$payload.messages.Count - 1]
  $allContent = ($payload.messages | ForEach-Object { $_.content }) -join "`n"
  $requestStarted = @($events.events | Where-Object { $_.type -eq "request_started" } | Select-Object -Last 1)[0]

  $checks = [ordered]@{
    response_output = $response.output[0].content[0].text
    upstream_path = $received.path
    upstream_message_count = $payload.messages.Count
    first_role = $firstMessage.role
    first_contains_instructions = $firstMessage.content.Contains("context fidelity smoke test")
    history_contains_function_call_output = $allContent.Contains("function_call_output")
    history_contains_cargo_toml = $allContent.Contains("Cargo.toml")
    history_contains_inline_data_redaction = $allContent.Contains("[inline-data-url omitted")
    history_omits_inline_data_payload = -not $allContent.Contains("AAAAAAAAAABBBBBBBBBB")
    history_contains_parent_assistant = $allContent.Contains("smoke-ok")
    last_role = $lastMessage.role
    last_content = $lastMessage.content
    diagnostic_instruction_messages = $requestStarted.detail.context.instruction_messages
    diagnostic_history_messages = $requestStarted.detail.context.history_messages
    diagnostic_verified_fact_items = $requestStarted.detail.context.current_input.verified_fact_items
    diagnostic_message_items = $requestStarted.detail.context.current_input.message_items
  }

  $checks | ConvertTo-Json -Depth 10

  if ($checks.response_output -ne "smoke-ok") {
    throw "unexpected response output"
  }
  if ($checks.first_role -ne "system") {
    throw "first upstream message was not the instructions system message"
  }
  if (-not $checks.first_contains_instructions) {
    throw "top-level instructions were not preserved in the upstream payload"
  }
  if (-not $checks.history_contains_function_call_output -or -not $checks.history_contains_cargo_toml) {
    throw "history verified tool facts were not preserved in the upstream payload"
  }
  if (-not $checks.history_contains_parent_assistant) {
    throw "completed parent assistant output was not preserved in history"
  }
  if (-not $checks.history_contains_inline_data_redaction -or -not $checks.history_omits_inline_data_payload) {
    throw "inline data URL was not redacted from the upstream payload"
  }
  if ($checks.last_role -ne "user" -or $checks.last_content -ne "what happened in previous turn?") {
    throw "current user message was not preserved as the final upstream message"
  }
  if ([int]$checks.diagnostic_instruction_messages -ne 1 -or [int]$checks.diagnostic_history_messages -lt 3) {
    throw "context diagnostic did not record instructions and reconstructed history"
  }
  if ([int]$checks.diagnostic_verified_fact_items -ne 0) {
    throw "child current input diagnostic should not contain verified fact items"
  }

  Write-Step "checking native MCP/external tool passthrough"
  $externalToolBody = @{
    id = "resp_external_tool_parent"
    model = "deepseek-v4-pro"
    tools = @(
      @{
        type = "mcp"
        name = "smoke_server"
        tools = @(
          @{
            name = "smoke_add"
            description = "Add two numbers"
            input_schema = @{
              type = "object"
              properties = @{
                a = @{ type = "integer" }
                b = @{ type = "integer" }
              }
              required = @("a", "b")
            }
          }
        )
      }
    )
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call external mcp smoke tool" }) }
    )
  } | ConvertTo-Json -Depth 30
  $externalToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $externalToolBody -ContentType "application/json" -TimeoutSec 15
  $externalToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=120" -TimeoutSec 5
  $externalToolPayload = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $externalCallItem = @($externalToolResponse.output | Where-Object { $_.type -eq "function_call" } | Select-Object -First 1)[0]
  $externalToolResultEvent = @($externalToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "smoke_add" } | Select-Object -First 1)[0]
  $externalToolChecks = [ordered]@{
    upstream_received_smoke_tool = [bool](@($externalToolPayload.body.tools | Where-Object { $_.function.name -eq "smoke_add" } | Select-Object -First 1)[0])
    response_has_function_call = [bool]$externalCallItem
    call_name = if ($externalCallItem) { $externalCallItem.name } else { "" }
    call_namespace = if ($externalCallItem) { $externalCallItem.namespace } else { "" }
    call_arguments = if ($externalCallItem) { $externalCallItem.arguments } else { "" }
    proxy_did_not_execute = -not [bool]$externalToolResultEvent
  }
  $externalToolChecks | ConvertTo-Json -Depth 10
  if (-not $externalToolChecks.upstream_received_smoke_tool -or -not $externalToolChecks.response_has_function_call -or $externalToolChecks.call_name -ne "smoke_add" -or $externalToolChecks.call_namespace -ne "smoke_server" -or -not $externalToolChecks.call_arguments.Contains("21") -or -not $externalToolChecks.proxy_did_not_execute) {
    throw "native MCP/external tool passthrough did not return a Codex function_call without proxy execution"
  }

  Write-Step "checking native MCP/external tool result replay"
  $externalToolChildBody = @{
    id = "resp_external_tool_child"
    previous_response_id = "resp_external_tool_parent"
    model = "deepseek-v4-pro"
    input = @(
      @{ type = "function_call_output"; call_id = "call_external_smoke_add"; output = "42" },
      @{ role = "user"; content = @(@{ type = "input_text"; text = "continue after external tool result" }) }
    )
  } | ConvertTo-Json -Depth 20
  $externalToolChildResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $externalToolChildBody -ContentType "application/json" -TimeoutSec 15
  $externalToolChildPayload = (Get-Content $payloadFile -Raw | ConvertFrom-Json).body
  $externalAssistantToolMessage = @($externalToolChildPayload.messages | Where-Object { $_.role -eq "assistant" -and $_.tool_calls } | Select-Object -First 1)[0]
  $externalToolMessage = @($externalToolChildPayload.messages | Where-Object { $_.role -eq "tool" -and $_.tool_call_id -eq "call_external_smoke_add" } | Select-Object -First 1)[0]
  $externalToolReplayChecks = [ordered]@{
    response_output = $externalToolChildResponse.output[0].content[0].text
    has_assistant_tool_calls = [bool]$externalAssistantToolMessage
    has_tool_result_message = [bool]$externalToolMessage
    tool_result_content = if ($externalToolMessage) { $externalToolMessage.content } else { "" }
  }
  $externalToolReplayChecks | ConvertTo-Json -Depth 10
  if ($externalToolReplayChecks.response_output -ne "external-tool-result-ok" -or -not $externalToolReplayChecks.has_assistant_tool_calls -or -not $externalToolReplayChecks.has_tool_result_message -or $externalToolReplayChecks.tool_result_content -ne "42") {
    throw "native MCP/external tool result was not replayed as a legal Chat tool pair"
  }

  Write-Step "checking failed parent history reconstruction"
  $failedParentBody = @{
    id = "resp_failed_parent"
    model = "deepseek-v4-pro"
    input = @(
      @{ type = "function_call_output"; call_id = "call_failed"; output = "FAILED_FACT_42" },
      @{ role = "user"; content = @(@{ type = "input_text"; text = "force upstream failure" }) }
    )
  } | ConvertTo-Json -Depth 20

  $failedParentRejected = $false
  try {
    Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $failedParentBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  } catch {
    $failedParentRejected = $true
  }
  if (-not $failedParentRejected) {
    throw "failed parent smoke request unexpectedly succeeded"
  }

  $failedChildBody = @{
    id = "resp_failed_child"
    previous_response_id = "resp_failed_parent"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "recover failed history" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $failedChildBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $failedReceived = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $failedPayload = $failedReceived.body
  $failedContent = ($failedPayload.messages | ForEach-Object { $_.content }) -join "`n"
  $failedChecks = [ordered]@{
    upstream_message_count = $failedPayload.messages.Count
    contains_failed_fact = $failedContent.Contains("FAILED_FACT_42")
    omits_failed_parent_assistant = -not $failedContent.Contains("forced smoke failure")
    last_content = $failedPayload.messages[$failedPayload.messages.Count - 1].content
  }
  $failedChecks | ConvertTo-Json -Depth 10
  if (-not $failedChecks.contains_failed_fact -or -not $failedChecks.omits_failed_parent_assistant) {
    throw "failed parent history was not reconstructed safely"
  }
  if ($failedChecks.last_content -ne "recover failed history") {
    throw "failed child current user message was not preserved"
  }

  Write-Step "checking streaming parent final response persistence"
  $streamParentBody = @{
    id = "resp_stream_parent"
    model = "deepseek-v4-pro"
    stream = $true
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "stream parent please" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $streamParentBody -ContentType "application/json" -TimeoutSec 15 | Out-Null

  $streamChildBody = @{
    id = "resp_stream_child"
    previous_response_id = "resp_stream_parent"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "recover streaming history" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $streamChildBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $streamReceived = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $streamPayload = $streamReceived.body
  $streamContent = ($streamPayload.messages | ForEach-Object { $_.content }) -join "`n"
  $streamChecks = [ordered]@{
    upstream_message_count = $streamPayload.messages.Count
    contains_stream_parent_user = $streamContent.Contains("stream parent please")
    contains_stream_parent_assistant = $streamContent.Contains("stream-ok")
    last_content = $streamPayload.messages[$streamPayload.messages.Count - 1].content
  }
  $streamChecks | ConvertTo-Json -Depth 10
  if (-not $streamChecks.contains_stream_parent_user -or -not $streamChecks.contains_stream_parent_assistant) {
    throw "streaming parent history was not persisted and reconstructed"
  }
  if ($streamChecks.last_content -ne "recover streaming history") {
    throw "stream child current user message was not preserved"
  }

  Write-Step "checking manual context compaction"
  $compactBody = @{
    model = "deepseek-v4-pro"
    previous_response_id = "resp_fact_child"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "please compact this context" }) }
    )
  } | ConvertTo-Json -Depth 20
  $compactResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses/compact" -Headers @{ Authorization = "Bearer codex-test" } -Body $compactBody -ContentType "application/json" -TimeoutSec 15
  $compactItem = $compactResponse.output[0]
  $compactSummary = $compactItem.summary[0].text
  $compactChecks = [ordered]@{
    response_status = $compactResponse.status
    output_type = $compactItem.type
    has_summary = $compactSummary.Contains("CodeSeeX compacted context")
    has_cargo_fact = $compactSummary.Contains("Cargo.toml")
    omits_encrypted_content = -not ($compactItem.PSObject.Properties.Name -contains "encrypted_content")
  }
  $compactChecks | ConvertTo-Json -Depth 10
  if ($compactChecks.response_status -ne "completed" -or $compactChecks.output_type -ne "compaction") {
    throw "manual compaction did not return a completed compaction item"
  }
  if (-not $compactChecks.has_summary -or -not $compactChecks.has_cargo_fact -or -not $compactChecks.omits_encrypted_content) {
    throw "manual compaction summary was incomplete or used fake encrypted content"
  }

  $compactChildBody = @{
    id = "resp_compact_child"
    previous_response_id = $compactResponse.id
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "recover compacted history" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $compactChildBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $compactReceived = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $compactPayload = $compactReceived.body
  $compactContent = ($compactPayload.messages | ForEach-Object { $_.content }) -join "`n"
  $compactChildChecks = [ordered]@{
    upstream_message_count = $compactPayload.messages.Count
    contains_recovered_compaction = $compactContent.Contains("Recovered CodeSeeX compaction summary")
    contains_cargo_fact = $compactContent.Contains("Cargo.toml")
    last_content = $compactPayload.messages[$compactPayload.messages.Count - 1].content
  }
  $compactChildChecks | ConvertTo-Json -Depth 10
  if (-not $compactChildChecks.contains_recovered_compaction -or -not $compactChildChecks.contains_cargo_fact) {
    throw "compaction response was not reconstructed into later history"
  }
  if ($compactChildChecks.last_content -ne "recover compacted history") {
    throw "compact child current user message was not preserved"
  }

  Write-Step "checking built-in tool call loop"
  $toolLoopBody = @{
    id = "resp_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call list_directory tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $toolLoopResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $toolLoopBody -ContentType "application/json" -TimeoutSec 15
  $toolLoopEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=80" -TimeoutSec 5
  $toolCallEvent = @($toolLoopEvents.events | Where-Object { $_.type -eq "tool_call" -and $_.detail.name -eq "list_directory" } | Select-Object -Last 1)[0]
  $toolResultEvent = @($toolLoopEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "list_directory" } | Select-Object -Last 1)[0]
  $toolLoopChecks = [ordered]@{
    response_output = $toolLoopResponse.output[0].content[0].text
    tool_call_event_seen = [bool]$toolCallEvent
    tool_result_event_seen = [bool]$toolResultEvent
    tool_result_ok = if ($toolResultEvent) { [bool]$toolResultEvent.detail.ok } else { $false }
  }
  $toolLoopChecks | ConvertTo-Json -Depth 10
  if ($toolLoopChecks.response_output -ne "tool-loop-ok" -or -not $toolLoopChecks.tool_call_event_seen -or -not $toolLoopChecks.tool_result_event_seen -or -not $toolLoopChecks.tool_result_ok) {
    throw "built-in tool call loop did not execute list_directory successfully"
  }

  Write-Step "checking persisted built-in tool facts in later history"
  $toolHistoryBody = @{
    id = "resp_tool_history_child"
    previous_response_id = "resp_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "recover built-in tool facts" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $toolHistoryBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $toolHistoryReceived = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $toolHistoryPayload = $toolHistoryReceived.body
  $toolHistoryContent = ($toolHistoryPayload.messages | ForEach-Object { $_.content }) -join "`n"
  $toolHistoryChecks = [ordered]@{
    contains_verified_tool_facts = $toolHistoryContent.Contains("Verified CodeSeeX tool execution facts")
    contains_list_directory_fact = $toolHistoryContent.Contains("list_directory")
    contains_cargo_toml = $toolHistoryContent.Contains("Cargo.toml")
    last_content = $toolHistoryPayload.messages[$toolHistoryPayload.messages.Count - 1].content
  }
  $toolHistoryChecks | ConvertTo-Json -Depth 10
  if (-not $toolHistoryChecks.contains_verified_tool_facts -or -not $toolHistoryChecks.contains_list_directory_fact -or -not $toolHistoryChecks.contains_cargo_toml) {
    throw "built-in tool facts were not persisted into later history"
  }
  if ($toolHistoryChecks.last_content -ne "recover built-in tool facts") {
    throw "tool history child current user message was not preserved"
  }

  Write-Step "checking read_file_range tool loop"
  $readToolBody = @{
    id = "resp_read_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call read_file_range tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $readToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $readToolBody -ContentType "application/json" -TimeoutSec 15
  $readToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=80" -TimeoutSec 5
  $readToolResultEvent = @($readToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "read_file_range" } | Select-Object -Last 1)[0]
  $readToolChecks = [ordered]@{
    response_output = $readToolResponse.output[0].content[0].text
    tool_result_seen = [bool]$readToolResultEvent
    tool_result_ok = if ($readToolResultEvent) { [bool]$readToolResultEvent.detail.ok } else { $false }
  }
  $readToolChecks | ConvertTo-Json -Depth 10
  if ($readToolChecks.response_output -ne "read-tool-ok" -or -not $readToolChecks.tool_result_seen -or -not $readToolChecks.tool_result_ok) {
    throw "built-in tool call loop did not execute read_file_range successfully"
  }

  Write-Step "checking tool result redaction before upstream replay"
  $dataUrlReadToolBody = @{
    id = "resp_data_url_read_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call data_url read_file_range tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $dataUrlReadToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $dataUrlReadToolBody -ContentType "application/json" -TimeoutSec 15
  $dataUrlReadToolChecks = [ordered]@{
    response_output = $dataUrlReadToolResponse.output[0].content[0].text
  }
  $dataUrlReadToolChecks | ConvertTo-Json -Depth 10
  if ($dataUrlReadToolChecks.response_output -ne "data-url-redacted-ok") {
    throw "tool result replay leaked inline data URL content upstream"
  }

  Write-Step "checking workspace_search tool loop"
  $searchToolBody = @{
    id = "resp_search_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call workspace_search tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $searchToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $searchToolBody -ContentType "application/json" -TimeoutSec 15
  $searchToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=80" -TimeoutSec 5
  $searchToolResultEvent = @($searchToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "workspace_search" } | Select-Object -Last 1)[0]
  $searchToolChecks = [ordered]@{
    response_output = $searchToolResponse.output[0].content[0].text
    tool_result_seen = [bool]$searchToolResultEvent
    tool_result_ok = if ($searchToolResultEvent) { [bool]$searchToolResultEvent.detail.ok } else { $false }
  }
  $searchToolChecks | ConvertTo-Json -Depth 10
  if ($searchToolChecks.response_output -ne "search-tool-ok" -or -not $searchToolChecks.tool_result_seen -or -not $searchToolChecks.tool_result_ok) {
    throw "built-in tool call loop did not execute workspace_search successfully"
  }

  Write-Step "checking web_search tool loop"
  $webSearchToolBody = @{
    id = "resp_web_search_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call web_search tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $webSearchToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $webSearchToolBody -ContentType "application/json" -TimeoutSec 15
  $webSearchToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=100" -TimeoutSec 5
  $webSearchToolResultEvent = @($webSearchToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "web_search" } | Select-Object -Last 1)[0]
  $webSearchToolChecks = [ordered]@{
    response_output = $webSearchToolResponse.output[0].content[0].text
    tool_result_seen = [bool]$webSearchToolResultEvent
    tool_result_ok = if ($webSearchToolResultEvent) { [bool]$webSearchToolResultEvent.detail.ok } else { $false }
    summary_has_fixture = if ($webSearchToolResultEvent) { [string]$webSearchToolResultEvent.detail.summary -like "*WEB_SEARCH_FIXTURE_OK*" } else { $false }
    summary_omits_script = if ($webSearchToolResultEvent) { -not ([string]$webSearchToolResultEvent.detail.summary -like "*window.noise*") } else { $false }
  }
  $webSearchToolChecks | ConvertTo-Json -Depth 10
  if ($webSearchToolChecks.response_output -ne "web-search-tool-ok" -or -not $webSearchToolChecks.tool_result_seen -or -not $webSearchToolChecks.tool_result_ok -or -not $webSearchToolChecks.summary_has_fixture -or -not $webSearchToolChecks.summary_omits_script) {
    throw "built-in tool call loop did not execute web_search successfully"
  }

  Write-Step "checking apply_patch tool loop"
  $applyPatchToolBody = @{
    id = "resp_apply_patch_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call apply_patch tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $applyPatchToolResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $applyPatchToolBody -ContentType "application/json" -TimeoutSec 15
  $applyPatchToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=140" -TimeoutSec 5
  $applyPatchToolResultEvent = @($applyPatchToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "apply_patch" } | Select-Object -Last 1)[0]
  $applyPatchToolChecks = [ordered]@{
    response_output = $applyPatchToolResponse.output[0].content[0].text
    file_content = (Get-Content -LiteralPath $applyPatchFixture -Raw).Trim()
    tool_result_seen = [bool]$applyPatchToolResultEvent
    tool_result_ok = if ($applyPatchToolResultEvent) { [bool]$applyPatchToolResultEvent.detail.ok } else { $false }
  }
  $applyPatchToolChecks | ConvertTo-Json -Depth 10
  if ($applyPatchToolChecks.response_output -ne "apply-patch-tool-ok" -or $applyPatchToolChecks.file_content -ne "after" -or -not $applyPatchToolChecks.tool_result_seen -or -not $applyPatchToolChecks.tool_result_ok) {
    throw "apply_patch tool loop did not execute successfully"
  }

  Write-Step "checking streaming built-in tool call loop"
  $streamToolBody = @{
    id = "resp_stream_tool_loop"
    model = "deepseek-v4-pro"
    stream = $true
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "call list_directory tool" }) }
    )
  } | ConvertTo-Json -Depth 20
  $streamToolRaw = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $streamToolBody -ContentType "application/json" -TimeoutSec 15
  $streamToolEvents = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$ProxyPort/api/events?limit=120" -TimeoutSec 5
  $streamToolText = [string]$streamToolRaw.Content
  $streamToolResultEvent = @($streamToolEvents.events | Where-Object { $_.type -eq "tool_result" -and $_.detail.name -eq "list_directory" -and $_.detail.id -eq "resp_stream_tool_loop" } | Select-Object -Last 1)[0]
  $streamToolChildBody = @{
    id = "resp_stream_tool_history_child"
    previous_response_id = "resp_stream_tool_loop"
    model = "deepseek-v4-pro"
    input = @(
      @{ role = "user"; content = @(@{ type = "input_text"; text = "recover streaming built-in tool facts" }) }
    )
  } | ConvertTo-Json -Depth 20
  Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$ProxyPort/v1/responses" -Headers @{ Authorization = "Bearer codex-test" } -Body $streamToolChildBody -ContentType "application/json" -TimeoutSec 15 | Out-Null
  $streamToolReceived = Get-Content $payloadFile -Raw | ConvertFrom-Json
  $streamToolPayload = $streamToolReceived.body
  $streamToolContent = ($streamToolPayload.messages | ForEach-Object { $_.content }) -join "`n"
  $streamToolChecks = [ordered]@{
    stream_has_function_call_item = $streamToolText.Contains("response.function_call_arguments.done")
    stream_has_final_text = $streamToolText.Contains("tool-loop-ok")
    tool_result_seen = [bool]$streamToolResultEvent
    tool_result_ok = if ($streamToolResultEvent) { [bool]$streamToolResultEvent.detail.ok } else { $false }
    history_contains_tool_fact = $streamToolContent.Contains("Verified CodeSeeX tool execution facts") -and $streamToolContent.Contains("list_directory") -and $streamToolContent.Contains("Cargo.toml")
  }
  $streamToolChecks | ConvertTo-Json -Depth 10
  if (-not $streamToolChecks.stream_has_function_call_item -or -not $streamToolChecks.stream_has_final_text -or -not $streamToolChecks.tool_result_seen -or -not $streamToolChecks.tool_result_ok -or -not $streamToolChecks.history_contains_tool_fact) {
    throw "streaming built-in tool call loop did not execute and persist facts successfully"
  }

  Write-Step "passed"
} finally {
  if ($proxy -and -not $proxy.HasExited) {
    Stop-Process -Id $proxy.Id -Force -ErrorAction SilentlyContinue
  }
  if ($fake -and -not $fake.HasExited) {
    Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue
  }
  Remove-Item -LiteralPath $dataUrlFixture -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $applyPatchFixture -Force -ErrorAction SilentlyContinue
}
