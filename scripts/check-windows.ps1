param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$CargoHome = $env:CARGO_HOME,
  [string]$CargoTargetDir = $env:CARGO_TARGET_DIR,
  [string]$DevRoot = ""
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot

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
  throw "MSVC Build Tools not found. Install Visual Studio Build Tools with VC.Tools.x86.x64 and Windows SDK."
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
  "`"$VsDevCmd`" -arch=x64 >nul",
  "cd /d `"$RepoRoot`"",
  "`"$cargo`" fmt --all --check",
  "`"$cargo`" test -p codeseex-core -p codeseex-store -p codeseex-proxy",
  "`"$cargo`" test -p codeseex-desktop --no-default-features --features tauri/custom-protocol",
  "`"$cargo`" check -p codeseex-desktop --no-default-features --features tauri/custom-protocol"
) -join " && "

cmd /d /c $command
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}
