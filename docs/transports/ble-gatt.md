# X3 / M600 BLE GATT transport

This is the canonical reference for X3-family BLE services, writes, and ACK behavior. Model-specific packet layouts are defined under [`../protocols/`](../protocols/README.md); investigation chronology and provenance live under [`../research/`](../research/README.md).

## Device identity

- **SoC**: RivieraWaves BLE stack (`"RivieraWaves SAS"`, model `"RW-BLE-1.0"`)
- **Sensor**: PixArt PAW3395
- **ODM**: XinMaiGao (鑫迈高), build date Sept 25, 2023
- **RF-mode / host-slot names**: `M600-5.2` and `M600-5.4` are **not firmware versions** — they are RF-mode slot names selected by `NVDS_TAG_RF_MODE` (tag 56). Both names live in the **same firmware payload**. Value 1 → `M600-5.2` with address family `B6:6B:17:*`; value 2 → `M600-5.4` with address family `B6:6B:18:*`. A **short press** of the pairing button toggles between slots; a **long press** enters BLE pairing for the selected slot. \[live-confirmed + corrected static]
- **USB VID/PID**: `1D57:FA55` (X11 Wired), `1D57:FA60` (2.4GHz Adapter), `1D57:FA61` (X3 Wired)
- **BLE address**: rotating resolvable private address (RPA). Device advertises as `"M600-5.2"` or `"M600-5.4"` depending on the selected RF-mode slot.
- **GATT identity**: Both RF-mode slots expose an identical GATT DB hash `e74e84c928f0a27918414cdc133583e9`, System ID `123456fffe9abcde`, and PnP ID `025e0440000003`. \[live-confirmed]

## Sources

| Source | What it provided |
|--------|-----------------|
| `Update.exe` (M600 firmware update tool) | Firmware debug strings → GM0x protocol layer, BLE stack internals, service UUIDs |
| `Attack_SharkX3Mouse.exe` (stock config app) | USB HID report formats via strings |
| `X3.exe` (same app, binary analysis) | Concrete packet builders at specific addresses, transport dispatcher, BLE exclusion of 0x06 |
| Live BLE probing (`bleak` + Python) | GATT service discovery, write/ACK behavior, packet validation |
| Live USB probing (`node-hid`) | Profile persistence testing |
| `attack-shark-x11-edit` driver codebase | Reference packet builders (DpiBuilder, UserPreferencesBuilder, etc.) |

---

## BLE GATT services

When in BLE pairing mode, the mouse advertises as `"M600-5.2"` or `"M600-5.4"` (depending on the selected RF-mode slot) and exposes:

### Standard services

| Service UUID | Handle | Key Characteristics | Notes |
|-------------|--------|-------------------|-------|
| `0x1800` GAP | `0x0001` | `0x2A00` Device Name → `"M600-5.2"` or `"M600-5.4"` | RF-mode slot name (not a firmware version). Name changes when slot is toggled via pairing-button short press. |
| `0x180A` Device Info | `0x0020` | `0x2A29` Manufacturer → `"RivieraWaves SAS"` | |
| | | `0x2A24` Model → `"RW-BLE-1.0"` | |
| `0x180F` Battery | `0x0029` | `0x2A19` Battery Level → readable + **notify** | Live battery over BLE — works even in pairing mode |
| `0x1812` HID | | HOGP service | Requires bonding/encryption; only visible when paired to OS |

### FEE0 — Custom config service (handle `0x0012`)

This is the GM0x parameter layer. Same protocol as USB HID.

| Char | Handle | Properties | Purpose |
|------|--------|-----------|---------|
| FEE1 | `0x0013` | read | 10-byte opaque readable value. Observed to change with reads and connection activity; semantics unknown. Not config. |
| FEE2 | `0x0016` | write-without-response | Fire-and-forget commands (unused by stock app) |
| **FEE3** | `0x0018` | **write** | **Config write pipe.** Accepts report IDs including 0x04, 0x05, 0x08, 0x09, 0x0c (non-exhaustive). |
| FEE4 | `0x001A` | **notify** | **ACK pipe.** After every FEE3 write, notifies with `10 50 <status> <report_id>`. |
| FEE5 | `0x001D` | indicate | Indication pipe. Rarely fires; stock app subscribes to it. |

### FFC0 — Data service (handle `0x0059`)

| Char | Handle | Properties | Notes |
|------|--------|-----------|-------|
| FFC1 | `0x005A` | write, notify | **Speculative** — possibly firmware updates or macro streaming; not accessed during normal probing |
| FFC2 | `0x005E` | write, notify | **Speculative** — possibly firmware updates or macro streaming; not accessed during normal probing |

**FFC0 was never accessed during normal probing.** No code references to these UUIDs exist in the stock `X3.exe` binary. Speculated to be used by the separate firmware updater tool (`Update.exe`). Purely speculative — no safe-use evidence exists. \[inference]

### GATT write wire format (confirmed via binary analysis)

The stock app uses standard Windows `BluetoothGATT*` APIs:
- `BluetoothGATTGetServices` → `BluetoothGATTGetCharacteristics` → filter by UUID `0xFEE3`
- `BluetoothGATTBeginReliableWrite` → `BluetoothGATTSetCharacteristicValue` → `BluetoothGATTEndReliableWrite`
- The `DataSize` field in `BTH_LE_GATT_CHARACTERISTIC_VALUE` controls how many bytes hit the wire. No extra prefix bytes.

Our `write_gatt_char(FEE3, packet)` via `bleak` is equivalent.

---

## Transport-agnostic packets (✅ confirmed)

**The same packet bytes work over USB and BLE.** Confirmed by:

1. Binary analysis: dispatcher at `X3.exe:0x413570` chooses transport but **does not modify the packet buffer**
2. Live testing: sending DPI and prefs packets built by the USB-side drivers over BLE FEE3 — both accepted with `status=0x00`
3. Persistence test: DPI written over BLE survived reconnection to Windows over USB

```
USB:  sendFeatureReport([0x04, 0x38, 0x01, ...52 bytes...])
BLE:  write_gatt_char(FEE3,   [0x04, 0x38, 0x01, ...52 bytes...])
                               ↑ byte-identical ↑
```

---

## Report ID status matrix

| Report | BLE result | Canonical packet reference | Transport notes |
|:-------|:-----------|:---------------------------|:----------------|
| [`0x04`](../protocols/04-dpi.md) DPI | `status=0x00` | X3 dialect | Same packet bytes as USB; 52- and 56-byte forms accepted |
| [`0x05`](../protocols/05-preferences.md) preferences | `status=0x00` | X3 dialect | Light-mode byte `0x00` crashes firmware over BLE; do not send |
| [`0x06`](../protocols/06-polling-rate.md) polling | `status=0x01` | Shared USB layout | Firmware rejects it; stock app explicitly skips BLE |
| [`0x07`](../protocols/07-wakeup-mode.md) wakeup | Untested | Static format only | Behavior remains unconfirmed |
| [`0x08`](../protocols/08-button-mapping.md) buttons | `status=0x00` | X3 dialect | Correct 16-bit checksum accepted; legacy checksum rejected |
| [`0x09`](../protocols/09-custom-macros.md) macro | `status=0x00` | X3 dialect | One packet accepted; full multi-page BLE behavior untested |
| `0x0b` version | No ACK/data | Unknown | One-byte query sent; device name is not a firmware version |
| [`0x0c`](../protocols/0c-profile-reset.md) profile | `status=0x00` | Shared action format | Load confirmed; save parser-accepted but persistence unconfirmed |

---

## Packet dialect ownership

BLE changes the pipe, not the X3 packet bytes. Report `0x04` through `0x0c` field layouts, model differences, integrity checks, and reset sequences are canonical in the corresponding [`protocols/`](../protocols/README.md) pages.

In particular, report `0x0c` does not change RF identity. The profile parser reads RF mode when selecting a layout but does not write it; the physical pairing button changes the RF/host slot. Save ACKs prove parser acceptance only, not EEPROM persistence. See [`0c-profile-reset.md`](../protocols/0c-profile-reset.md).
---

## BLE ACK format (FACT)

After every write to FEE3, FEE4 notifies with exactly 4 bytes:

```
10 50 <status> <report_id>
```

| Status | Meaning |
|--------|---------|
| `0x00` | Accepted / success |
| `0x01` | Rejected / error / unsupported |

- Status `0x00` means the firmware parsed and accepted the packet. Does NOT mean the change took effect (e.g. 0x0c with `05 fa` gets `0x00` but we haven't verified EEPROM write completion).
- Status `0x01` means the firmware rejected the packet (bad checksum, unsupported report, wrong size).
- **No ACK** at all means the write was sent to the GATT layer but the firmware didn't explicitly respond (e.g. 0x0b version query).

---

## FEE1 — opaque readable value (corrected)

FEE1 is a 10-byte read-only characteristic. Its semantics are **unknown**. Observed behaviour:

- Values change between consecutive reads and across connection activity.
- Reads after FEE3 writes may show different values, but this does **not** prove it is a monotonic counter — it could be an ephemeral session token, a rolling log index, or any other opaque state.
- The stock config app never reads FEE1.

**FEE4 ACK is the sole authoritative source of write acceptance.** Do not use FEE1 as acceptance evidence. \[corrected, live-confirmed]

---

## What BLE gives you that USB doesn't

| Feature | USB HID | BLE GATT |
|---------|---------|----------|
| Write config | ✅ | ✅ |
| ACK/error per write | ❌ fire-and-forget | ✅ `10 50 <status> <report>` |
| Read config back | ❌ `getFeatureReport` broken | ❌ FEE1 is opaque, not config |
| RF-mode slot name | ❌ | ✅ device name = `"M600-5.2"` or `"M600-5.4"` (not a firmware version) |
| Battery | ❌ (wired mode silent) | ✅ read + notify on `0x2A19` |
| Profile save feedback | ❌ | ✅ `status=0x00` on `05 fa` |
| Transport feedback during dev | none | ACK distinguishes bad checksum vs valid packet |

---

## API shape recommendation (derived from findings)

```ts
interface X3Transport {
  kind: "usb-wired" | "usb-dongle" | "ble-gatt";

  open(): Promise<void>;
  close(): Promise<void>;

  /** Write a packet. Report ID is byte 0. Same bytes for all transports. */
  writePacket(packet: Uint8Array): Promise<WriteResult>;
}

interface WriteResult {
  /** Did the transport layer accept the write? */
  accepted: boolean;

  /** BLE only: firmware ACK status (0x00 = ok, 0x01 = rejected) */
  ackStatus?: number;
}
```

Key insight: the builder/transport separation remains valid — builders produce byte arrays while only the pipe changes. However, builders must emit the correct model dialect: X3 requires 16-bit checksums for 0x05/0x08 (the existing builders need model-specific checksum fixes).

**USB path** (stable, product-facing): `node-hid`, fire-and-forget, no feedback.

**BLE path** (experimental, dev/diagnostics): `bleak`/Web Bluetooth, gives ACK feedback per write. Already proven with standalone `ble-probe.py` harness.

---

## Open / untested

| Item | Status |
|------|--------|
| Profile slot count | Unknown: `M_ProfileMax` from firmware strings, at least 2 confirmed |
| Profile save timing | Unknown: how long firmware takes to flush RAM to EEPROM |
| 0x07 wakeup mode behavior | Untested but format known: `[07, 08, mode, ~mode, 00, FF, 00, 00]` (webHID bundle). Read path uses 0x0C sub-cmd `07 08`. |
| RF power (`_GM07_rfpower` from firmware strings) | Where it lives is unknown — GM numbering doesn't map 1:1 to report IDs (GM07 ≠ report 0x07) |
| 0x09 multi-packet macro protocol over BLE | Probable: single packet accepted with `status=0x00` |
| FFC0 service purpose | Speculative: possibly firmware updates or factory calibration; not accessed during normal probing |
| Read-back path for settings | Not found: FEE1 is opaque, no other readable config char discovered |
| BLE bonding impact | Untested: native access to an already-connected/bonded device through the OS BLE stack |
| Stock app's `BeginReliableWrite` significance | Unclear: app wraps writes in reliable-write pair but passes NULL context |

---

## Tooling

- `ble-probe.py` — standalone BLE diagnostic harness (`uv run ble-probe.py`)
- `attack-shark-x11-edit` — driver with protocol builders for USB HID
- `X3.exe` binary analysis — reverse-engineered packet builders at known addresses
- Live probes tested via `bleak` (Python) and `node-hid` (Node.js)
