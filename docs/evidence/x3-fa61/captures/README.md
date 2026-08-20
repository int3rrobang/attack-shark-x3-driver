# X3/FA61 guided USBPcap captures

These directories preserve the complete guided USBPcap sessions from 2026-07-24:

- [`2026-07-24-fa60-safe/`](2026-07-24-fa60-safe/) — X3 through the FA60 receiver
- [`2026-07-24-fa61-safe/`](2026-07-24-fa61-safe/) — X3 through wired FA61

Each directory contains one combined capture (`*-manual-all.pcapng`) and one
capture for each guided step. The combined capture is chronological; use the
individual files when isolating one operation.

## Provenance

The sessions were run by the historical `scripts/fa61-test-suite.ps1` harness
(not included in this repository; not currently runnable),
which delegated capture to the sibling `tshark_mouse` Python package. The
historical invocations were:

```text
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 `
  -Mode capture-manual -Transport receiver

pwsh -NoProfile -File scripts/fa61-test-suite.ps1 `
  -Mode capture-manual -Transport wired
```

The receiver run used `fa60-driver-safe`; the wired run used
`fa61-driver-safe`. The harness captured all USB traffic, retained the combined
session, and split each guided interval by its wall-clock boundaries.

## Button-action mapping session — 2026-08-14

- [`2026-08-14-button-action-map-r2/`](2026-08-14-button-action-map-r2/) — first half: INIT through Multimedia > Volume +
- [`2026-08-14-button-action-map-r2-remainder/`](2026-08-14-button-action-map-r2-remainder/) — second half: Volume - through Shortcut, RESTORE, FINAL IDLE
- [`2026-08-14-button-actions.json`](2026-08-14-button-actions.json) — parsed `0x08` writes: stock-app action, full 59-byte packet, slot-6 triplet, checksum

Each session directory contains one combined capture and one capture per
guided step. The stock app rebinds button 4 (Forward slot, index 6) to every
action in its assignment menu, one action per step, on X3/FA61 wired; every
write is a complete 59-byte table that differs from the Forward baseline only
at slot 6 (verified, checksums valid). The plan split into two runs because
the first command was truncated mid-paste after Volume +; button 4 carried
Volume + across the boundary. Wheel-slot remaps and macro bindings were
excluded. The parsed mapping lives in the paired JSON and in
`../../protocols/08-button-mapping.md`.

## Deep-sleep boundary session — 2026-08-15

- [`2026-08-15-deep-sleep-boundaries/`](2026-08-15-deep-sleep-boundaries/) —
  clean single-write USBPcap intervals for stock-app selections at 15, 16, 17,
  32, 33, and 48 minutes on wired X3/FA61
- [`2026-08-15-deep-sleep-boundaries.json`](2026-08-15-deep-sleep-boundaries.json)
  — parsed report `0x05` packets and their configuration/deep-sleep fields

Only intervals containing one unambiguous stock-app write were retained.
Together they capture both sides of the 16-minute boundary and the exact 32-
and 48-minute boundaries. They confirm that exact multiples of 16 advance the
configuration high-nibble bucket and encode a zero minute-within-bucket nibble
in byte 5 (`0x08`). \[capture-confirmed]

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
