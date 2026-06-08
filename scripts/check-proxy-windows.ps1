param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$CargoHome = $env:CARGO_HOME,
  [string]$CargoTargetDir = $env:CARGO_TARGET_DIR,
  [string]$DevRoot = ""
)

$ErrorActionPreference = "Stop"

if (-not $DevRoot) {
  $DevRoot = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "CodeSeeX\Dev"
}

if (-not $CargoHome) {
  $CargoHome = Join-Path $DevRoot "Cargo"
}
if (-not $CargoTargetDir) {
  $CargoTargetDir = Join-Path $DevRoot "CargoTarget"
}

$env:CARGO_HOME = $CargoHome
$env:CARGO_TARGET_DIR = $CargoTargetDir
$env:TEMP = Join-Path $DevRoot "Temp"
$env:TMP = $env:TEMP

New-Item -ItemType Directory -Force -Path $env:CARGO_HOME, $env:CARGO_TARGET_DIR, $env:TEMP | Out-Null

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

$repoRoot = Split-Path -Parent $PSScriptRoot
$command = @(
  "set `"CARGO_HOME=$env:CARGO_HOME`"",
  "set `"CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR`"",
  "set `"TEMP=$env:TEMP`"",
  "set `"TMP=$env:TMP`"",
  "`"$VsDevCmd`" -arch=x64 >nul",
  "cd /d `"$repoRoot`"",
  "`"$cargo`" fmt --all --check",
  "`"$cargo`" test -p codeseex-proxy --lib"
) -join " && "

cmd /d /c $command
exit $LASTEXITCODE
