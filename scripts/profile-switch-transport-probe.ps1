[CmdletBinding()]
param(
    [ValidateSet('wired', 'receiver')]
    [string]$Transport = 'receiver',

    [ValidateSet('read-only', 'switches', 'all')]
    [string]$Suite = 'read-only',

    [ValidateRange(1, 50)]
    [int]$ReadRepetitions = 20,

    [ValidateRange(1, 3)]
    [int]$WriteRepetitions = 3,

    [string]$OutputRoot = (Join-Path $PSScriptRoot '..\test-artifacts\profile-switch-transport'),
    [string]$CaptureRepo = (Join-Path $PSScriptRoot '..\..\tshark_mouse'),

    [switch]$Execute,
    [switch]$Capture,
    [switch]$AllowProfileWrites,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Manifest = Join-Path $RepoRoot 'Cargo.toml'
$ProbeExecutable = Join-Path $RepoRoot 'target\debug\examples\profile-switch-transport-probe.exe'
$RunRoot = Join-Path $OutputRoot (Get-Date -Format 'yyyyMMdd-HHmmss')
$CaptureOutput = Join-Path $RunRoot 'captures'
$PlanPath = Join-Path $RunRoot 'capture-plan.json'

if ($Execute -and $Suite -in @('switches', 'all') -and -not $AllowProfileWrites) {
    throw "Suite '$Suite' sends profile-control reports. Re-run with -AllowProfileWrites after reviewing the plan without -Execute."
}

$cases = @()
if ($Suite -in @('read-only', 'all')) {
    $cases += [pscustomobject]@{ Name = 'baseline'; Case = 'baseline'; Repetitions = 3; Writes = $false }
    $cases += [pscustomobject]@{ Name = 'metadata-read'; Case = 'metadata-read'; Repetitions = $ReadRepetitions; Writes = $false }
    $cases += [pscustomobject]@{ Name = 'polling-read'; Case = 'polling-read'; Repetitions = $ReadRepetitions; Writes = $false }
}
if ($Suite -in @('switches', 'all')) {
    $cases += [pscustomobject]@{ Name = 'idempotent-switch'; Case = 'idempotent-switch'; Repetitions = $WriteRepetitions; Writes = $true }
    $cases += [pscustomobject]@{ Name = 'raw-edge'; Case = 'raw-edge'; Repetitions = $WriteRepetitions; Writes = $true }
    $cases += [pscustomobject]@{ Name = 'verified-edge'; Case = 'verified-edge'; Repetitions = $WriteRepetitions; Writes = $true }
}

function New-ProbeArguments {
    param([Parameter(Mandatory = $true)]$Case)

    $arguments = @(
        '--transport', $Transport,
        '--case', $Case.Case,
        '--repetitions', [string]$Case.Repetitions,
        '--target-profile', '2'
    )
    if ($Execute) {
        $arguments += '--execute'
    }
    if ($Case.Writes -and $AllowProfileWrites) {
        $arguments += '--allow-profile-control-writes'
    }
    return $arguments
}

Write-Host '=== Profile-switch transport comparison ==='
Write-Host "Transport: $Transport"
Write-Host "Suite: $Suite"
Write-Host "Hardware execution: $Execute"
Write-Host "USBPcap capture: $Capture"
Write-Host ''
Write-Host 'Cases:'
foreach ($case in $cases) {
    $arguments = New-ProbeArguments -Case $case
    Write-Host "  $($case.Name): $ProbeExecutable $($arguments -join ' ')"
}
if ($Suite -in @('switches', 'all')) {
    Write-Host '  preparation (outside capture): verify maximum=5 and normalize current profile to 1'
}

if (-not $Execute) {
    Write-Host ''
    Write-Host 'PLAN ONLY: no build, capture, or hardware access occurred.'
    Write-Host 'Add -Execute to run. Switch suites additionally require -AllowProfileWrites.'
    return
}

if (-not $SkipBuild) {
    Write-Host "`nBuilding probe example..."
    & cargo build --manifest-path $Manifest -p attack-shark-x3 --example profile-switch-transport-probe
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}
if (-not (Test-Path $ProbeExecutable)) {
    throw "Probe executable not found at $ProbeExecutable; run without -SkipBuild"
}
if ($Suite -in @('switches', 'all')) {
    Write-Host "`nPreparing profile-1 baseline before starting the capture..."
    $preparationArguments = @(
        '--transport', $Transport,
        '--case', 'prepare-profile-one',
        '--repetitions', '1',
        '--target-profile', '2',
        '--execute',
        '--allow-profile-control-writes'
    )
    & $ProbeExecutable @preparationArguments
    if ($LASTEXITCODE -ne 0) {
        throw "profile-1 preparation failed with exit code $LASTEXITCODE; capture was not started"
    }
}

New-Item -ItemType Directory -Force -Path $RunRoot | Out-Null

$plan = @(
    foreach ($case in $cases) {
        $arguments = New-ProbeArguments -Case $case
        @{
            name = "$Transport-$($case.Name)"
            command = @($ProbeExecutable) + $arguments
        }
    }
)
$plan | ConvertTo-Json -Depth 5 | Set-Content -Path $PlanPath -Encoding utf8
Write-Host "Capture plan: $PlanPath"
Write-Host ''
Write-Host 'During every case, move only the tested mouse continuously in smooth circles'
Write-Host 'from the countdown until the probe prints STOP.'

if ($Capture) {
    $ResolvedCaptureRepo = (Resolve-Path $CaptureRepo).Path
    New-Item -ItemType Directory -Force -Path $CaptureOutput | Out-Null
    Write-Host "`nStarting one USBPcap batch session. Approve the UAC prompt when it appears."
    Push-Location $ResolvedCaptureRepo
    try {
        & uv run python -m tshark_mouse batch `
            --plan $PlanPath `
            --output-dir $CaptureOutput `
            --all-interfaces `
            --all-traffic `
            --include-get-reports `
            --stop-on-error `
            --no-parse `
            --post-delay 2
        if ($LASTEXITCODE -ne 0) {
            throw "capture batch failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
    Write-Host "`nCaptures: $CaptureOutput"
}
else {
    foreach ($case in $cases) {
        $arguments = New-ProbeArguments -Case $case
        Write-Host "`n==> $($case.Name)"
        & $ProbeExecutable @arguments
        if ($LASTEXITCODE -ne 0) {
            throw "probe case '$($case.Name)' failed with exit code $LASTEXITCODE"
        }
    }
}

Write-Host "`nCompleted. Run artifacts: $RunRoot"
