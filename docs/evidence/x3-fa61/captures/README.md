# X3/FA61 guided USBPcap captures

These directories preserve the complete guided USBPcap sessions from 2026-07-24:

- [`2026-07-24-fa60-safe/`](2026-07-24-fa60-safe/) — X3 through the FA60 receiver
- [`2026-07-24-fa61-safe/`](2026-07-24-fa61-safe/) — X3 through wired FA61

Each directory contains one combined capture (`*-manual-all.pcapng`) and one
capture for each guided step. The combined capture is chronological; use the
individual files when isolating one operation.

## Provenance

The sessions were run by the historical `scripts/fa61-test-suite.ps1` harness,
which delegated capture to the sibling `tshark_mouse` Python package:

```text
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 `
  -Mode capture-manual -Transport receiver

pwsh -NoProfile -File scripts/fa61-test-suite.ps1 `
  -Mode capture-manual -Transport wired
```

The receiver run used `fa60-driver-safe`; the wired run used
`fa61-driver-safe`. The harness captured all USB traffic, retained the combined
session, and split each guided interval by its wall-clock boundaries.

## Guided order

Both presets use this order:

1. Device initialization and idle baseline
2. Movement and wheel input
3. Normal mouse-button input
4. Exactly six physical DPI-button presses
5. Profile 1 polling-rate change and restoration
6. Profile 1 DPI stage-1 change and restoration
7. Profile 1 lift-off-distance change and restoration
8. Profile 1 key-response-time change and restoration
9. Profile 1 ripple-control change and restoration
10. Profile 1 angle-snap change and restoration
11. Profile 1 motion-sync change and restoration
12. Profile 1 side-button remap and restoration
13. Optional Profile 1 sleep-timer change and restoration
14. Final idle interval

The FA60 session also includes a battery-observation step after initialization.
The wired FA61 path does not emit the receiver battery report, so it does not
have that step.

Every reversible setting change has a separate restore interval. The presets
exclude reset packets, macros, lighting, profile switching, maximum-profile
changes, scroll remaps, and multi-field changes.

## Inspecting a capture

The raw `.pcapng` files are self-contained and can be opened directly in
Wireshark. No private helper is required.

From the Rust repository root, standard `tshark` can produce a raw USB/HID
field dump:

```powershell
tshark `
  -r docs\evidence\x3-fa61\captures\2026-07-24-fa61-safe\fa61-manual-all.pcapng `
  -Y "usb.capdata || usbhid.data" `
  -T fields `
  -e frame.number `
  -e frame.time_relative `
  -e usb.src `
  -e usb.capdata `
  -e usbhid.data
```

If `tshark` is not on `PATH` on Windows, use the Wireshark installation path,
normally `C:\Program Files\Wireshark\tshark.exe`.

The optional `tshark_mouse` sibling helper provides report-aware JSON output:

```powershell
cd ..\..\..\..\tshark_mouse
python -m tshark_mouse read `
  ..\attack-shark-x3-rust\docs\evidence\x3-fa61\captures\2026-07-24-fa61-safe\fa61-manual-all.pcapng `
  --all-traffic

python -m tshark_mouse read `
  ..\attack-shark-x3-rust\docs\evidence\x3-fa61\captures\2026-07-24-fa61-safe\fa61-manual-all.pcapng `
  --all-traffic |
  ConvertFrom-Json |
  Where-Object { $_.report_id -eq '0x08' } |
  Select-Object timestamp, direction, raw_hex
```

The post-parse filter is intentional: these captures carry some report IDs in
interrupt payloads, where a Wireshark `wValue` display filter can omit them.

Useful report IDs for configuration diffs are:

- `0x04` — DPI
- `0x05` — preferences
- `0x06` — polling rate
- `0x08` — button mapping
- `0x09` — custom macro

Use `--all-traffic` when investigating physical input, notifications, or
vendor reports rather than configuration writes alone.

These files are raw evidence. Do not rewrite them to match an interpretation;
record interpretations in the relevant protocol or research document and keep
the transport and model distinction explicit.
