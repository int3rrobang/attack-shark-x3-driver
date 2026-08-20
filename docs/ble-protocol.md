# Attack Shark X3 / Kysona M600 — BLE reverse engineering

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

| Report | BLE ACK | USB | Notes |
|--------|---------|-----|-------|
| **0x04** DPI | ✅ `status=0x00` | ✅ | Works unchanged. 52b wired, 56b wireless both accepted. |
| **0x05** Prefs | ✅ `status=0x00` | ✅ | **16-bit checksum** at bytes 11-12 (X3). LED mode byte 3 must be ≥ `0x10` — `0x00` crashes firmware over BLE. (FACT) |
| **0x06** Polling | ❌ `status=0x01` | ✅ | **Explicitly excluded from BLE** in stock app: `cmp [eax],0x6; je skip_ble`. Firmware refuses it. (FACT) |
| **0x07** Wakeup Mode | ❓ untested | ❓ | WebHID bundle: `[07, 08, mode, ~mode, 00, FF, 00, 00]`. Mode 1=button, 2=movement. Sent during factory reset. (FACT via webHID JS) |
| **0x08** Buttons | ✅ `status=0x00` | ✅ | X3 16-bit checksum accepted over BLE; 8-bit/low-byte-only checksum rejected. \[live-confirmed, findings §4.2–4.3] |
| **0x09** Macro | ✅ `status=0x00` | ✅ | Single packet with `09 40` header accepted. `09 83` also accepted as an alternate during BLE probing but its model association is unknown. Full multi-packet macro protocol not tested over BLE. |
| **0x0b** Version | ❓ no response | ❓ | 1-byte query accepted over BLE but no ACK/data returned. Device name is an RF-mode slot identifier, not a firmware version. |
| **0x0c** Profile | ✅ `status=0x00` | ✅ | LOAD confirmed; SAVE accepted by parser/ACKed but EEPROM persistence not confirmed. See below. |

---

## Profile mechanism (✅ confirmed)

Report 0x0c does **not** change RF identity (M600-5.2 vs M600-5.4). The profile/report code reads `NVDS_TAG_RF_MODE` to select the packet layout but never writes it. The RF-mode slot is toggled only by the physical pairing button. \[corrected static]

### Packet structure

```
0c 0a [profile_N] [~profile_N] [action_lo] [action_hi] [padding to 6/10]
```

### Action codes (confirmed sub-commands on 0x0C)

| Bytes 2-3 | Bytes 4-5 | Operation | Evidence |
|-----------|-----------|-----------|----------|
| `01 fe` | `01 fe` | **LOAD** profile 1 from flash to RAM (clears RAM) | Proven: DPI lost after load, re-applied config fixes it |
| `02 fd` | `05 fa` | **SAVE** to profile 2? | ACK=0x00 but persistence not fully confirmed |
| `07 08` | `00 00` | **READ** wakeup mode | WebHID bundle: `sendWakeupModeReadCommand()` sends `[12,7,8,0,0,0,0,0]` under 0x0C. Response comes back via the input/interrupt pipe with `n[2]===7 && n[3]===8` |

### Integrity check

Both profile_N and action fields use `N + ~N == 0xFF` as a simple firmware-level integrity guard. Packets with mismatched complements are rejected.

### Known issues

- **Timing**: firmware needs time (potentially seconds) to flush RAM to EEPROM after a save. Exact timing unknown.
- **Partial state**: `0c 0a 01 fe 01 fe` (LOAD/reset) clears ALL RAM config, not just DPI. After loading a profile, you must re-apply the full config block (DPI + prefs + buttons + macros) or the mouse enters a broken state where settings don't respond.
- **Slot count**: at least 2 confirmed working. The firmware has `M_ProfileMax` (value unknown).
- **`05 fa` is from a different codebase** (WebHID factory tool bundle) but accepted by the X3 parser/ACKed; persistence not confirmed.
- **ACK ≠ persistence**: a SAVE action receiving `status=0x00` on FEE4 means the firmware parsed the packet, **not** that the EEPROM write completed. RAM-to-EEPROM flush timing is unknown. \[inference]

---

## X3 vs X11 protocol differences (FACT)

| Feature | X11 | X3 |
|---------|-----|-----|
| Prefs (0x05) checksum | 8-bit at byte 12, state flag at byte 11 | **16-bit** big-endian at bytes 11-12, no state flag |
| DPI stage count | Exactly 6 | 1–8 |
| DPI encoding | Lookup table (DPI_STEP_MAP) | Direct: `(dpi/50 - 1)` as low/high byte pair |
| Fixed bytes (DPI offsets 25-49) | Last byte `0x02` | Last byte `0x01` |
| Macro (0x09) page 0 & 1 byte 1 | `0x40` | `0x40` |
| Macro (0x09) page 2 byte 1 | `0x0c` | `0x40` |

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
