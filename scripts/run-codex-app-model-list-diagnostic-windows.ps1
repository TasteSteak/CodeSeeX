param(
    [int]$Port = 9222,
    [string]$ScanPorts = "9222-9260",
    [int]$TimeoutMs = 45000,
    [switch]$NoKill,
    [switch]$NoOpenReport
)

$ErrorActionPreference = "Stop"

function Write-Step {
    param([string]$Message)
    Write-Host "[CodeSeeX] $Message"
}

function Get-RepoRoot {
    return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

function Test-CdpPort {
    param([int]$CandidatePort)
    try {
        $null = Invoke-RestMethod -Uri "http://127.0.0.1:$CandidatePort/json/version" -TimeoutSec 1
        return $true
    } catch {
        return $false
    }
}

function Find-CdpPort {
    param([string]$RangeText)
    $ports = @()
    if ($RangeText -match "^\s*(\d+)\s*-\s*(\d+)\s*$") {
        $start = [int]$Matches[1]
        $end = [int]$Matches[2]
        if ($end -lt $start) {
            return $null
        }
        for ($port = $start; $port -le $end; $port++) {
            $ports += $port
        }
    } else {
        $ports = $RangeText -split "," | ForEach-Object {
            $value = 0
            if ([int]::TryParse($_.Trim(), [ref]$value)) { $value }
        }
    }

    foreach ($candidate in $ports) {
        if (Test-CdpPort -CandidatePort $candidate) {
            return $candidate
        }
    }
    return $null
}

function Get-CodexProcesses {
    Get-CimInstance Win32_Process |
        Where-Object {
            ($_.Name -ieq "Codex.exe" -or $_.Name -ieq "codex.exe") -and
            ($_.CommandLine -like "*OpenAI.Codex*" -or $_.ExecutablePath -like "*OpenAI.Codex*")
        } |
        Sort-Object ProcessId
}

function Stop-CodexProcesses {
    $processes = @(Get-CodexProcesses)
    if ($processes.Count -eq 0) {
        Write-Step "No Codex process to stop."
        return
    }

    Write-Step "Stopping $($processes.Count) Codex process(es)."
    foreach ($process in $processes) {
        try {
            Stop-Process -Id $process.ProcessId -Force -ErrorAction Stop
        } catch {
            Write-Host "Failed to stop PID $($process.ProcessId): $($_.Exception.Message)"
        }
    }

    $deadline = (Get-Date).AddSeconds(12)
    while ((Get-Date) -lt $deadline) {
        if (@(Get-CodexProcesses).Count -eq 0) {
            Write-Step "Codex processes stopped."
            return
        }
        Start-Sleep -Milliseconds 300
    }

    $remaining = @(Get-CodexProcesses)
    if ($remaining.Count -gt 0) {
        Write-Host "Warning: some Codex processes are still running:"
        $remaining | Select-Object ProcessId, Name, CommandLine | Format-List
    }
}

function Run-Diagnostic {
    param(
        [string]$RepoRoot,
        [bool]$ConnectOnly,
        [int]$SelectedPort
    )

    $scriptPath = Join-Path $RepoRoot "scripts\codex-app-model-list-diagnostic.mjs"
    $args = @(
        $scriptPath,
        "--timeout-ms", "$TimeoutMs",
        "--out-dir", ".private\codex-app-diagnostics"
    )

    if ($ConnectOnly) {
        $args += @("--connect-only", "--port", "$SelectedPort")
    } else {
        $args += @("--launch", "--port", "$Port", "--scan-ports", $ScanPorts)
    }

    $args += "--probe-modules"

    Write-Step "Running: node $($args -join ' ')"
    $output = & node @args 2>&1
    $exitCode = $LASTEXITCODE
    $output | ForEach-Object { Write-Host $_ }
    if ($exitCode -ne 0) {
        throw "Diagnostic script failed with exit code $exitCode."
    }
    return $output
}

function Open-ReportFromOutput {
    param([object[]]$OutputLines)
    if ($NoOpenReport) {
        return
    }
    $markdownLine = $OutputLines | Where-Object { "$_" -like "Markdown report:*" } | Select-Object -Last 1
    if ($null -eq $markdownLine) {
        return
    }
    $reportPath = ("$markdownLine" -replace "^Markdown report:\s*", "").Trim()
    if (Test-Path -LiteralPath $reportPath) {
        Write-Step "Opening report location."
        Start-Process explorer.exe -ArgumentList "/select,`"$reportPath`""
    }
}

$repoRoot = Get-RepoRoot
Set-Location $repoRoot

if ($null -eq (Get-Command node -ErrorAction SilentlyContinue)) {
    throw "Node.js was not found in PATH. Install Node.js or add it to PATH before running this script."
}

Write-Step "Workspace: $repoRoot"
Write-Step "Checking existing CDP port(s): $ScanPorts"
$existingPort = Find-CdpPort -RangeText $ScanPorts

if ($existingPort) {
    Write-Step "Found existing Codex CDP on port $existingPort. Running read-only diagnostic."
    $output = Run-Diagnostic -RepoRoot $repoRoot -ConnectOnly $true -SelectedPort $existingPort
    Open-ReportFromOutput -OutputLines $output
    exit 0
}

if ($NoKill) {
    Write-Step "No CDP port found and -NoKill was set. Running launch attempt without stopping existing Codex."
} else {
    Write-Step "No CDP port found. A fresh debug launch requires stopping existing Codex processes."
    Stop-CodexProcesses
}

$output = Run-Diagnostic -RepoRoot $repoRoot -ConnectOnly $false -SelectedPort $Port
Open-ReportFromOutput -OutputLines $output
