# Reverse-engineering provenance

This document is the canonical, in-repo summary of unique reverse-engineering facts previously documented only in external reports and artifacts. It preserves provenance without copying stale reports wholesale.

---

## X3.exe native analysis

- **Artifact:** `X3.exe` (stock Attack Shark X3 configuration application)
- **Evidence class:** static-analysis

The binary contains a **byte-identical USB/BLE transport dispatcher**: the same report-building code paths serve both transports from a single set of packet builders. USB HID report IDs and their BLE characteristic equivalents are constructed by the same routines.

Key findings:

- The stock app explicitly excludes report `0x06` from BLE. A corrected same-hardware probe later accepted the exact nine-byte X3 packet over BLE and changed the rate observed after USB reconnect; shorter legacy-shaped packets were rejected.
- Reports `0x05` and `0x08` use X3-specific 16-bit checksums. Canonical formulas and live ACK evidence are maintained in [`05-preferences.md`](../protocols/05-preferences.md) and [`08-button-mapping.md`](../protocols/08-button-mapping.md).
- Report `0x0c` uses complement-pair integrity checks and reads, but does not write, RF mode. Canonical action semantics are maintained in [`0c-profile-reset.md`](../protocols/0c-profile-reset.md).

---

## WebDriver.exe analysis

- **Artifact:** `web-driver/REPORT.md`
- **Evidence class:** static-analysis

The stock WebDriver executable implements a **WebSocket bridge** architecture: it opens the mouse via native HID and exposes packet read/write over a local WebSocket server for a browser-based configuration UI.

PID audit findings:

- **FA60** (X11 2.4GHz adapter): explicitly referenced in the binary as a known PID.
- **FA61** (X3 wired): no explicit `push`-style reference found. Detection of FA61 may rely on a VID fallback path rather than a direct PID match.
- This explains why wired FA61 detection can fail in environments where the driver only matches explicit PID entries.

---

## Firmware updater payload analysis

- **Artifacts:** Updater README and payload files (from the M600 firmware update tool)
- **Evidence class:** static-analysis

The embedded firmware payload segment uses a **32-data-byte + 2-byte CRC-16/CMS** record format. Prior linear disassembly treated the data as flat ARM code, which produced spurious opcode decodes; corrected ARM/Thumb analysis confirmed the record structure.

Identified in the payload:

- Both device-name strings `M600-5.2` and `M600-5.4` live in the **same firmware payload**.
- An **RF-mode name selector** reads `NVDS_TAG_RF_MODE` (tag 56): value **1** selects `M600-5.2`, value **2** selects `M600-5.4`.
- NVDS persistence writes the RF mode to non-volatile storage.

No updater execution or FFC writes were performed in this investigation. The updater applications are Windows GUI/MFC executables that require UAC elevation and communicate via USB HID; they were not executed, and no silent or command-line mode was identified by static inspection. The analysis here is purely static from the payload binary.

---

## USBPcap reset capture

- **Artifact:** `reset_packets_x3.json` (preserved as [`../evidence/x3-fa61/reset-packets.json`](../evidence/x3-fa61/reset-packets.json))
- **Evidence class:** capture-confirmed

A USBPcap capture of the stock Attack Shark X3 software performing a factory reset shows this report order:

1. **0x0c** (internal state reset)
2. **0x04** (DPI)
3. **0x05** (user preferences)
4. **0x06** (polling rate)
5. **0x08** (button mapping)

Inter-packet spacing is approximately **500 ms** between each report. No **0x09** (custom macro definition) pages are sent during stock reset when no macros are dirty.

---

## Historical session evidence

- **Artifact:** Historical OpenCode session DB snapshot (private; not included in this repo)
- **Evidence class:** historical-session (not live/capture proof)

A historical tooling session database recorded observations not directly reproduced in the current investigation:

- Direct GAP reads recorded both `M600-5.2` and `M600-5.4` advertising identities across different moments in the same device's session history.
- No evidence in the historical session data showed `Update.exe` execution or FFC1/FFC2 writes during the identity transition from 5.2 to 5.4.
- The session data is consistent with (though does not independently prove) the static finding that RF-mode toggling via the physical pairing button — not a firmware update — explains the identity change.

**These are historical-session observations, not live/capture proof.** The private DB snapshot is not included in this repository.

---

## Browser experiments

- **Artifact:** [`2026-07-browser-investigation.md`](2026-07-browser-investigation.md)
- **Evidence class:** live-confirmed

For Web Bluetooth, WebHID, and WebUSB viability findings, see the dated [`2026-07 browser investigation`](2026-07-browser-investigation.md). Stable conclusions live in [`../transports/browser.md`](../transports/browser.md). Key conclusions:

- **Web Bluetooth:** Session-based one-shot BLE configurator works; persistent paired-device driver does not (page refresh loses device reference, device not always advertising).
- **WebHID:** Can enumerate and open but cannot issue undeclared feature reports.
- **WebUSB:** Blocked by OS HID claim on the interface.
- **node-hid (native):** Works for all feature-report writes; usable from Electron main process.

---

## Artifact provenance

These are the original external artifact names referenced by the summaries above. Machine-specific absolute paths are intentionally omitted; the public repo relies on the factual summaries, not the raw artifacts.

| Artifact | Description |
|----------|-------------|
| `X3.exe` | Stock Attack Shark X3 configuration binary (native analysis source) |
| `web-driver/REPORT.md` | WebDriver.exe WebSocket bridge architecture and PID audit |
| Firmware updater README / payloads | M600 firmware update tool analysis |
| `reset_packets_x3.json` | USBPcap capture of stock X3 factory reset, preserved as [`../evidence/x3-fa61/reset-packets.json`](../evidence/x3-fa61/reset-packets.json) |
| Tshark wrapper README | Packet capture tooling used for USB analysis |
| WebHID inspector (`webhid-inspect.html`) | Browser-based HID descriptor enumeration tool |

---

## Corrected and retracted claims

Corrections are maintained once in the canonical [`corrections.md`](corrections.md) ledger. This page preserves the provenance and analysis that support those replacements.

---

*These notes are a companion to the stable [protocol](../protocols/README.md), [transport](../transports/README.md), and [device](../README.md#choose-a-device) references.*
