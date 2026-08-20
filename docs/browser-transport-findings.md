# Browser Transport & RF-Mode Identity Findings

**Date:** 2026-07-10
**Scope:** BLE GATT, Web Bluetooth, WebHID, WebUSB transport viability for Attack Shark X3 / Kysona M600 configuration tooling, plus corrected RF-mode identity analysis.

## Evidence legend

| Tag | Meaning |
|-----|---------|
| **live-confirmed** | Observed on a live device via test harness or interactive probe |
| **corrected static** | Prior static analysis was wrong; corrected by re-analysis of the same artifact |
| **inference** | Reasonable deduction from circumstantial evidence; not directly observed |

---

## 1. Executive summary

- The firmware persists `NVDS_TAG_RF_MODE` = 56. Value **1** advertises as `M600-5.2` with address family `B6:6B:17:*`. Value **2** advertises as `M600-5.4` with address family `B6:6B:18:*`.
- A short press of the pairing button toggles between RF-mode slots; a long press enters BLE pairing for the selected slot. Short-press toggling is confirmed by user; long-press pairing behaviour was observed and used operationally.
- Both modes expose an identical GATT DB hash `e74e84c928f0a27918414cdc133583e9`, System ID `123456fffe9abcde`, and PnP ID `025e0440000003`.
- This is **not** a firmware update. Both device names (`M600-5.2` and `M600-5.4`) live in the same embedded firmware payload. No firmware-updater execution or FFC writes were found during the identity-toggle episode.

---

## 2. Corrected firmware-static evidence

### 2.1 Updater binary structure (corrected)

The updater `BIN/129` (the embedded firmware payload segment) uses a **32-data-byte + 2-byte CRC-16/CMS** record format. Prior linear disassembly treated the binary as flat ARM code; that was invalid and produced spurious opcode decodes.

### 2.2 ARM/Thumb analysis

Corrected ARM/Thumb disassembly proves:

- An **RF mode name selector** reads `NVDS_TAG_RF_MODE` (tag 56) and branches to device-name strings `M600-5.2` (mode 1) or `M600-5.4` (mode 2).
- NVDS persistence commits the value to non-volatile storage.
- The physical key (pairing button) toggles 1 ↔ 2; the pairing/mode-transition path commits the new value.

### 2.3 Report behaviour

Reports **0x05** (user preferences), **0x08** (button mapping), and **0x0c** (profile) do **not** directly write RF mode. Profile/report code reads RF mode to select its packet layout; separate advertising code reads it to select the device name.

---

## 3. Incident chronology and confounds

1. **prefs 0x05 checksum A/B** — accepted and rejected on 5.2 slot.
2. **button 0x08 checksum A/B** — accepted and rejected on 5.2 slot.
3. Stock button mappings were restored.
4. A later pairing-button action toggled the slot to **5.4**.
5. No successful **0x0c** (profile) write preceded the first 5.4 observation.

**Confound:** The mouse was powered via a USB power brick providing continuous 5 V, which explained apparent "off-but-alive" behaviour (device remained advertising even when switched off). The 5 V itself was normal USB bus power, not overvoltage.

---

## 4. Checksum facts

### 4.1 Report 0x05 (user preferences) — X3

```
checksum = sum(bytes[3..10])   // 16-bit big-endian at bytes[11..12]
```

### 4.2 Report 0x08 (button mapping) — X3

```
checksum = (sum(bytes[2..56]) - 1) & 0xffff   // 16-bit big-endian at bytes[57..58]
```

### 4.3 ACK evidence summary

| Packet | Slot | Checksum variant | ACK status | Evidence |
|--------|------|-----------------|------------|----------|
| 0x05 prefs | 5.2 | X3 16-bit | `10 50 00 05` | live-confirmed |
| 0x05 prefs | 5.2 | Legacy 8-bit/state-byte | `10 50 01 05` | live-confirmed |
| 0x08 buttons | 5.2 | X3 16-bit | `10 50 00 08` | live-confirmed |
| 0x08 buttons | 5.2 | Legacy low-byte-only | `10 50 01 08` | live-confirmed |

FEE4 ACK is authoritative for acceptance/rejection. FEE1 is an opaque side-channel, not an ACK substitute.

---

## 5. FEE1 correction

FEE1 is **opaque**. Observed values change between consecutive reads and across connection activity. The prior claim that FEE1 is a monotonic write counter is **not provable** from observed data alone — it could be an ephemeral session token, a rolling log index, or genuinely a counter. FEE4 ACK remains the sole authoritative source of write acceptance.

---

## 6. Web Bluetooth live findings

### 6.1 Probe path

`web-bluetooth-probe/` (served from localhost).

### 6.2 Successful operations

- Chrome discovered FEE0, FEE1, FEE3, FEE4, and the battery service.
- Wrote a one-stage 1600 DPI packet; received `10 50 00 04` (ACK success).
- DPI changed on the mouse, confirmed by cursor movement.

### 6.3 Browser constraints

- First permission requires the browser chooser and an advertising device on the selected RF slot.
- Tested Chromium builds **lacked** usable `navigator.bluetooth.getDevices()`; the control was unavailable even after a successful chooser grant.
- Page refresh **loses** the `BluetoothDevice` reference; a new chooser grant is required.
- An explicit or spontaneous disconnect followed by a reconnect failed once the mouse stopped advertising; the chooser then showed no devices.

### 6.4 Viability conclusion

| Use case | Viable? | Notes |
|----------|---------|-------|
| Session-based / one-shot BLE configurator (pure website) | ✅ | Works for a single session; user must grant permission each time |
| Reliable persistent paired-device driver (pure website) | ❌ | Refresh/relink requires re-choosing; device not always advertising |

### 6.5 Hard restrictions

| Restriction | Source |
|-------------|--------|
| Report 0x06 blocked on BLE | Stock app `cmp [eax],0x6; je skip_ble` |
| Report 0x05 byte 3 = 0x00 crashes firmware over BLE | Live-confirmed |
| No FFC0/FFC1/FFC2 access | Stock app never references these; no evidence of safe use |

The web-bluetooth-probe JS enforces these blocks at the UI validation layer.

> The `web-bluetooth-probe/` files are currently untracked in git.

---

## 7. WebHID / WebUSB findings

### 7.1 Probe paths

- Safe inspector: external `webhid-inspect.html` (browser-based HID descriptor enumeration tool; not included in this repo)
- Local probe context: external `webhid-probe/` (not included in this repo; same architecture as the external inspector)

### 7.2 WebHID enumeration

The browser chooser returned **4 logical devices** matching `1D57:FA55/FA60/FA61`. The MI_02 interface collections included:

| Collection | Usage Page | Notes |
|------------|-----------|-------|
| Col03 | `0x000A` (Ordinal) | No declared feature/output reports |
| Col04 | `0x000B` (Telephony) | Config-related descriptor |

However, all relevant `featureReports` and `outputReports` arrays for Col03 and Col04 were **empty** — the browser HID descriptor parser could not enumerate the configuration feature reports.

### 7.3 node-hid comparison

`node-hid` (native) enumerated **7 paths** and correctly identified Col04 as interface 2, usage page `0x000B`. `HidD_SetFeature` / `node-hid` feature-report writes work despite the browser descriptor restrictions.

### 7.4 Viability conclusion

| Approach | Config write? | Notes |
|----------|--------------|-------|
| WebHID | ❌ | Can enumerate/open but cannot issue undeclared config feature reports |
| WebUSB | ❌ | HID interfaces are OS-claimed; WinUSB swap not recommended |
| node-hid (native) | ✅ | Works; usable from Electron main process or native sidecar |

---

## 8. Product architecture implication table

| Architecture | Transport | Viability | Notes |
|-------------|-----------|-----------|-------|
| Public website | BLE (Web Bluetooth) | Session-based one-shot configurator | Requires chooser each session; no persistent pairing |
| Electron — wired | USB HID (node-hid) | ✅ | node-hid in main/native side; stable, production-grade |
| Electron — robust BLE | OS-native BLE stack | ✅ | Bleak (Python) or Rust/native adapter reference; Web Bluetooth as fallback |
| Shared packet core | — | ✅ | `MouseTransport` abstraction; same dialect bytes for all transports |

All architectures share a pure packet/dialect core behind a `MouseTransport` interface.

---

## 9. Evidence / artifact paths

| Artifact | Path | Status |
|----------|------|--------|
| BLE probe harness | `ble-probe.py` | Active; `uv run ble-probe.py` |
| BLE results | `ble-results.jsonl` | Archive |
| BLE recovery results | `ble-recovery-results.jsonl` | Archive |
| Web Bluetooth probe | `web-bluetooth-probe/` | Active; served from localhost |
| WebHID inspector | external `webhid-inspect.html` (not in repo) | Reference |
| WebHID local probe | external `webhid-probe/` (not in repo) | Reference |
| BLE protocol doc | [`docs/ble-protocol.md`](ble-protocol.md) | Reconciled with this report |
| Updater binaries | See [reverse-engineering-notes.md](reverse-engineering-notes.md) | Summarized |
| Historical session DB snapshot | external / private; summarized in [reverse-engineering-notes.md](reverse-engineering-notes.md) | Not included |

---

## 10. Remaining questions

| Question | Status |
|----------|--------|
| Sleep / deep-sleep behaviour | Untested — power-brick confound may mask normal behaviour |
| LED 0 controlled retest | Not performed |
| Profile save/slot semantics | Save timing unknown (RAM-to-EEPROM flush); slot count ≥ 2 (`M_ProfileMax` exact value unknown) |
| Paired native GATT access | Untested — need to enumerate and access an already-connected bonded device through the native OS stack |
| Exact LED-colour → RF-mode mapping | Not mapped; firmware string references exist but colours not correlated |

---

## 11. Corrections to older notes

The following claims from earlier analysis stages are **explicitly retracted**:

| Retracted claim | Correction |
|-----------------|------------|
| `M600-5.2` = V1, `M600-5.4` = V1.1+ (firmware versions) | Both names come from the **same** firmware payload via `NVDS_TAG_RF_MODE` toggle; not version indicators |
| FEE1 is a monotonic write counter | Opaque; changes observed on reads and connection activity; FEE4 ACK is authoritative |
| Profile 0x0c causes identity toggle | Profile/report reads RF mode but does not write it; toggle is from the physical pairing button |
| An ordinary FEE3 firmware update exists | No evidence of firmware writes via FEE3; FFC0 may be the firmware-update service but was never accessed |
| WebHID / WebUSB as viable wired config | WebHID can enumerate/open but cannot issue undeclared feature reports; WebUSB is blocked by OS HID claim |
