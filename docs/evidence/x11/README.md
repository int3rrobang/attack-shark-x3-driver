# X11 evidence

These artifacts describe the Attack Shark X11 wired (`0xfa55`) or 2.4 GHz adapter (`0xfa60`). They are evidence for the X11 dialect only.

| Path | Description | Variant |
|:-----|:------------|:--------|
| `descriptors/` | USB descriptor dumps | X11 wired and adapter |
| `dpi-change.txt` | Raw report `0x04` DPI hex log covering 50–22,000 DPI | X11 |
| `dpi.json` | Structured DPI packet samples | X11 |
| `dpi-stage-mask.md` | Analysis of X11 high-stage flags and masks | X11 |
| `set-custom-macro.txt` | Report `0x09` pages using the X11 `09 0c` page-2 convention | X11 |
| `change-polling-rate.pcapng` | USBPcap polling-rate capture | X11 |
| `reset-button.pcapng` | USBPcap reset capture | X11 |

`dpi-change.txt` and `dpi.json` cover the same packet space in raw and structured forms. Packet captures should be opened with Wireshark and the USBPcap dissector.
