# X3 / FA61 evidence

These artifacts describe devices using the X3/FA61 protocol target. They must not be
generalized to X11 without independent evidence.

## Captures

| Path | Description | Evidence |
|:-----|:------------|:---------|
| `reset-packets.json` | Byte-for-byte USBPcap JSON export of a stock X3/FA61 factory reset: reports `0x0c`, `0x04`, `0x05`, `0x06`, and `0x08` | capture-confirmed |

## Probing sessions 2026-07-17

All sessions used the wired FA61 Col04 path. They were driven by
`scripts/fa61-profile-ab.ts` and `scripts/fa61-readback-benchmark.ts` and must not be
generalized to BLE or to non-Col04 interfaces.

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
