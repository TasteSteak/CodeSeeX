param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$DevRoot = "D:\DevTools\CodeSeeXNext",
  [string]$DataDir = "D:\DevTools\CodeSeeXNext\Data",
  [int]$Port = 8787,
  [string]$UpstreamBaseUrl = ""
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot

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

$command = @(
  "set `"CARGO_HOME=$env:CARGO_HOME`"",
  "set `"CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR`"",
  "set `"TEMP=$env:TEMP`"",
  "set `"TMP=$env:TMP`"",
  "set `"CODESEEX_DATA_DIR=$env:CODESEEX_DATA_DIR`"",
  "set `"CODESEEX_PORT=$env:CODESEEX_PORT`""
)
if ($env:DEEPSEEK_BASE_URL) {
  $command += "set `"DEEPSEEK_BASE_URL=$env:DEEPSEEK_BASE_URL`""
}
$command += @(
  "`"$VsDevCmd`" -arch=x64 >nul",
  "cd /d `"$RepoRoot`"",
  "`"$cargo`" run -p codeseex-proxy"
)

cmd /d /c ($command -join " && ")
exit $LASTEXITCODE
