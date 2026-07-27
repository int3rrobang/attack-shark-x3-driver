[CmdletBinding()]
param(
    [ValidateSet('plan', 'offline', 'hardware', 'capture-manual', 'all')]
    [string]$Mode = 'plan',

    [ValidateSet('wired', 'receiver', 'both')]
    [string]$Transport = 'wired',

    [string]$OutputRoot = (Join-Path $PSScriptRoot '..\test-artifacts\fa61-suite'),
    [string]$CaptureRepo = (Join-Path $PSScriptRoot '..\..\tshark_mouse'),
    [switch]$IncludeAppControls,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$DriverRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$CargoManifest = Join-Path $DriverRoot 'Cargo.toml'
$RunRoot = Join-Path $OutputRoot (Get-Date -Format 'yyyyMMdd-HHmmss')
$LogRoot = Join-Path $RunRoot 'logs'

function Show-Plan {
    @"
X3/M600 test suite

This runner is deliberately observational for hardware. It never sends configuration
writes, reset packets, firmware updates, BLE writes, lighting settings, scroll remaps,
or button-table changes. The physical DPI button and ordinary input reports are tested
through USBPcap capture. Hardware mode intentionally limits itself to discovery;
use capture-manual to exercise physical reports and record evidence.

Offline coverage (no device access):
  * Rust workspace fmt/check/clippy/test
  * x3ctl --help and debug subcommand help (dpi, profile-control, read-selector)
  * Offline packet builders via 'cargo run -p x3ctl -- <args>':
      exactly six DPI slots on wired and receiver, profile-control framing (compact/full),
      and read-selector report generation for all report types
  * Six-slot boundary rejection: an attempted seventh active stage must fail (exit code 1)

Hardware discovery coverage:
  * x3ctl devices on wired and/or receiver for the selected transport
  * No configuration writes or targeted profile reads in this observational suite

Manual USBPcap coverage uses the safe driver-validation preset:
  * receiver/mouse initialization, idle baselines, motion, wheel, normal buttons,
    and exactly six physical DPI-button presses
  * one Profile1 field at a time for polling rate, one of six DPI stages, lift-off
    distance, key response, ripple control, angle snap, motion sync, optional sleep
    timer, and one safe side-button assignment; every change has a separate
    restoration prompt
  * Enter records a step, 's' skips an unavailable control, and 'q' finishes early
  * lighting, macros, reset, non-Profile1 changes, profile creation/switching,
    maximum-profile changes, scroll remaps, and multi-field changes are excluded

Artifacts are written to a new timestamped directory under -OutputRoot. Existing files
are never deleted. The capture mode uses the sibling tshark_mouse repository by default.

Examples:
  pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode plan
  pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode offline
  pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode hardware -Transport wired
  pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode hardware -Transport receiver
  pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode capture-manual -Transport receiver
  pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode all -Transport both
"@ | Write-Host
}

if ($Mode -eq 'plan') {
    Show-Plan
    return
}

New-Item -ItemType Directory -Force -Path $LogRoot | Out-Null

$script:StepNumber = 0
$script:ContinueOnFailure = $false
$script:Failures = @()
function Invoke-Step {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [string]$WorkingDirectory = $DriverRoot,
        [int[]]$ExpectedExitCodes = @(0)
    )

    $script:StepNumber++
    $safeName = ($Name -replace '[^A-Za-z0-9_.-]', '_')
    $logPath = Join-Path $LogRoot ('{0:D3}-{1}.log' -f $script:StepNumber, $safeName)
    Write-Host "`n==> $Name"
    Write-Host "    $Executable $($Arguments -join ' ')"

    Push-Location $WorkingDirectory
    try {
        $captured = @(& $Executable @Arguments 2>&1)
        $exitCode = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }

    $text = ($captured | Out-String).TrimEnd()
    if ($text) {
        $text | Write-Host
    }
    Set-Content -Path $logPath -Value $text -Encoding utf8

    if ($ExpectedExitCodes -notcontains $exitCode) {
        $failure = "Step '$Name' failed with exit code $exitCode. See $logPath"
        if ($script:ContinueOnFailure) {
            $script:Failures += $failure
            Write-Host "    FAILED (continuing): $failure"
        }
        else {
            throw $failure
        }
    }
    return $text
}

function Invoke-X3Ctl {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [int[]]$ExpectedExitCodes = @(0)
    )
    $prefix = @('run', '--quiet', '--manifest-path', $CargoManifest, '-p', 'x3ctl', '--')
    return Invoke-Step -Name $Name -Executable 'cargo' -Arguments ($prefix + $Arguments) -ExpectedExitCodes $ExpectedExitCodes
}

function Run-OfflineSuite {
    $script:ContinueOnFailure = $true
    $script:Failures = @()
    try {
        Invoke-Step -Name 'cargo-fmt-check' -Executable 'cargo' -Arguments @('fmt', '--all', '--', '--check') | Out-Null
        Invoke-Step -Name 'cargo-check' -Executable 'cargo' -Arguments @('check', '--workspace', '--all-targets', '--all-features') | Out-Null
        Invoke-Step -Name 'cargo-clippy' -Executable 'cargo' -Arguments @('clippy', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings') | Out-Null
        Invoke-Step -Name 'cargo-test' -Executable 'cargo' -Arguments @('test', '--workspace', '--all-targets', '--all-features') | Out-Null

        Invoke-X3Ctl -Name 'x3ctl-help' -Arguments @('--help') | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-help' -Arguments @('debug', '--help') | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-dpi-help' -Arguments @('debug', 'dpi', '--help') | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-profile-control-help' -Arguments @('debug', 'profile-control', '--help') | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-help' -Arguments @('debug', 'read-selector', '--help') | Out-Null

        $sixStages = '50,800,1600,2400,3200,26000'
        Invoke-X3Ctl -Name 'x3ctl-debug-dpi-wired-six-slots' -Arguments @(
            '--no-state', 'debug', 'dpi', '--transport', 'wired', '--profile', '1',
            '--stages', $sixStages, '--active', '6', '--lod', '2',
            '--ripple-control', '--angle-snap', '--motion-sync'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-dpi-receiver-six-slots' -Arguments @(
            '--no-state', 'debug', 'dpi', '--transport', 'receiver', '--profile', '1',
            '--stages', $sixStages, '--active', '6'
        ) | Out-Null

        Invoke-X3Ctl -Name 'x3ctl-debug-profile-control-compact' -Arguments @(
            '--no-state', 'debug', 'profile-control', '--current', '2', '--maximum', '5',
            '--framing', 'compact'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-profile-control-full' -Arguments @(
            '--no-state', 'debug', 'profile-control', '--current', '2', '--maximum', '5',
            '--framing', 'full'
        ) | Out-Null

        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-version' -Arguments @(
            '--no-state', 'debug', 'read-selector', '--report', 'version'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-profile-metadata' -Arguments @(
            '--no-state', 'debug', 'read-selector', '--report', 'profile-metadata'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-polling-rate' -Arguments @(
            '--no-state', 'debug', 'read-selector', '--report', 'polling-rate'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-dpi' -Arguments @(
            '--no-state', 'debug', 'read-selector', '--report', 'dpi', '--profile', '1'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-preferences' -Arguments @(
            '--no-state', 'debug', 'read-selector', '--report', 'preferences', '--profile', '1'
        ) | Out-Null
        Invoke-X3Ctl -Name 'x3ctl-debug-read-selector-buttons' -Arguments @(
            '--no-state', 'debug', 'read-selector', '--report', 'buttons', '--profile', '1'
        ) | Out-Null

        Invoke-X3Ctl -Name 'x3ctl-six-slot-boundary-rejection' -Arguments @(
            '--no-state', 'debug', 'dpi', '--transport', 'wired', '--stages', $sixStages,
            '--active', '7'
        ) -ExpectedExitCodes @(1) | Out-Null
    }
    finally {
        $script:ContinueOnFailure = $false
    }
    if ($script:Failures.Count -gt 0) {
        $failureSummary = $script:Failures -join [Environment]::NewLine
        throw "Offline suite completed with $($script:Failures.Count) failure(s):`n$failureSummary"
    }
}

function Run-HardwareChecks {
    if ($Transport -in @('wired', 'both')) {
        Invoke-X3Ctl -Name 'x3ctl-fa61-discovery' -Arguments @('--transport', 'wired', '--no-state', 'devices') | Out-Null
    }
    if ($Transport -in @('receiver', 'both')) {
        Invoke-X3Ctl -Name 'x3ctl-fa60-discovery' -Arguments @('--transport', 'receiver', '--no-state', 'devices') | Out-Null
    }

    Write-Host "`nNo configuration readback or hardware writes were attempted."
    Write-Host 'Use capture-manual to exercise physical reports and record evidence.'
}

function Run-ManualCapture {
    if (-not (Test-Path $CaptureRepo)) {
        throw "Capture repository not found at $CaptureRepo. Use -CaptureRepo to override it."
    }
    $captureOutput = Join-Path $RunRoot 'capture-manual'
    New-Item -ItemType Directory -Force -Path $captureOutput | Out-Null
    $capturePrefix = switch ($Transport) {
        'wired' { 'fa61-manual' }
        'receiver' { 'fa60-manual' }
        default { 'x3-manual' }
    }
    $capturePreset = if ($Transport -eq 'wired') {
        'fa61-driver-safe'
    }
    else {
        'fa60-driver-safe'
    }
    if ($IncludeAppControls) {
        Write-Host '-IncludeAppControls is no longer required; safe app controls are in the default preset.'
    }

    $pythonArgs = @(
        '-m', 'tshark_mouse', 'guided', '--all-interfaces', '--all-traffic',
        '--diff', '--diff-against', 'previous', '--preset', $capturePreset,
        '--output-dir', $captureOutput, '--prefix', $capturePrefix
    )

    $script:StepNumber++
    $logPath = Join-Path $LogRoot ('{0:D3}-usbpcap-manual-capture.log' -f $script:StepNumber)
    Write-Host "`n==> usbpcap-manual-capture"
    Write-Host "    python $($pythonArgs -join ' ')"
    Write-Host "    (interactive — prompts appear below)`n"

    Push-Location $CaptureRepo
    try {
        python @pythonArgs
        $exitCode = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }

    "exit code: $exitCode" | Set-Content -Path $logPath -Encoding utf8
    if ($exitCode -ne 0) {
        throw "Step 'usbpcap-manual-capture' failed with exit code $exitCode. See $logPath"
    }
    Write-Host "Manual capture artifacts: $captureOutput"
}

switch ($Mode) {
    'offline' {
        Run-OfflineSuite
    }
    'hardware' {
        Run-HardwareChecks
    }
    'capture-manual' {
        Run-ManualCapture
    }
    'all' {
        Run-OfflineSuite
        Run-HardwareChecks
        Run-ManualCapture
    }
}

Write-Host "`nCompleted '$Mode'. Artifacts and logs: $RunRoot"
