param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$DevRoot = "D:\DevTools\CodeSeeXNext"
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$LogDir = Join-Path $DevRoot "Logs"

$env:CARGO_HOME = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $DevRoot "Cargo" }
$env:CARGO_TARGET_DIR = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $DevRoot "CargoTarget" }
$env:npm_config_cache = if ($env:npm_config_cache) { $env:npm_config_cache } else { Join-Path $DevRoot "npm-cache" }
$env:TEMP = Join-Path $DevRoot "Temp"
$env:TMP = $env:TEMP

New-Item -ItemType Directory -Force -Path $env:CARGO_HOME, $env:CARGO_TARGET_DIR, $env:npm_config_cache, $env:TEMP, $LogDir | Out-Null

function Test-LocalPort {
  param([int]$Port)

  $client = [System.Net.Sockets.TcpClient]::new()
  try {
    $connect = $client.BeginConnect("127.0.0.1", $Port, $null, $null)
    if (-not $connect.AsyncWaitHandle.WaitOne(300)) {
      return $false
    }
    $client.EndConnect($connect)
    return $true
  } catch {
    return $false
  } finally {
    $client.Close()
  }
}

function Wait-LocalPort {
  param(
    [int]$Port,
    [int]$TimeoutSeconds = 30
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  while ((Get-Date) -lt $deadline) {
    if (Test-LocalPort -Port $Port) {
      return
    }
    Start-Sleep -Milliseconds 300
  }

  throw "Frontend dev server did not start on http://127.0.0.1:$Port within $TimeoutSeconds seconds."
}

function Stop-ProcessTree {
  param([int]$ProcessId)

  $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $ProcessId" -ErrorAction SilentlyContinue
  foreach ($child in $children) {
    Stop-ProcessTree -ProcessId $child.ProcessId
  }

  Stop-Process -Id $ProcessId -Force -ErrorAction SilentlyContinue
}

function Get-LatestRustInputWriteTime {
  $paths = @(
    (Join-Path $RepoRoot "Cargo.toml"),
    (Join-Path $RepoRoot "Cargo.lock"),
    (Join-Path $RepoRoot "crates"),
    (Join-Path $RepoRoot "apps\codeseex-desktop\src-tauri")
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

function Test-DesktopBuildRequired {
  param([string]$DesktopExe)

  if ($env:CODESEEX_FORCE_BUILD -and $env:CODESEEX_FORCE_BUILD -ne "0") {
    return $true
  }
  if (-not (Test-Path $DesktopExe)) {
    return $true
  }
  $exeTime = (Get-Item -LiteralPath $DesktopExe).LastWriteTimeUtc
  $sourceTime = Get-LatestRustInputWriteTime
  return $sourceTime -gt $exeTime
}

if (-not $VsDevCmd) {
  $defaultVsDevCmd = Join-Path $DevRoot "VSBuildTools\Common7\Tools\VsDevCmd.bat"
  if (Test-Path $defaultVsDevCmd) {
    $VsDevCmd = $defaultVsDevCmd
  }
}

if (-not $VsDevCmd -or -not (Test-Path $VsDevCmd)) {
  throw "MSVC Build Tools not found. Run scripts/check-windows.ps1 after installing Build Tools."
}

$buildCommand = @(
  "chcp 65001 >nul",
  "set `"PATH=$env:USERPROFILE\.cargo\bin;%PATH%`"",
  "set `"CARGO_HOME=$env:CARGO_HOME`"",
  "set `"CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR`"",
  "set `"npm_config_cache=$env:npm_config_cache`"",
  "set `"TEMP=$env:TEMP`"",
  "set `"TMP=$env:TMP`"",
  "`"$VsDevCmd`" -arch=x64 >nul",
  "cargo build -p codeseex-desktop --no-default-features"
) -join " && "

$viteProcess = $null
$startedVite = $false
$exitCode = 0

Push-Location $RepoRoot
try {
  if (-not (Test-LocalPort -Port 5173)) {
    $viteOut = Join-Path $LogDir "vite-dev.out.log"
    $viteErr = Join-Path $LogDir "vite-dev.err.log"
    Remove-Item -LiteralPath $viteOut, $viteErr -ErrorAction SilentlyContinue

    Write-Host "Starting frontend dev server on http://127.0.0.1:5173 ..."
    $viteProcess = Start-Process -FilePath "cmd.exe" `
      -ArgumentList "/d", "/c", "npm --workspace apps/codeseex-ui run dev" `
      -WorkingDirectory $RepoRoot `
      -RedirectStandardOutput $viteOut `
      -RedirectStandardError $viteErr `
      -WindowStyle Hidden `
      -PassThru
    $startedVite = $true
    Wait-LocalPort -Port 5173 -TimeoutSeconds 30
  }

  $desktopExe = Join-Path $env:CARGO_TARGET_DIR "debug\codeseex-desktop.exe"
  if (Test-DesktopBuildRequired -DesktopExe $desktopExe) {
    Write-Host "Building CodeSeeX Next desktop ..."
    cmd /d /c $buildCommand
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
      throw "Desktop build failed with code $exitCode."
    }
  } else {
    Write-Host "Skipping desktop Rust build; existing executable is up to date."
  }

  if (-not (Test-Path $desktopExe)) {
    throw "Desktop executable was not found: $desktopExe"
  }

  Write-Host "Starting CodeSeeX Next desktop ..."
  $desktopProcess = Start-Process -FilePath $desktopExe -WorkingDirectory $RepoRoot -PassThru
  Start-Sleep -Milliseconds 800
  if ($desktopProcess.HasExited) {
    $exitCode = $desktopProcess.ExitCode
    throw "CodeSeeX Next desktop exited immediately with code $exitCode."
  }

  $desktopProcess.WaitForExit()
  $exitCode = $desktopProcess.ExitCode
} finally {
  if ($startedVite -and $viteProcess -and -not $viteProcess.HasExited) {
    Stop-ProcessTree -ProcessId $viteProcess.Id
  }
  Pop-Location
}

exit $exitCode
