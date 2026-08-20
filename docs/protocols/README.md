# Packet protocol reference

These documents are organized by report ID rather than by product name. Each report page identifies the supported model dialects, transports, evidence, and known implementation gaps.

| Report | Purpose | X11 USB | X3 USB | X3 BLE |
|:-------|:--------|:--------|:-------|:-------|
| [`0x04`](04-dpi.md) | DPI and sensor settings (1–8 configurable stages; physical DPI button is a separate auxiliary input `03 00 10 <stage> 00` with six positions observed in captures) | Supported | Live/capture-confirmed; Rust wired and FA60 receiver paths; USB readback via armed `0xa0` selector | Live-confirmed |
| [`0x05`](05-preferences.md) | Preferences, sleep, debounce, lighting fields | Supported | Rust wired and FA60 receiver paths; USB readback via armed `0xa0` selector | Live-confirmed with restrictions |
| [`0x06`](06-polling-rate.md) | Polling rate | Supported | Live-confirmed; Rust wired/FA60 receiver read/write paths; exact BLE packet cross-transport effect confirmed | Stock app skips; shorter BLE packet forms rejected |
| [`0x07`](07-wakeup-mode.md) | Wakeup mode | Unknown | Format known; behavior untested — not exposed by Rust driver/CLI/manager; no fixture coverage | Untested — not exposed; no fixture coverage |
| [`0x08`](08-button-mapping.md) | Button mapping | Supported | Rust wired and FA60 receiver paths; USB readback via armed `0xa0` selector | Checksum live-confirmed |
| [`0x09`](09-custom-macros.md) | Custom macro pages | Supported | Live-confirmed; Rust driver/manager/CLI not exposed — parser acceptance only; no fixture coverage | Parser acceptance only; not exposed |
| [`0x0b`](0b-version.md) | Version and profile state composite | — | Live-confirmed; wired and FA60 receiver paths; byte [4] mirrors `0x05` light mode | No useful response |
| [`0x0c`](0c-profile-reset.md) | Profile load/reset actions | Supported | Capture/live-confirmed; Rust wired and FA60 receiver paths | Parser acceptance; persistence incomplete |
| [Battery](battery.md) | Battery status | Legacy `03 55 40 01 <pct>` 0–100 | X3/M600 FA60 `03 10 40 01 <level>` level 1–10 ×10 = percentage; confirmed by X3.exe disassembly + capture 2026-07-24 | Standard BLE Battery Service `0x180f`/`0x2a19` |

Targeted reads for `0x04`/`0x05`/`0x08` carry the one-based profile in byte 2 (and selector byte 4 of the `0xa0` read) and load that target's working buffers, which can change live mouse behavior without necessarily changing persistent `0x0c` current metadata. Report `0x06` skips that loader: byte 2 is a save alias, the deferred writer serializes the complete *live* image into that slot, and a `0x06` read is a live-rate read (byte 2 is a wire-shape check, not a content proof).

“Parser acceptance” means the firmware acknowledged a packet; it does not by itself prove application or persistence. BLE ACK `10 50 00 <report>` is parser acceptance only, never a readback or persistence proof.

### Coverage and fixture gaps

`fixtures/protocol/` covers `dpi.json`, `preferences.json`, `buttons.json`, and `profile.json`. There are no fixtures for `0x06` polling rate, `0x07` wakeup mode, `0x09` macros, battery/input reports (`0x03` family), or 16-bit checksum negative cases beyond the live ACK tables. `0x07` and `0x09` remain unsupported in the Rust driver/manager/`x3ctl` surface (see their report pages).
