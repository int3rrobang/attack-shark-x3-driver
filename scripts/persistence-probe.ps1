[CmdletBinding()]
param(
    # Wired only. The true-cycle barrier depends on the mouse itself being the
    # USB device that disappears on unplug. Over the FA60 receiver the mouse is
    # wireless, so its power state is not visible through receiver USB
    # enumeration (the dongle stays up).
    [ValidateSet('wired')]
    [string]$Transport = 'wired',

    [ValidateRange(1, 5)]
    [int]$Profile = 1,

    [ValidateRange(1, 8)]
    [int]$Stage = 1,

    # Post-write dwell in seconds before the unplug trigger fires. 0 = unplug
    # immediately after the write is read back. Repeat a value to run it more
    # times (e.g. 0,0,0,0,0,1,2,5,15 gives five zero-dwell trials). The real,
    # machine-observed delay is recorded per trial as `actualDelaySeconds` and
    # always includes readback, human reaction, and discovery-poll latency.
    [int[]]$Delays = @(0, 5, 15),

    # Countdown tail at the end of a non-zero dwell, so the unplug moment is
    # predictable without adding delay on top of the dwell. Zero-dwell trials
    # have no countdown. Capped at the dwell length.
    [ValidateRange(1, 10)]
    [int]$CountdownSeconds = 3,

    [ValidateRange(50, 26000)]
    [int]$MarkerBase = 850,

    [ValidateRange(1, 26000)]
    [int]$MarkerStep = 50,

    [ValidateRange(1, 120)]
    [int]$OffSeconds = 5,

    # Post-replug dwell before polling for reappearance. The mouse needs this
    # to finish Windows re-enumeration without discovery polls contending with
    # the HID stack (tight polling here has frozen the cursor).
    [ValidateRange(0, 60)]
    [int]$SettleSeconds = 4,

    [string]$OutputRoot = (Join-Path $PSScriptRoot '..\test-artifacts\persistence-probe'),

    [switch]$Execute,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Manifest = Join-Path $RepoRoot 'Cargo.toml'
$X3Ctl = Join-Path $RepoRoot 'target\debug\x3ctl.exe'
$RunRoot = Join-Path $OutputRoot (Get-Date -Format 'yyyyMMdd-HHmmss')
$ResultsPath = Join-Path $RunRoot 'results.json'

$badDelays = @($Delays | Where-Object { $_ -lt 0 -or $_ -gt 3600 })
if ($badDelays.Count -gt 0) {
    throw "Delays must be between 0 and 3600 seconds; got: $($badDelays -join ', ')"
}

# Every x3ctl invocation is stateless so the probe never mutates the user's
# durable state file. Each call is bounded by a process timeout so a blocked
# hidapi enumeration cannot wedge the script or hold the mouse's HID stack.
function Invoke-X3Ctl {
    param(
        [Parameter(Mandatory = $true)][string[]]$X3CtlArgs,
        [int]$TimeoutSeconds = 20
    )

    $fullArgs = @('--stateless', '--output', 'json', '--transport', $Transport) + $X3CtlArgs

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $X3Ctl
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    foreach ($arg in $fullArgs) {
        $psi.ArgumentList.Add([string]$arg)
    }

    $proc = [System.Diagnostics.Process]::new()
    $proc.StartInfo = $psi
    try {
        $proc.Start() | Out-Null
    } catch {
        throw "failed to start x3ctl: $($_.Exception.Message)"
    }

    $stdoutTask = $proc.StandardOutput.ReadToEndAsync()
    $stderrTask = $proc.StandardError.ReadToEndAsync()

    if (-not $proc.WaitForExit($TimeoutSeconds * 1000)) {
        try { $proc.Kill() } catch { }
        throw "x3ctl $($X3CtlArgs -join ' ') did not exit within ${TimeoutSeconds}s (killed)"
    }

    $code = $proc.ExitCode
    $text = $stdoutTask.GetAwaiter().GetResult()
    $errText = $stderrTask.GetAwaiter().GetResult()

    $json = $null
    if ($text) {
        try { $json = $text | ConvertFrom-Json } catch { }
    }
    if ($null -eq $json) {
        throw "x3ctl $($X3CtlArgs -join ' ') produced no JSON (exit $code):`n$text`n$errText"
    }
    if ($code -ne 0 -or -not $json.ok) {
        $err = if ($json.error) { $json.error } else { $text }
        throw "x3ctl $($X3CtlArgs -join ' ') failed (exit $code): $err"
    }
    return $json
}

function Get-DpiStages {
    param([Parameter(Mandatory = $true)][string]$DeviceId)

    $json = Invoke-X3Ctl @('--device', $DeviceId, '--profile', "$Profile", 'dpi', 'get')
    $value = $null
    $resource = $json.data.resource
    if ($resource.observed -and $resource.observed.value) { $value = $resource.observed.value }
    elseif ($resource.desired -and $resource.desired.value) { $value = $resource.desired.value }
    if ($null -eq $value) {
        throw "dpi get returned no observed/desired DPI value"
    }
    return @($value.stages | ForEach-Object { [int]$_ })
}

function Set-DpiStages {
    param(
        [Parameter(Mandatory = $true)][string]$DeviceId,
        [Parameter(Mandatory = $true)][int[]]$Stages
    )

    $csv = ($Stages | ForEach-Object { [string]$_ }) -join ','
    $null = Invoke-X3Ctl @('--device', $DeviceId, '--profile', "$Profile", 'dpi', 'set', '--stages', $csv)
}

function Get-ConnectedDeviceIds {
    $json = Invoke-X3Ctl @('devices') -TimeoutSeconds 10
    return @($json.data | Where-Object { $_.connected } | ForEach-Object { [string]$_.identity.id })
}

function Wait-DeviceGone {
    param(
        [Parameter(Mandatory = $true)][string]$DeviceId,
        [int]$TimeoutSeconds = 90
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $ids = @(Get-ConnectedDeviceIds)
        if ($ids -notcontains $DeviceId) {
            return (Get-Date)
        }
        Start-Sleep -Milliseconds 200
    }
    throw "device $DeviceId did not disappear within ${TimeoutSeconds}s"
}

# The device ID is not guaranteed to survive a replug (Windows can reassign the
# USB instance hash), so reappearance looks for any connected wired device and
# returns its (possibly changed) ID for the caller to adopt.
function Wait-DeviceBack {
    param([int]$TimeoutSeconds = 120)

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $ids = @(Get-ConnectedDeviceIds)
        if ($ids.Count -gt 0) {
            return $ids[0]
        }
        Start-Sleep -Milliseconds 1000
    }
    throw "no wired device reappeared within ${TimeoutSeconds}s"
}

Write-Host '=== Persistence probe: true power cycle ==='
Write-Host "Transport: $Transport"
Write-Host "Profile: $Profile (stage $Stage)"
Write-Host "Dwells (s): $($Delays -join ', ')"
Write-Host "Unplug countdown tail: ${CountdownSeconds}s (capped at dwell)"
Write-Host "Marker base: $MarkerBase, step: $MarkerStep"
Write-Host "Switch-off dwell: ${OffSeconds}s, post-replug settle: ${SettleSeconds}s"
Write-Host "Hardware execution: $Execute"
Write-Host ''
Write-Host 'Each trial writes one distinctive DPI stage value, waits, then power-cycles'
Write-Host 'the mouse. A USB unplug alone is NOT a real power cycle for the X3: its'
Write-Host 'battery keeps the MCU alive, and VBUS keeps it alive even with the switch'
Write-Host 'off. The true cycle is: UNPLUG -> switch OFF -> wait -> switch ON -> replug.'
Write-Host 'Wired-only: over the FA60 receiver the mouse is wireless, so a mouse'
Write-Host 'power cycle is not observable through receiver USB enumeration.'
Write-Host ''
Write-Host 'Timing is wall-clock and includes readback, human reaction, and ~0.2-1 s'
Write-Host 'discovery-poll latency, so sub-second commit latency cannot be resolved.'
Write-Host 'A zero-dwell trial is the fastest human-unplug cycle possible. Repeat a'
Write-Host 'delay in -Delays (e.g. 0,0,0,0,0,5,15) to gather more low-dwell trials.'

if (-not $Execute) {
    Write-Host ''
    Write-Host 'PLAN ONLY: no build or hardware access occurred.'
    Write-Host 'Add -Execute to run against live hardware.'
    return
}

if (-not $SkipBuild) {
    Write-Host "`nBuilding x3ctl..."
    & cargo build --manifest-path $Manifest -p x3ctl
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}
if (-not (Test-Path $X3Ctl)) {
    throw "x3ctl not found at $X3Ctl; run without -SkipBuild"
}

New-Item -ItemType Directory -Force -Path $RunRoot | Out-Null

$ids = @(Get-ConnectedDeviceIds)
if ($ids.Count -eq 0) {
    throw "no connected wired device found"
}
if ($ids.Count -gt 1) {
    throw "multiple connected wired devices; pass --device to disambiguate"
}
$deviceId = $ids[0]
Write-Host "`nDevice: $deviceId"

$originalStages = @(Get-DpiStages -DeviceId $deviceId)
Write-Host "Original stages: $($originalStages -join ',')"

if ($Stage -gt $originalStages.Count) {
    throw "stage $Stage exceeds the configured stage count $($originalStages.Count)"
}

$trials = @()
$idChanged = $false
$trialIndex = 0
foreach ($delay in $Delays) {
    $trialIndex++
    $marker = $MarkerBase + (($trialIndex - 1) * $MarkerStep)
    if ($marker -gt 26000) { $marker = 26000 }

    Write-Host "`n=== Trial $trialIndex/$($Delays.Count): marker=$marker, dwell=${delay}s ==="

    $current = @(Get-DpiStages -DeviceId $deviceId)
    $preValue = $current[$Stage - 1]
    if ($marker -eq $preValue) {
        $marker += $MarkerStep
        Write-Host "  marker collided with live value $preValue; using $marker"
    }

    $newStages = @($current)
    $newStages[$Stage - 1] = $marker

    Write-Host "  Writing stage $Stage = $marker ..."
    Set-DpiStages -DeviceId $deviceId -Stages $newStages
    $writeAt = Get-Date

    $liveStages = @(Get-DpiStages -DeviceId $deviceId)
    $liveValue = $liveStages[$Stage - 1]
    if ($liveValue -ne $marker) {
        Write-Warning "  live readback $liveValue != marker $marker; firmware may round; using $liveValue as expected"
    }
    $expected = $liveValue
    Write-Host "  Live readback: $liveValue"

    if ($delay -gt 0) {
        $countdown = [math]::Min($delay, $CountdownSeconds)
        $quiet = $delay - $countdown
        Write-Host ''
        Write-Host "  Write done. ${delay}s dwell before unplug:"
        if ($quiet -gt 0) {
            Write-Host "    dwelling ${quiet}s..."
            Start-Sleep -Seconds $quiet
        }
        for ($c = $countdown; $c -ge 1; $c--) {
            Write-Host "      $c ..."
            Start-Sleep -Seconds 1
        }
        Write-Host '      >>> UNPLUG NOW'
        Write-Host '      (VBUS must be gone before the power switch can cut power.)'
    }
    else {
        Write-Host ''
        Write-Host '  >>> UNPLUG NOW (zero dwell: unplug the instant the write is done).'
        Write-Host '      (VBUS must be gone before the power switch can cut power.)'
    }
    $disappearAt = Wait-DeviceGone -DeviceId $deviceId
    Write-Host "  Disappeared at $($disappearAt.ToString('HH:mm:ss.fff'))"

    Write-Host "  >>> Switch the mouse OFF, wait ${OffSeconds}s, switch it ON, then REPLUG the USB-C cable."
    Start-Sleep -Seconds $SettleSeconds
    $returnedId = Wait-DeviceBack
    $reappearAt = Get-Date
    if ($returnedId -ne $deviceId) {
        Write-Warning "  device ID changed on replug (expected; Windows reassigned the USB instance hash):"
        Write-Warning "    old: $deviceId"
        Write-Warning "    new: $returnedId"
        $deviceId = $returnedId
        $idChanged = $true
    }
    Write-Host "  Reappeared at $($reappearAt.ToString('HH:mm:ss.fff'))"

    $postStages = @(Get-DpiStages -DeviceId $deviceId)
    $postValue = $postStages[$Stage - 1]

    $persisted = ($postValue -eq $expected)
    $reverted = ($postValue -eq $preValue)
    $corrupted = (-not $persisted) -and (-not $reverted)

    $actualDelay = [math]::Round(($disappearAt - $writeAt).TotalSeconds, 3)

    $trial = [pscustomobject]@{
        trial                 = $trialIndex
        marker                = $marker
        dwellSeconds          = $delay
        writeAt               = $writeAt.ToString('yyyy-MM-ddTHH:mm:ss.fff')
        liveReadback          = $liveValue
        expectedValue         = $expected
        disappearAt           = $disappearAt.ToString('yyyy-MM-ddTHH:mm:ss.fff')
        reappearAt            = $reappearAt.ToString('yyyy-MM-ddTHH:mm:ss.fff')
        actualDelaySeconds    = $actualDelay
        postCycleValue        = $postValue
        persisted             = $persisted
        revertedToBaseline    = $reverted
        corrupted             = $corrupted
        reappearedDeviceId    = $returnedId
    }
    $trials += $trial

    $status = if ($persisted) {
        'PERSISTED'
    } elseif ($reverted) {
        'REVERTED'
    } else {
        'CORRUPTED'
    }
    Write-Host "  RESULT: $status (marker=$marker, post-cycle=$postValue, actual D=${actualDelay}s)"
}

Write-Host "`nRestoring original stages: $($originalStages -join ',')"
Set-DpiStages -DeviceId $deviceId -Stages $originalStages
Write-Host '  Restored (this restore is not persistence-verified).'

$summary = [pscustomobject]@{
    transport        = $Transport
    barrier          = 'power-cycle'
    profile          = $Profile
    stage            = $Stage
    offSeconds       = $OffSeconds
    countdownSeconds = $CountdownSeconds
    settleSeconds    = $SettleSeconds
    deviceId         = $deviceId
    deviceIdChanged  = $idChanged
    originalStages   = $originalStages
    trials           = $trials
    notes            = @(
        'A true power cycle (unplug + switch off/on) is operator-attested; USB re-enumeration alone is not a real MCU power-down.',
        'actualDelaySeconds is wall-clock from write completion to detected USB disappearance; it includes readback, human reaction, and ~0.2-1 s poll latency.',
        'persisted means the marker survived; revertedToBaseline means it returned to the pre-trial live value; corrupted means it differs from both.',
        'The USB device ID is not stable across replug; the probe adopts the new ID automatically and records deviceIdChanged.',
        'The switch-away/switch-back barrier is covered separately by `x3ctl verify --method profile-reload`.'
    )
}
$summary | ConvertTo-Json -Depth 6 | Set-Content -Path $ResultsPath -Encoding utf8

Write-Host "`nResults: $ResultsPath"
Write-Host 'Completed.'
