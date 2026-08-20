# X3 / FA61 evidence

These artifacts describe devices using the X3/FA61 protocol target. They must not be
generalized to X11 without independent evidence.

## Captures

| Path | Description | Evidence |
|:-----|:------------|:---------|
| `reset-packets.json` | Byte-for-byte USBPcap JSON export of a stock X3/FA61 factory reset: reports `0x0c`, `0x04`, `0x05`, `0x06`, and `0x08` | capture-confirmed |

## Guided USBPcap sessions — 2026-07-24

The following byte-for-byte USBPcap sessions were preserved from the historical
`scripts/fa61-test-suite.ps1` runner and its sibling `tshark_mouse` capture
repository. The runner is not included in this repository and is not currently runnable;
the captures below are the authoritative evidence. Each directory contains the combined
capture plus the individual time-sliced capture for every guided step.

For a handoff-oriented session index and inspection commands, see
[`captures/README.md`](captures/README.md).

| Path | Transport | Description | Evidence |
|:-----|:----------|:------------|:---------|
| [`captures/2026-07-24-fa60-safe/`](captures/2026-07-24-fa60-safe/) | FA60 receiver | Safe guided preset: receiver initialization, idle and battery observation, motion and wheel input, normal buttons, six DPI-button presses, and reversible Profile 1 controls | capture-confirmed; live-confirmed |
| [`captures/2026-07-24-fa61-safe/`](captures/2026-07-24-fa61-safe/) | FA61 wired | Safe guided preset: wired initialization, idle and input baselines, normal buttons, six DPI-button presses, and reversible Profile 1 controls | capture-confirmed; live-confirmed |

The combined artifacts are `fa60-manual-all.pcapng` and
`fa61-manual-all.pcapng`. The guided presets intentionally excluded reset,
macros, lighting, profile switching, maximum-profile changes, scroll remaps,
and multi-field changes. The capture session was observational with respect to
the Rust driver; configuration changes were made through the stock application
and each reversible change was restored during the guided run.


## Button-action mapping session — 2026-08-14

A stock-app button-assignment sweep on X3/FA61 wired, capturing the wire
encoding of every action in the stock assignment menu (media, browser,
shortcut presets, fire, easy aim, scroll actions).

| Path | Transport | Description | Evidence |
|:-----|:----------|:------------|:---------|
| [`captures/2026-08-14-button-action-map-r2/`](captures/2026-08-14-button-action-map-r2/) | FA61 wired | First half: INIT through Multimedia > Volume + | capture-confirmed |
| [`captures/2026-08-14-button-action-map-r2-remainder/`](captures/2026-08-14-button-action-map-r2-remainder/) | FA61 wired | Second half: Volume - through Shortcut, RESTORE to Forward, FINAL IDLE | capture-confirmed |
| [`captures/2026-08-14-button-actions.json`](captures/2026-08-14-button-actions.json) | — | Parsed `0x08` writes: stock action, packet hex, slot-6 triplet, checksum | capture-confirmed |

Button 4 (Forward slot, index 6) was rebound to each menu action in the
stock app, one action per guided step; each write differs from the Forward
baseline only at slot 6. The interpreted mapping is in
[`../../protocols/08-button-mapping.md`](../../protocols/08-button-mapping.md),
and every captured packet is a golden fixture in
[`../../../fixtures/protocol/buttons.json`](../../../fixtures/protocol/buttons.json).
Wheel-slot remaps and macro bindings were excluded.

## Probing sessions 2026-07-17

All sessions used the wired FA61 Col04 path. They were driven by the historical (now-deleted) TypeScript scripts `scripts/fa61-profile-ab.ts` and `scripts/fa61-readback-benchmark.ts` and must not be generalized to BLE or to non-Col04 interfaces. The script references are provenance records — the raw JSON captures below are the authoritative evidence. Equivalent experiments can be reproduced using the Rust `x3ctl` CLI.

| Path | Mouse | Description | Evidence |
|:-----|:------|:------------|:---------|
| `2026-07-17-m600-profile-ab.json` | M600 | Initial A/B profile probe; result correctly identified the targeted-read contamination | live-confirmed |
| `2026-07-17-x3-profile-ab.json` | X3 | First X3 A/B probe after the targeted-read correction | live-confirmed |
| `2026-07-17-m600-profile-targeted-errored.json` | M600 | Correctly targeted A/B run that aborted on a transient metadata-subtype read; preserved to document the publication race | live-confirmed |
| `2026-07-17-m600-profile-targeted.json` | M600 | Successful M600 targeted A/B run; profiles 1 and 2 load distinct persisted DPI images | live-confirmed |
| `2026-07-17-x3-profile-targeted.json` | X3 | Successful X3 targeted A/B run with `--with-cycle`; one physical Forward press bound to `34 00 00` changed profile 1 to profile 2 | live-confirmed |
| `2026-07-17-x3-readback-benchmark-preliminary.json` | X3 | First readback-latency benchmark; observed `0b 08 02` version prefix anomalies attributed to leftover mailbox state from earlier stress | live-confirmed |
| `2026-07-17-x3-readback-benchmark.json` | X3 | Strict readback-latency benchmark; no cross-report contamination after warm-up | live-confirmed |
| `2026-07-17-m600-readback-benchmark.json` | M600 | Readback-latency benchmark; reproduced the metadata-subtype publication race | live-confirmed |
| `2026-07-17-x3-profile-load-timing.json` | X3 | Limited profile-1 ↔ profile-2 transition timing; first clean transitions only | live-confirmed |
| `2026-07-17-m600-profile-load-timing.json` | M600 | Limited profile-1 ↔ profile-2 transition timing; first clean transitions only | live-confirmed |

Interpretation of the reset sequence lives in
[`../../protocols/0c-profile-reset.md`](../../protocols/0c-profile-reset.md). The
targeted-read correction, mailbox race, and readback latency comparison live in
[`../../transports/usb-hid.md`](../../transports/usb-hid.md) and
[`../../devices/x3-fa61.md`](../../devices/x3-fa61.md).
