# Battery status

The X3/M600 FA60 receiver reports battery level through an autonomous HID
interrupt IN report on endpoint `0x83`. The level is on a **0–10 scale**
(not 0–100). The stock X3.exe multiplies by 10 for percentage display.
This is confirmed by disassembly of X3.exe's message handler and a live
capture showing `03 10 40 01 0a` while the mouse was at 100% charge.

## Confirmed X3/M600 FA60 receiver format

```text
03 10 40 01 <level>
│  │  │  │  └─ battery level (0–10 scale; 0x0a = 10 → 100%)
│  │  │  └──── battery status flag (0x01 observed)
│  │  └─────── presence/connection flag (0x40)
│  └────────── model ID (0x10 = 16)
└───────────── report tag (0x03)
```

- **Interface**: 2
- **Endpoint**: `0x83` (Interrupt IN)
- **Transfer Type**: Interrupt (autonomous device push, no host request needed)
- **Level encoding**: Byte 4 is 0–10; multiply by 10 for percentage
- **Charging flag**: Delivered in the low byte of the DLL's PostMessage lParam
  (0 = charging, nonzero = discharging)

## Compatibility

| Variant | Path | Status | Evidence |
|:--------|:-----|:-------|:---------|
| X3/M600 FA60 receiver | Endpoint `0x83` interrupt IN, report `03 10 40 01 <level>` | **Confirmed**: level 0–10, ×10 = percentage | disassembly + capture 2026-07-24 |
| X11 receiver | Endpoint `0x83` interrupt IN, report `03 55 40 01 <pct>` | Established: level 0–100 directly | static-analysis + implementation |
| X3/FA61 wired | Auxiliary collection (usage_page `0x000a`, Col03) exists but emits no battery reports | **Confirmed unavailable**: firmware suppresses battery telemetry when wired; DPI button reports (`03 00 10 <stage> 00`) DO arrive on this collection | live-capture 2026-07-24 |
| X3/M600 BLE | GATT `0x180f` / `0x2a19` | Read + notify confirmed | live-confirmed |

## Delivery mechanism (X3.exe)

The hiddriver DLL's background thread reads the interrupt IN report and posts
it to X3.exe via `PostMessageA`:

```text
hiddriver_2.dll background thread (RVA 0x1560):
  overlapped ReadFile on endpoint 0x83
  → parses 5-byte input report
  → PostMessageA(hwnd, WM_USER 0x4010, wParam, lParam)
  → X3.exe window procedure at 0x41339e
```

**lParam encoding:**

| Bits | Field | Values |
|------|-------|--------|
| 15–8 | Battery level | 1–10 (0–10 scale) |
| 7–0 | Charging flag | 0 = charging, ≠0 = discharging |

## X3.exe decode (disassembly)

```asm
; X3.exe:0x41339e — message dispatch
41339e: cmp eax, 0x4010          ; WM_USER battery message
4133a3: jne 0x4134db

; Unpack lParam
413418: mov edx, [ebp+0xc]      ; edx = lParam from DLL
413423: shr eax, 0x8            ; high byte = battery level (0–10)
413426: movzx esi, al           ; esi = level
413429: test dl, dl             ; low byte = charging flag
41342b: jne 0x41343b            ; nonzero → discharging

; Range validation
41344f: cmp esi, 1              ; level >= 1?
413452: jb 0x41354d             ; skip if 0
413458: cmp esi, 0xa            ; level <= 10?
41345b: ja 0x41354d             ; skip if > 10

; Convert to percentage
41346d: lea esi, [esi+esi*4]   ; esi = level × 5
413470: add esi, esi            ; esi = level × 10  ← PERCENTAGE
413472: push esi
413473: call [0x41b0b4]         ; CProgressUI::SetValue(level×10)
```

**Verification offsets:**

| Symbol | Address |
|--------|---------|
| Message compare (`cmp eax, 0x4010`) | `X3.exe:0x41339e` |
| lParam unpack | `X3.exe:0x413418` |
| Charging flag test | `X3.exe:0x413429` |
| Range check (1–10) | `X3.exe:0x41344f–0x41345b` |
| ×10 conversion | `X3.exe:0x41346d–0x413470` |
| CProgressUI::SetValue call | `X3.exe:0x413473` (IAT `0x41b0b4`) |
| Progress bar control (`ms_1_progress_power`) | string VA `0x0041eecc`, ptr `[edi+0x3d4]` |
| Label control (`ms_1_label_power`) | string VA `0x0041eef4`, ptr `[edi+0x39c]` |
| DLL PostMessageA import | `hiddriver_2.dll:0x1000e158` |
| DLL window handle global | `0x10014104` |
| DLL message ID global | `0x10014108` |
| DLL ReadFile import | `hiddriver_2.dll:0x1000e080` |
| DLL background thread | RVA `0x1560` |

## Device behavior

1. **Autonomous emission**: The mouse/receiver pushes battery reports periodically
   on endpoint `0x83` (HID usage_page `0x000a`, interface 2, Col03). No
   `SET_REPORT` or handshake is required. Reports arrive roughly every 10–15
   seconds while the mouse is active.
2. **Wireless only**: This report is only active on the 2.4 GHz receiver. Wired
   mode does not emit battery reports on this endpoint.
3. **0–10 scale**: Byte 4 ranges from 1 to 10. Multiply by 10 for percentage.
   X3.exe validates the range and discards values outside 1–10.
4. **Suppressed while charging**: When the mouse detects VBUS (plugged into any
   USB power source), the firmware stops emitting the battery telemetry report.
   All other 2.4 GHz paths remain fully active — pointer, buttons, DPI button
   events, and SetFeature command/response all continue working normally.
   Confirmed live: DPI read and polling-rate read succeed over the receiver
   while the mouse charges from an external USB-C charger.
5. **Stock app "sleep" misnomer**: The stock X3.exe interprets absence of the
   battery pulse as "device sleep." Since charging suppresses only this one
   report, the app shows "sleep" even though the mouse is fully functional.
   This is a stock-app UI bug, not an actual sleep state.
6. **Charging flag (X3.exe lParam)**: The low byte of the DLL's PostMessage
   lParam (0 = charging, nonzero = discharging) is likely only meaningful in
   wired mode. On the receiver path, byte 3 of the report is always `0x01`
   (discharging) — the report simply stops arriving when charging, rather than
   changing its flag byte.
7. **USB-A to USB-C power cycling quirk**: Connecting the mouse to a USB-A
   charger via a USB-A-to-USB-C cable causes intermittent power on/off cycling
   (LED and cursor flicker). The USB-C receptacle's CC pins expect an Rp
   pull-up from the source; USB-A cables don't carry CC, so the power
   management IC repeatedly fails negotiation and resets. USB-C to USB-C
   chargers work stably.

## X11 legacy format (different signature)

The X11 receiver uses a different report signature with a direct 0–100 scale:

```text
03 55 40 01 <pct>    (pct = 0–100 directly)
```

The Rust decoder accepts both signatures: `03 55 40 01` (X11, 0–100) and
`03 10 40 01` (X3/M600, 0–10 requiring ×10).

## Web driver bug

The `WebDriver.exe` JS bundle applies ×10 only for device 135 (M740):

```javascript
const percent = (deviceNo === 135) ? level * 10 : level;
```

For device 0x10 (X3, detected as "R1"), it uses the raw 0–10 value directly
as a percentage — reporting 10% when the mouse is actually at 100%. The stock
X3.exe correctly applies ×10 for all devices.

## Implementation guide (Rust)

```rust
/// Parse a battery input report from endpoint 0x83.
/// Returns (level_percent, is_charging) or None if not a battery report.
fn parse_battery_report(data: &[u8]) -> Option<(u8, bool)> {
    if data.len() < 5 {
        return None;
    }
    match (data[0], data[1], data[2], data[3]) {
        // X3/M600 FA60: level is 0–10 scale
        (0x03, 0x10, 0x40, 0x01) => {
            let level = data[4];
            if level >= 1 && level <= 10 {
                Some((level * 10, false))
            } else {
                None
            }
        }
        // X11 legacy: level is 0–100 directly
        (0x03, 0x55, 0x40, 0x01) => {
            Some((data[4], false))
        }
        _ => None,
    }
}
```

Note: The charging flag is not available from the raw HID report alone on the
X3 path — it is extracted by the hiddriver DLL from additional report context
before posting to X3.exe. If implementing direct HID reads (bypassing the DLL),
charging state may need a separate mechanism or may not be available.

## Technical summary

- **X3/M600 USB Interface**: 2
- **X3/M600 Endpoint**: `0x83` (Interrupt IN)
- **X3/M600 Report**: `03 10 40 01 <level>` (level 1–10)
- **X3/M600 Conversion**: level × 10 = percentage
- **X11 Report**: `03 55 40 01 <pct>` (pct 0–100)
- **Update frequency**: Periodic / event-driven (autonomous push)
- **Confirmation**: X3.exe disassembly + 2026-07-24 FA60 capture
