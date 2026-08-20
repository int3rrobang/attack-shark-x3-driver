# Samples Directory

Raw captures, packet dumps, and analysis fragments supporting the protocol reverse-engineering.

| File | Description | Model / Variant |
|------|-------------|-----------------|
| `dpi-change.txt` | Hex dump of DPI report 0x04 packets for varying DPI values (50–22000) | X11 |
| `dpi.json` | Structured DPI packet samples exported from a local inspector tool (50–22000) | X11 |
| `dpi-stage-mask.md` | Analysis of high-stage flags and mask activation thresholds in DPI packets | X11 |
| `set-custom-macro.txt` | Hex dumps of custom macro report sequences (pages 0–2) using the X11 page-2 `09 0c` convention | X11 |
| `change-polling-rate.pcapng` | Raw USBPcap capture of a polling rate change | X11 |
| `reset-button.pcapng` | Raw USBPcap capture of a factory reset sequence | X11 |
| `reset-packets-x3.json` | USBPcap JSON export of a stock X3/FA61 factory reset (reports 0x0c, 0x04, 0x05, 0x06, 0x08) | X3 / FA61 |

### Notes

- **`dpi-change.txt` and `dpi.json`** cover the same X11 DPI packet space. `dpi.json` is structured for programmatic use; `dpi-change.txt` is a raw hex log.
- **`set-custom-macro.txt`** uses the X11 `09 0c` page-2 convention. X3/FA61 hardware uses `09 40` instead — see [`../x3-fa61-quirks.md`](../x3-fa61-quirks.md) for the difference.
- **`dpi-stage-mask.md`** documents the X11-specific mask and high-flag behavior; X3 DPI uses a different layout (see [`../dpi-protocol.md`](../dpi-protocol.md) for offsets).
- **`reset-packets-x3.json`** is a byte-for-byte copy of an external USBPcap capture. It is the only non-X11 sample in this directory.
- **`.pcapng` files** are raw Wireshark/USBPcap captures. Open with Wireshark and the USBPcap dissector.
