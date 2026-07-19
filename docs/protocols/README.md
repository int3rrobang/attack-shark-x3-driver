# Packet protocol reference

These documents are organized by report ID rather than by product name. Each report page identifies the supported model dialects, transports, evidence, and known implementation gaps.

| Report | Purpose | X11 USB | X3 USB | X3 BLE |
|:-------|:--------|:--------|:-------|:-------|
| [`0x04`](04-dpi.md) | DPI and sensor settings | Supported | Live/capture-confirmed | Live-confirmed |
| [`0x05`](05-preferences.md) | Preferences, sleep, debounce, lighting fields | Supported | Partially implemented | Live-confirmed with restrictions |
| [`0x06`](06-polling-rate.md) | Polling rate | Supported | Capture-confirmed | Rejected; do not send |
| [`0x07`](07-wakeup-mode.md) | Wakeup mode | Unknown | Format known; behavior untested | Untested |
| [`0x08`](08-button-mapping.md) | Button mapping | Supported | Partially implemented | Checksum live-confirmed |
| [`0x09`](09-custom-macros.md) | Custom macro pages | Supported | Live-confirmed | Parser acceptance only |
| [`0x0c`](0c-profile-reset.md) | Profile load/reset actions | Supported | Capture/live-confirmed | Parser acceptance; persistence incomplete |
| [Battery](battery.md) | Battery status | Adapter only | Wired unavailable | Standard BLE battery characteristic |

“Parser acceptance” means the firmware acknowledged a packet; it does not by itself prove application or persistence.
