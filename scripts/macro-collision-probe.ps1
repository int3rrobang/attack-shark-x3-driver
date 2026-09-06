[CmdletBinding()]
param([int]$ListenSeconds = 8)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class KeyPoll {
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
}
"@

function Wait-Key {
    param([int]$Vk, [int]$Seconds)
    $dead = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $dead) {
        if (([KeyPoll]::GetAsyncKeyState($Vk) -band 0x8001) -ne 0) { return $true }
        Start-Sleep -Milliseconds 2
    }
    return $false
}

Write-Host "=== Macro profile collision (flat vs per-profile) ==="
Write-Host "Forward slot 07 is the test button (array slot 7). DPI not needed here."
Write-Host ""

Write-Host "[1/4] Write P1 Forward A (100ms) -> 08 12 00 07 + 09 83 07"
cargo run --quiet -p attack-shark-x3 --example macro-wired -- --profile 1 --slot 7 --macro press-A --transport wired
if ($LASTEXITCODE -ne 0) { throw "P1 write failed" }
Write-Host "  Press FORWARD now (expect A) — listening ${ListenSeconds}s..."
if (Wait-Key -Vk 0x41 -Seconds $ListenSeconds) { Write-Host "  -> A fired (P1 ok)" } else { Write-Host "  -> TIMEOUT (no A)" ; throw "P1 A did not fire" }

Write-Host ""
Write-Host "[2/4] Write P2 Forward B (100ms) -> 08 12 00 07 + 09 83 07"
cargo run --quiet -p attack-shark-x3 --example macro-wired -- --profile 2 --slot 7 --macro press-B --transport wired
if ($LASTEXITCODE -ne 0) { throw "P2 write failed" }
# Ensure current is P2 for the test (macro-wired does not switch current, so activate P2)
cargo run --quiet -p attack-shark-x3 --example profile-activate -- --profile 2 --transport wired | Out-Host
Write-Host "  Press FORWARD now (expect B) — listening..."
if (Wait-Key -Vk 0x42 -Seconds $ListenSeconds) { Write-Host "  -> B fired (P2 ok)" } else { Write-Host "  -> TIMEOUT (no B)" ; throw "P2 B did not fire" }

Write-Host ""
Write-Host "[3/4] Activate P1 WITHOUT re-apply (tests flat vs per-profile) — collision point"
cargo run --quiet -p attack-shark-x3 --example profile-activate -- --profile 1 --transport wired | Out-Host
Write-Host "  Press FORWARD now — listening 8s for A vs B..."
$hitA = Wait-Key -Vk 0x41 -Seconds $ListenSeconds
$hitB = $false
if (-not $hitA) { $hitB = Wait-Key -Vk 0x42 -Seconds 2 } # quick check if B instead (need separate window)
# Better: poll both simultaneously for one window
# Redo as joint poll if first missed B
if (-not $hitA -and -not $hitB) {
    $dead = (Get-Date).AddSeconds($ListenSeconds)
    while ((Get-Date) -lt $dead) {
        if (([KeyPoll]::GetAsyncKeyState(0x41) -band 0x8001) -ne 0) { $hitA = $true; break }
        if (([KeyPoll]::GetAsyncKeyState(0x42) -band 0x8001) -ne 0) { $hitB = $true; break }
        Start-Sleep -Milliseconds 2
    }
}
if ($hitA -and -not $hitB) {
    Write-Host "  -> A fired again => device kept per-profile (unexpected, contradicts flat 0x7D000+slot*0x80)"
    $result = "per-profile"
} elseif ($hitB) {
    Write-Host "  -> B fired => FLAT last-write-wins (device global, host per-profile cache only) — matches x3-exe-09:3946"
    $result = "flat"
} else {
    Write-Host "  -> TIMEOUT (neither A nor B)"
    $result = "timeout"
}

Write-Host ""
Write-Host "=== RESULT: $result ==="
Write-Host "Flat = PC's 13×0x80E0 per-profile save is host illusion, mouse holds 18 flat slots at 0x7D000+mm*0x80."
Write-Host "Restore: re-apply your original profile if needed via macro-wired or x3ctl."
# save
$result | Set-Content -Path "test-artifacts/macro-probe/collision-$((Get-Date).ToString('yyyyMMdd-HHmmss')).txt" -Encoding utf8
Write-Host "Done."
