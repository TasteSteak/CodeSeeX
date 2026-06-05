param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$DevRoot = "D:\DevTools\CodeSeeXNext",
  [string]$DataDir = "D:\DevTools\CodeSeeXNext\Data",
  [int]$Port = 8787,
  [string]$UpstreamBaseUrl = "",
  [switch]$NoBuild
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot

function Import-CodeSeeXDotEnv {
  param([string]$Path)

  if (-not (Test-Path -LiteralPath $Path)) {
    return
  }

  Get-Content -LiteralPath $Path | ForEach-Object {
    $line = $_.Trim()
    if (-not $line -or $line.StartsWith("#") -or -not $line.Contains("=")) {
      return
    }
    $parts = $line.Split("=", 2)
    $name = $parts[0].Trim()
    $value = $parts[1].Trim()
    if (-not $name -or [Environment]::GetEnvironmentVariable($name, "Process")) {
      return
    }
    if (
      ($value.StartsWith('"') -and $value.EndsWith('"')) -or
      ($value.StartsWith("'") -and $value.EndsWith("'"))
    ) {
      $value = $value.Substring(1, $value.Length - 2)
    }
    [Environment]::SetEnvironmentVariable($name, $value, "Process")
  }
}

Import-CodeSeeXDotEnv (Join-Path $RepoRoot ".env")

$env:CARGO_HOME = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $DevRoot "Cargo" }
$env:CARGO_TARGET_DIR = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $DevRoot "CargoTarget" }
$env:TEMP = Join-Path $DevRoot "Temp"
$env:TMP = $env:TEMP
$env:CODESEEX_DATA_DIR = $DataDir
$env:CODESEEX_PORT = [string]$Port
if ($UpstreamBaseUrl) {
  $env:DEEPSEEK_BASE_URL = $UpstreamBaseUrl
}

New-Item -ItemType Directory -Force -Path $env:CARGO_HOME, $env:CARGO_TARGET_DIR, $env:TEMP, $env:CODESEEX_DATA_DIR | Out-Null

if (-not $VsDevCmd) {
  $defaultVsDevCmd = Join-Path $DevRoot "VSBuildTools\Common7\Tools\VsDevCmd.bat"
  if (Test-Path $defaultVsDevCmd) {
    $VsDevCmd = $defaultVsDevCmd
  }
}

if (-not $VsDevCmd -or -not (Test-Path $VsDevCmd)) {
  throw "MSVC Build Tools not found. Run scripts/check-windows.ps1 after installing Build Tools."
}

$cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
if (-not (Test-Path $cargo)) {
  $cargo = "cargo"
}

function Get-LatestProxyInputWriteTime {
  $paths = @(
    (Join-Path $RepoRoot "Cargo.toml"),
    (Join-Path $RepoRoot "Cargo.lock"),
    (Join-Path $RepoRoot "crates\core"),
    (Join-Path $RepoRoot "crates\store"),
    (Join-Path $RepoRoot "crates\proxy")
  )
  $latest = [DateTime]::MinValue
  foreach ($path in $paths) {
    if (-not (Test-Path $path)) {
      continue
    }
    $item = Get-Item -LiteralPath $path
    if (-not $item.PSIsContainer) {
      if ($item.LastWriteTimeUtc -gt $latest) {
        $latest = $item.LastWriteTimeUtc
      }
      continue
    }
    Get-ChildItem -LiteralPath $path -Recurse -File -Include *.rs,*.toml,*.json,build.rs -ErrorAction SilentlyContinue |
      ForEach-Object {
        if ($_.LastWriteTimeUtc -gt $latest) {
          $latest = $_.LastWriteTimeUtc
        }
      }
  }
  return $latest
}

function Test-ProxyBuildRequired {
  param([string]$ProxyExe)

  if ($NoBuild) {
    return $false
  }
  if ($env:CODESEEX_FORCE_BUILD -and $env:CODESEEX_FORCE_BUILD -ne "0") {
    return $true
  }
  if (-not (Test-Path $ProxyExe)) {
    return $true
  }
  $exeTime = (Get-Item -LiteralPath $ProxyExe).LastWriteTimeUtc
  $sourceTime = Get-LatestProxyInputWriteTime
  return $sourceTime -gt $exeTime
}

$buildCommand = @(
  "set `"CARGO_HOME=$env:CARGO_HOME`"",
  "set `"CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR`"",
  "set `"TEMP=$env:TEMP`"",
  "set `"TMP=$env:TMP`"",
  "set `"CODESEEX_DATA_DIR=$env:CODESEEX_DATA_DIR`"",
  "set `"CODESEEX_PORT=$env:CODESEEX_PORT`""
)
if ($env:DEEPSEEK_BASE_URL) {
  $buildCommand += "set `"DEEPSEEK_BASE_URL=$env:DEEPSEEK_BASE_URL`""
}
if ($env:DEEPSEEK_API_KEY) {
  $buildCommand += "set `"DEEPSEEK_API_KEY=$env:DEEPSEEK_API_KEY`""
}
$buildCommand += @(
  "`"$VsDevCmd`" -arch=x64 >nul",
  "cd /d `"$RepoRoot`"",
  "`"$cargo`" build -p codeseex-proxy"
)

$proxyExe = Join-Path $env:CARGO_TARGET_DIR "debug\codeseex-proxy.exe"
if (Test-ProxyBuildRequired -ProxyExe $proxyExe) {
  Write-Host "Building CodeSeeX proxy ..."
  cmd /d /c ($buildCommand -join " && ")
  if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
  }
} else {
  Write-Host "Skipping proxy Rust build; existing executable is up to date."
}

if (-not (Test-Path $proxyExe)) {
  throw "Proxy executable was not found: $proxyExe"
}

Write-Host "Starting CodeSeeX proxy on http://127.0.0.1:$Port ..."
& $proxyExe
exit $LASTEXITCODE
