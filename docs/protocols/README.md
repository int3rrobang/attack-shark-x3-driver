# Packet protocol reference

These documents are organized by report ID rather than by product name. Each report page identifies the supported model dialects, transports, evidence, and known implementation gaps.

| Report | Purpose | X11 USB | X3 USB | X3 BLE |
|:-------|:--------|:--------|:-------|:-------|
| [`0x04`](04-dpi.md) | DPI and sensor settings | Supported | Live/capture-confirmed; Rust wired and FA60 receiver paths | Live-confirmed |
| [`0x05`](05-preferences.md) | Preferences, sleep, debounce, lighting fields | Supported | Rust wired and FA60 receiver paths; readback-verified | Live-confirmed with restrictions |
| [`0x06`](06-polling-rate.md) | Polling rate | Supported | Live-confirmed; Rust wired/FA60 receiver read/write paths; exact BLE packet cross-transport effect confirmed | Stock app skips; shorter BLE packet forms rejected |
| [`0x07`](07-wakeup-mode.md) | Wakeup mode | Unknown | Format known; behavior untested | Untested |
| [`0x08`](08-button-mapping.md) | Button mapping | Supported | Rust wired and FA60 receiver paths; readback-verified | Checksum live-confirmed |
| [`0x09`](09-custom-macros.md) | Custom macro pages | Supported | Live-confirmed; Rust API not exposed | Parser acceptance only |
| [`0x0b`](0b-version.md) | Version and profile state composite | — | Live-confirmed; wired and FA60 receiver paths; byte [4] mirrors `0x05` light mode | No useful response |
| [`0x0c`](0c-profile-reset.md) | Profile load/reset actions | Supported | Capture/live-confirmed; Rust wired and FA60 receiver paths | Parser acceptance; persistence incomplete |
| [Battery](battery.md) | Battery status | Legacy FA60 input signature | Candidate FA60 input signature; raw X3 emission unconfirmed | Standard BLE Battery Service |

“Parser acceptance” means the firmware acknowledged a packet; it does not by itself prove application or persistence.
