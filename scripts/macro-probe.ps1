[CmdletBinding()]
param(
    [string]$OutputRoot = 'test-artifacts/macro-probe',
    [ValidateSet('wired','receiver')]
    [string]$Transport = 'wired',
    [int[]]$Profiles = @(1,2),
    [ValidateSet('A','B','all')]
    [string]$Slots = 'all',
    [switch]$IncludeDpi,
    [switch]$Execute,
    [switch]$AllowMacroWrites,
    [switch]$SkipBuild,
    [int]$ListenSeconds = 5
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Manifest = Join-Path $RepoRoot 'Cargo.toml'
$RunRoot = Join-Path $OutputRoot (Get-Date -Format 'yyyyMMdd-HHmmss')
$PlanPath = Join-Path $RunRoot 'plan.json'
$LogPath = Join-Path $RunRoot 'results.json'

if ($Execute -and -not $AllowMacroWrites) {
    throw "Refusing to send macro writes without -AllowMacroWrites. Run without -Execute first to review plan."
}
if ($Transport -eq 'receiver') {
    Write-Warning "Receiver (FA60) macro writes are unproven and share the wired 64B image; ACK is parser-only, not persistence."
}

# Slots to test: wire Button IDs. 01 Left, 02 Right, 03 Middle, 04 DPI (if -IncludeDpi), 07 Forward (slot6), 08 Backward (slot7)
$slotMap = @(
    @{ label='Left';     slot=1; wireId='01'; safe=$true }
    @{ label='Right';    slot=2; wireId='02'; safe=$true }
    @{ label='Middle';   slot=3; wireId='03'; safe=$true }
    @{ label='Forward';  slot=7; wireId='07'; safe=$true }
    @{ label='Backward'; slot=8; wireId='08'; safe=$true }
)
if ($IncludeDpi) {
    $slotMap += @{ label='DPI'; slot=4; wireId='04'; safe=$true } # live-confirmed 0x08 arbitrary safe actions, macro unproven but safe to test
}
# No wheel slots 4*? (array indices 5,6 offsets 15/18 -> 0x3c) — never emit, repeats until unplug per safety.md
if ($Slots -eq 'A') { $slotMap = $slotMap | Where-Object { $_.label -in @('Forward') } }
elseif ($Slots -eq 'B') { $slotMap = $slotMap | Where-Object { $_.label -eq 'DPI' } }

$macroKinds = @(
    @{ name='press-A'; desc="press A 100ms + release A 100ms (04 01 6400 / 04 02 6400 -> round 0A/8A probe)"; keys=@('04') }
    @{ name='press-B'; desc="press B 100ms + release B 100ms (05 01 6400 / 05 02 6400 -> round 0A/8A probe)"; keys=@('05') }
)

Write-Host "=== Macro wired probe (report 0x09 + 0x08 bind 12 00 <slot>) ==="
Write-Host "Transport: $Transport (FA61 wired is primary, FA60 receiver is optional second pass)"
Write-Host "Profiles: $($Profiles -join ',') (custom driver 1..5, stock X3.exe has 13 blocks hidden)"
Write-Host "Slots: $(($slotMap | ForEach-Object { "$($_.label)=$($_.wireId) (array slot $($_.slot))" }) -join ', ')"
if ($IncludeDpi) { Write-Host "  DPI (04) included for completeness — stock never binds anything to DPI, but slot 3 is safe per 08-button-mapping.md:68" }
Write-Host "Macros: $(($macroKinds | ForEach-Object { $_.name }) -join ', ')"
Write-Host "Hardware execution: $Execute (requires -AllowMacroWrites)"
Write-Host ""
Write-Host "Safety: one safe field at a time from known-good 10ms A/B card, backup 0x08 59B table, 500ms gaps like reset_packets_x3.json, recovery restore after each profile."
Write-Host "No wheel slots 4/5 (0x3c) — repeats until unplug. No 01..08 sweep — only enumerated 01,02,03,04,07,08."
Write-Host "D6 framing: Wired -> P2 09 40 tail, Receiver -> P2 09 0C tail, BLE -> raw 09 83 to FEE3 (not tested here)."
Write-Host ""

$cases = @()
foreach ($profile in $Profiles) {
    foreach ($slot in $slotMap) {
        foreach ($mk in $macroKinds) {
            $cases += [pscustomobject]@{
                profile = $profile
                slot = $slot.slot
                wireId = $slot.wireId
                label = $slot.label
                macro = $mk.name
                desc = $mk.desc
                bytes = "08 12 00 $($slot.wireId) + 09 83 $($slot.wireId) 00 00 00 01 .. 02 01 04 81 04 .. sum16[3..128] BE (131B -> sliced 3x 09 40/09 0C) per x3-exe-09 analysis 0x414340"
            }
        }
    }
}
Write-Host "Cases ($($cases.Count)):"
foreach ($c in $cases) { Write-Host "  P$($c.profile) $($c.label)($($c.wireId)).$($c.macro) : $($c.bytes)" }

# Validate profiles 1..5 — catches -Profiles 12 typo (you saw P12). Custom driver supports 1..5, stock X3.exe hides them.
$bad = @($Profiles | Where-Object { $_ -lt 1 -or $_ -gt 5 })
if ($bad.Count -gt 0) { throw "Profiles must be 1..5 (custom driver). Got: $($bad -join ','). Try -Profiles 1,2 not 12." }

if (-not $Execute) {
    Write-Host ""
    Write-Host "Dry-run only. Re-run with -Execute -AllowMacroWrites to send."
    New-Item -ItemType Directory -Force -Path $RunRoot | Out-Null
    $plan = [pscustomobject]@{
        transport = $Transport
        profiles = $Profiles
        slots = $slotMap
        macros = $macroKinds
        cases = $cases
        note = "Each case: backup 0x08 table, send logical 09 83 (131B) sliced to 3x64B HID per transport (wired 09 40*3 Sleep200, receiver 09 40,09 40,09 0C Sleep1000) then 08 12 00 <slot> bind, expect 4x 10 50 00 ACK wired or FEE4 ACK BLE, auto hook listens for synthesized A/B for ${ListenSeconds}s, then restore backup. Check1 collision: P1 Forward A vs P2 Forward B without re-apply -> flat if B persists. Check2 capacity: all 5 + DPI 04. Check3 persistence: power-cycle + press."
    }
    $plan | ConvertTo-Json -Depth 6 | Set-Content -Path $PlanPath -Encoding utf8
    Write-Host "Plan: $PlanPath"
    exit 0
}

# --- Execute path ---
if (-not $SkipBuild) {
    Write-Host "Building x3ctl + macro probe helper..."
    cargo build --manifest-path $Manifest -p attack-shark-x3 --examples 2>&1 | Out-Host
}

New-Item -ItemType Directory -Force -Path $RunRoot | Out-Null

# Auto-hook listener: polls GetAsyncKeyState for A (0x41) / B (0x42) — same hook stock Hook.dll uses, but polling avoids native Hook.dll.
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class KeyPoll {
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
}
"@

function Wait-MacroKey {
    param([string]$Expect, [int]$Seconds)
    $vk = if ($Expect -eq 'press-A') { 0x41 } elseif ($Expect -eq 'press-B') { 0x42 } else { 0x41 }
    $deadline = (Get-Date).AddSeconds($Seconds)
    Write-Host "  Listening ${Seconds}s for '$Expect' — press $($c.label) now (macro 100ms hold, poll 2ms)..."
    while ((Get-Date) -lt $deadline) {
        $st = [KeyPoll]::GetAsyncKeyState($vk)
        if ($st -band 0x8001) { return $true } # down now OR pressed since last call
        Start-Sleep -Milliseconds 2
    }
    return $false
}

$results = @()
foreach ($c in $cases) {
    Write-Host ""
    Write-Host "== P$($c.profile) $($c.label) $($c.wireId) $($c.macro) =="
    $sendArgs = @("--profile", "$($c.profile)", "--slot", "$($c.slot)", "--macro", "$($c.macro)", "--transport", $Transport)
    Write-Host "  sending 4 packets: 09 83 sliced + 08 12 00 $($c.wireId) for P$($c.profile) slot $($c.slot) ..."
    $out = & cargo run --quiet -p attack-shark-x3 --example macro-wired -- @sendArgs 2>&1
    $out | ForEach-Object { Write-Host "    $_" }
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  !! send failed exit $LASTEXITCODE — logging fail & continuing (check 10 50 ACK / hidapi)"
        $hit = $false
        $observed = "send-fail"
    } else {
        $hit = Wait-MacroKey -Expect $c.macro -Seconds $ListenSeconds
        $observed = if ($hit) { "auto:$($c.macro) fired" } else { "auto:timeout" }
        Write-Host "  -> $observed (macro executed by mouse itself, no manual typing)"
    }
    $results += [pscustomobject]@{ case=$c; observed=$observed; transport=$Transport; at=(Get-Date).ToString('o') }
    $results | ConvertTo-Json -Depth 6 | Set-Content -Path $LogPath -Encoding utf8
    Write-Host "  Logged to $LogPath — pause 500ms before next case"
    Start-Sleep -Milliseconds 500
}

Write-Host ""
Write-Host "Completed. Results: $LogPath"
Write-Host "Restore original 0x08 table from backup before unplug. Power-cycle: unplug -> OFF -> wait 2s -> ON -> replug -> press again."
