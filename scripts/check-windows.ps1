param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$CargoHome = $env:CARGO_HOME,
  [string]$CargoTargetDir = $env:CARGO_TARGET_DIR,
  [string]$DevRoot = "D:\DevTools\CodeSeeXNext"
)

$ErrorActionPreference = "Stop"

if (-not $CargoHome) {
  $CargoHome = Join-Path $DevRoot "Cargo"
}
if (-not $CargoTargetDir) {
  $CargoTargetDir = Join-Path $DevRoot "CargoTarget"
}
if (-not $env:npm_config_cache) {
  $env:npm_config_cache = Join-Path $DevRoot "npm-cache"
}

$env:CARGO_HOME = $CargoHome
$env:CARGO_TARGET_DIR = $CargoTargetDir
$env:TEMP = Join-Path $DevRoot "Temp"
$env:TMP = $env:TEMP

New-Item -ItemType Directory -Force -Path $env:CARGO_HOME, $env:CARGO_TARGET_DIR, $env:TEMP, $env:npm_config_cache | Out-Null

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
  "`"$cargo`" fmt --all --check",
  "`"$cargo`" test -p codeseex-core -p codeseex-store -p codeseex-proxy",
  "`"$cargo`" check -p codeseex-desktop"
) -join " && "

cmd /d /c $command
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}

npm --workspace apps/codeseex-ui run check
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}

npm --workspace apps/codeseex-ui run build
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}
