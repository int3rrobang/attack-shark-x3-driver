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
$NativeExe = Join-Path $DriverRoot 'target\debug\attack-shark-x3.exe'
$TsCli = Join-Path $DriverRoot 'src\cli.ts'

function Show-Plan {
    @"
X3/M600 test suite

This runner is deliberately observational for hardware. It never sends configuration
writes, reset packets, firmware updates, BLE writes, lighting settings, scroll remaps,
or button-table changes. The physical DPI button and ordinary input reports are tested
through USBPcap capture. Hardware mode intentionally limits itself to discovery,
open/close, and battery behavior; use the native receiver smoke example for validated
configuration readback and reversible write coverage.

Offline coverage (no device access):
  * Bun unit tests, TypeScript typecheck/build, ESLint/Prettier checks
  * Rust workspace tests/checks/clippy/fmt
  * TypeScript CLI help and offline packet builders:
      exactly six DPI slots on wired and receiver, all four polling rates, safe binding,
      and reset packet generation (reset is never sent by this suite)
  * Six-slot boundary rejection: an attempted seventh active stage must fail
  * Native Rust CLI help and offline DPI/profile/read-selector packet generation
  * Preference/lighting builders remain covered only by existing offline unit tests;
    they are not treated as X3 hardware features.

Hardware discovery coverage:
  * Native FA61 wired and/or FA60 receiver discovery for the selected transport
  * TypeScript wired open/close and wired battery-unavailable behavior
  * TypeScript FA60 receiver open/close and battery telemetry when a receiver is present
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

function Invoke-BunCli {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [int[]]$ExpectedExitCodes = @(0)
    )
    return Invoke-Step -Name $Name -Executable 'bun' -Arguments (@('run', 'cli') + $Arguments) -ExpectedExitCodes $ExpectedExitCodes
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [int[]]$ExpectedExitCodes = @(0)
    )
    $prefix = @('run', '--quiet', '--manifest-path', $CargoManifest, '-p', 'attack-shark-x3', '--')
    return Invoke-Step -Name $Name -Executable 'cargo' -Arguments ($prefix + $Arguments) -ExpectedExitCodes $ExpectedExitCodes
}

function Ensure-NativeBuild {
    if ($SkipBuild) {
        if (-not (Test-Path $NativeExe)) {
            throw "-SkipBuild was supplied but $NativeExe does not exist."
        }
        return
    }
    Invoke-Step -Name 'cargo-build-native-cli' -Executable 'cargo' -Arguments @(
        'build', '--quiet', '--manifest-path', $CargoManifest, '-p', 'attack-shark-x3'
    ) | Out-Null
}

function Run-OfflineSuite {
    $script:ContinueOnFailure = $true
    $script:Failures = @()
    try {
    $staticSteps = @(
        @{ Name = 'bun-test'; Executable = 'bun'; Arguments = @('test') },
        @{ Name = 'bun-typecheck'; Executable = 'bun'; Arguments = @('run', 'typecheck') },
        @{ Name = 'bun-build'; Executable = 'bun'; Arguments = @('run', 'build') },
        @{ Name = 'bun-lint'; Executable = 'bun'; Arguments = @('run', 'lint') },
        @{ Name = 'bun-format-check'; Executable = 'bun'; Arguments = @('run', 'format') },
        @{ Name = 'cargo-test'; Executable = 'cargo'; Arguments = @('test', '--workspace', '--all-targets', '--all-features') },
        @{ Name = 'cargo-check'; Executable = 'cargo'; Arguments = @('check', '--workspace', '--all-targets', '--all-features') },
        @{ Name = 'cargo-clippy'; Executable = 'cargo'; Arguments = @('clippy', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings') },
        @{ Name = 'cargo-format-check'; Executable = 'cargo'; Arguments = @('fmt', '--all', '--', '--check') }
    )
    foreach ($step in $staticSteps) {
        Invoke-Step -Name $step.Name -Executable $step.Executable -Arguments $step.Arguments | Out-Null
    }

    foreach ($step in @(
        @{ Name = 'typescript-cli-help'; Arguments = @('--help') },
        @{ Name = 'typescript-cli-hex-help'; Arguments = @('hex', '--help') },
        @{ Name = 'typescript-cli-bind-actions'; Arguments = @('bind', '--list-actions') },
        @{ Name = 'typescript-cli-bind-buttons'; Arguments = @('bind', '--list-buttons') }
    )) {
        Invoke-BunCli -Name $step.Name -Arguments $step.Arguments | Out-Null
    }

    # Six slots are the hardware contract. The builders may support broader protocol
    # ranges for historical devices, but this suite never proposes 7 or 8 to X3/M600.
    $sixStages = '50,800,1600,2400,3200,26000'
    foreach ($transportName in @('wired', 'receiver')) {
        Invoke-BunCli -Name "typescript-hex-dpi-$transportName" -Arguments @(
            'hex', 'dpi', '--transport', $transportName, '--stages', $sixStages, '--active', '6',
            '--lod', '2', '--ripple', 'on', '--angle-snap', 'on', '--motion-sync', 'on'
        ) | Out-Null
        Invoke-BunCli -Name "typescript-hex-reset-$transportName" -Arguments @('hex', 'reset', '--transport', $transportName) | Out-Null
        foreach ($rate in @(125, 250, 500, 1000)) {
            Invoke-BunCli -Name "typescript-hex-rate-$transportName-$rate" -Arguments @(
                'hex', 'rate', '--transport', $transportName, '--rate', [string]$rate
            ) | Out-Null
        }
    }
    Invoke-BunCli -Name 'typescript-six-slot-active-stage-rejection' -Arguments @(
        'hex', 'dpi', '--transport', 'wired', '--stages', $sixStages, '--active', '7'
    ) -ExpectedExitCodes @(1) | Out-Null
    Invoke-BunCli -Name 'typescript-hex-bind-safe' -Arguments @(
        'hex', 'bind', '--transport', 'wired', '--button', 'forward', '--action', 'shortcut-swap-window'
    ) | Out-Null

    $nativeOffline = @(
        @{ Name = 'native-cli-help'; Arguments = @('--help') },
        @{ Name = 'native-read-help'; Arguments = @('read', '--help') },
        @{ Name = 'native-hex-help'; Arguments = @('hex', '--help') },
        @{ Name = 'native-hex-dpi-wired-six-slots'; Arguments = @('hex', 'dpi', '--transport', 'wired', '--stages', $sixStages, '--active', '6', '--lod', 'two', '--ripple-control', '--angle-snap', '--motion-sync') },
        @{ Name = 'native-hex-dpi-receiver-six-slots'; Arguments = @('hex', 'dpi', '--transport', 'receiver', '--stages', $sixStages, '--active', '6') },
        @{ Name = 'native-hex-profile-compact'; Arguments = @('hex', 'profile-control', '--current', '2', '--maximum', '5', '--framing', 'compact') },
        @{ Name = 'native-hex-profile-full'; Arguments = @('hex', 'profile-control', '--current', '2', '--maximum', '5', '--framing', 'full') }
    )
    foreach ($report in @('version', 'profile-metadata', 'polling-rate')) {
        $nativeOffline += @{ Name = "native-hex-selector-$report"; Arguments = @('hex', 'read-selector', '--report', $report) }
    }
    foreach ($report in @('dpi', 'preferences', 'buttons')) {
        $nativeOffline += @{ Name = "native-hex-selector-$report"; Arguments = @('hex', 'read-selector', '--report', $report, '--profile', '1') }
    }
    foreach ($step in $nativeOffline) {
        Invoke-Native -Name $step.Name -Arguments $step.Arguments | Out-Null
    }
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
    Ensure-NativeBuild
    if ($Transport -in @('wired', 'both')) {
        Invoke-Native -Name 'native-fa61-discovery' -Arguments @('--transport', 'wired', 'list') | Out-Null
    }
    if ($Transport -in @('receiver', 'both')) {
        Invoke-Native -Name 'native-fa60-discovery' -Arguments @('--transport', 'receiver', 'list') | Out-Null
    }
    if ($Transport -in @('wired', 'both')) {
        Invoke-BunCli -Name 'typescript-wired-open-close' -Arguments @('--transport', 'wired', 'open') | Out-Null
        Invoke-BunCli -Name 'typescript-wired-battery-unavailable' -Arguments @('--transport', 'wired', 'battery') | Out-Null
    }
    if ($Transport -in @('receiver', 'both')) {
        Invoke-BunCli -Name 'typescript-fa60-open-close' -Arguments @('--transport', 'receiver', 'open') | Out-Null
        Invoke-BunCli -Name 'typescript-fa60-battery' -Arguments @('--transport', 'receiver', 'battery') | Out-Null
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
