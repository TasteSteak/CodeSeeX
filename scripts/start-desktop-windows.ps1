param(
  [string]$VsDevCmd = $env:CODESEEX_VSDEVCMD,
  [string]$DevRoot = "D:\DevTools\CodeSeeXNext",
  [switch]$NoBuild,
  [switch]$BuildOnly,
  [switch]$KeepExisting
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$LogDir = Join-Path $DevRoot "Logs"

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

New-Item -ItemType Directory -Force -Path $env:CARGO_HOME, $env:CARGO_TARGET_DIR, $env:TEMP, $LogDir | Out-Null

function Resolve-VsDevCmd {
  param([string]$Requested)

  if ($Requested -and (Test-Path $Requested)) {
    return $Requested
  }

  $defaultVsDevCmd = Join-Path $DevRoot "VSBuildTools\Common7\Tools\VsDevCmd.bat"
  if (Test-Path $defaultVsDevCmd) {
    return $defaultVsDevCmd
  }

  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  if (Test-Path $vswhere) {
    $installPath = & $vswhere -all -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1
    if ($installPath) {
      $candidate = Join-Path $installPath "Common7\Tools\VsDevCmd.bat"
      if (Test-Path $candidate) {
        return $candidate
      }
    }
  }

  throw "MSVC Build Tools not found. Run scripts/check-windows.ps1 after installing Build Tools."
}

function Resolve-Cargo {
  $cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
  if (Test-Path $cargo) {
    return $cargo
  }
  return "cargo"
}

function Get-LatestPathWriteTime {
  param(
    [string[]]$Paths,
    [string[]]$Include
  )

  $latest = [DateTime]::MinValue
  foreach ($path in $Paths) {
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
    Get-ChildItem -LiteralPath $path -Recurse -File -Include $Include -ErrorAction SilentlyContinue |
      ForEach-Object {
        if ($_.LastWriteTimeUtc -gt $latest) {
          $latest = $_.LastWriteTimeUtc
        }
      }
  }
  return $latest
}

function Get-LatestDesktopInputWriteTime {
  $paths = @(
    (Join-Path $RepoRoot "Cargo.toml"),
    (Join-Path $RepoRoot "Cargo.lock"),
    (Join-Path $RepoRoot "crates"),
    (Join-Path $RepoRoot "apps\desktop\src-tauri"),
    (Join-Path $RepoRoot "apps\ui\public")
  )
  return Get-LatestPathWriteTime -Paths $paths -Include @("*.rs", "*.toml", "*.json", "build.rs", "*.js", "*.css", "*.html", "*.svg", "*.png", "*.ico")
}

function Get-DesktopBuildStampPath {
  return Join-Path $env:CARGO_TARGET_DIR "debug\codeseex-desktop.static-ui.stamp.json"
}

function Get-DesktopBuildStampData {
  [pscustomobject]@{
    mode = "static-ui"
    input_ticks = (Get-LatestDesktopInputWriteTime).Ticks
  }
}

function Test-DesktopBuildRequired {
  param([string]$DesktopExe)

  if ($NoBuild) {
    return $false
  }
  if ($env:CODESEEX_FORCE_BUILD -and $env:CODESEEX_FORCE_BUILD -ne "0") {
    return $true
  }
  if (-not (Test-Path $DesktopExe)) {
    return $true
  }
  $stampPath = Get-DesktopBuildStampPath
  if (-not (Test-Path $stampPath)) {
    return $true
  }
  $expected = Get-DesktopBuildStampData
  try {
    $actual = Get-Content -LiteralPath $stampPath -Raw | ConvertFrom-Json
  } catch {
    return $true
  }
  return (
    $actual.mode -ne $expected.mode -or
    [int64]$actual.input_ticks -ne [int64]$expected.input_ticks
  )
}

function Write-DesktopBuildStamp {
  $stampPath = Get-DesktopBuildStampPath
  $stampDir = Split-Path -Parent $stampPath
  New-Item -ItemType Directory -Force -Path $stampDir | Out-Null
  $json = Get-DesktopBuildStampData | ConvertTo-Json -Compress
  [System.IO.File]::WriteAllText($stampPath, $json, [System.Text.UTF8Encoding]::new($false))
}

function Stop-ExistingDesktopProcesses {
  param([string]$DesktopExe)

  $resolvedDesktopExe = [System.IO.Path]::GetFullPath($DesktopExe)
  $matches = @(
    Get-Process -Name "codeseex-desktop" -ErrorAction SilentlyContinue |
      Where-Object {
        try {
          $_.Path -and ([System.IO.Path]::GetFullPath($_.Path) -ieq $resolvedDesktopExe)
        } catch {
          $false
        }
      }
  )
  if ($matches.Count -eq 0) {
    return
  }

  Write-Host "Stopping existing CodeSeeX desktop process before dev launch ..."
  foreach ($process in $matches) {
    try {
      if (-not $process.HasExited) {
        [void]$process.CloseMainWindow()
        [void]$process.WaitForExit(1500)
      }
      if (-not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit(3000)
      }
    } catch {
      Write-Warning "Failed to stop process $($process.Id): $($_.Exception.Message)"
    }
  }
}

$VsDevCmd = Resolve-VsDevCmd -Requested $VsDevCmd
$cargo = Resolve-Cargo
$desktopExe = Join-Path $env:CARGO_TARGET_DIR "debug\codeseex-desktop.exe"

$buildCommand = @(
  "chcp 65001 >nul",
  "set `"PATH=$env:USERPROFILE\.cargo\bin;%PATH%`"",
  "set `"CARGO_HOME=$env:CARGO_HOME`"",
  "set `"CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR`"",
  "set `"TEMP=$env:TEMP`"",
  "set `"TMP=$env:TMP`"",
  "`"$VsDevCmd`" -arch=x64 >nul",
  "cd /d `"$RepoRoot`"",
  "`"$cargo`" build -p codeseex-desktop --no-default-features --features tauri/custom-protocol"
) -join " && "

Push-Location $RepoRoot
try {
  if (Test-DesktopBuildRequired -DesktopExe $desktopExe) {
    Write-Host "Building CodeSeeX desktop (static UI) ..."
    cmd /d /c $buildCommand
    if ($LASTEXITCODE -ne 0) {
      throw "Desktop build failed with code $LASTEXITCODE."
    }
    Write-DesktopBuildStamp
  } else {
    Write-Host "Skipping desktop Rust build; existing executable is up to date."
  }

  if (-not (Test-Path $desktopExe)) {
    throw "Desktop executable was not found: $desktopExe"
  }

  if ($BuildOnly) {
    Write-Host "Build-only requested; desktop was not started."
    exit 0
  }

  if (-not $KeepExisting) {
    Stop-ExistingDesktopProcesses -DesktopExe $desktopExe
  }

  Write-Host "Starting CodeSeeX desktop ..."
  $desktopProcess = Start-Process -FilePath $desktopExe -WorkingDirectory $RepoRoot -PassThru
  Start-Sleep -Milliseconds 800
  if ($desktopProcess.HasExited) {
    throw "CodeSeeX desktop exited immediately with code $($desktopProcess.ExitCode)."
  }

  $desktopProcess.WaitForExit()
  exit $desktopProcess.ExitCode
} finally {
  Pop-Location
}
