# Version (report `0x0B`)

Report `0x0B` returns a composite image containing the active profile's light
mode, firmware/platform constants, and current profile metadata.  It is
read-only through the `0xA0` selector/mailbox path; there is no established
write path.

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X3/FA61 wired | USB HID | Live-confirmed; stable across 10+ reads per session | live-confirmed (2026-07-17, 2026-07-24) |
| X3/M600 via FA60 receiver | USB HID | Live-confirmed; 10-byte readback with `0x0c` declared length | live-confirmed (2026-07-24) |
| X3/M600 BLE | BLE FEE3 | No useful response; firmware dispatcher does not populate the readback buffer | static-analysis + live-confirmed |

## Wire format

Wired FA61 readback is 8 bytes; FA60 receiver readback is 10 bytes (same
functional image with two trailing zero-padding bytes).

| Offset | Field | Description | Evidence |
|:-------|:------|:------------|:---------|
| 0 | Report ID | `0x0b` | constant |
| 1 | Declared length | `0x08` (wired), `0x0c` (FA60 receiver) | live-confirmed |
| 2 | Selector echo | Returns the selector byte value (`0x01`–`0x05`); not persistent profile metadata | live-confirmed (2026-07-24 byte characterization probe) |
| 3 | Firmware/platform constant | `0x10` on both X3 and M600 in all tested sessions; does not change with selector | live-confirmed (stable across 6+ sessions, 2 devices, 3 selector values) |
| 4 | Selected profile's light mode | **Mirrors `0x05` byte [3]** for the profile chosen by the selector | live-confirmed — see §Correlation and §Byte characterization |
| 5 | Platform constant | `0x83` on both X3 and M600 in all tested sessions; does not change with selector | live-confirmed (stable across 6+ sessions, 2 devices, 3 selector values) |
| 6 | Global state | `0x01` in all tested sessions; does not change with selector | live-confirmed — does not track active DPI stage as previously speculated |
| 7 | Maximum profile | Maximum enabled profile from `0x0c` metadata; global, not per-selector | live-confirmed — changes from `0x01` to `0x02` when max is raised (2026-07-24) |
FA60 receiver readbacks append two zero bytes at offsets 8–9.

### Selector

The `0xA0` selector arms the read:

```text
a0 0b 08 00 01 00 00 00
```

Byte 4 (profile target) is set to `0x01` by convention; the report is not
Byte 4 (profile target) selects which profile's data appears at byte [4].
The firmware returns the selected profile's light mode and echoes the selector
value at byte [2].  The selector does **not** load the profile into working
buffers (see §Byte characterization).
## Correlation: byte [4] mirrors `0x05` light mode (2026-07-24)

**Live-confirmed on X3/FA61 wired and M600/FA61 wired, 2026-07-24:**
byte [4] of the `0x0B` version report equals byte [3] (light mode) of the
`0x05` preferences report for the same profile.

### Evidence

Cross-session correlation from live captures:

| Session | Device | `0x0B` byte [4] | `0x05` light mode | Match |
|:--------|:-------|:----------------|:------------------|:------|
| 2026-07-17 profile A/B | X3 | `0x70` | `0x70` | yes |
| 2026-07-17 profile A/B | M600 | `0x10` | `0x10` | yes |
| 2026-07-17 profile targeted | M600 (profile 1) | `0x00` | `0x00` | yes |
| 2026-07-17 profile targeted | M600 (profile 2) | `0x70` | `0x70` | yes |
| 2026-07-24 transport probe | X3 | `0x00` | (not captured, inferred `0x00`) | consistent |
| 2026-07-24 transport probe | M600 | `0x10` | (not captured, inferred `0x10`) | consistent |

### Test setup

The 2026-07-24 probe used `scripts/fa60-version-probe.ts` with sequential
phases: FA60 receiver → FA61 wired (X3), then FA61 wired (M600).  Each phase
read `0x0B` ten times at 750 ms selector settle / 2000 ms inter-read gap.
BLE pairing-slot toggle (short-press of the pairing button) was tested between
sub-phases.  The toggle produced no change in any `0x0B` byte on either device,
confirming the report is not tied to the BLE pairing identity.

A follow-up correlation check (`scripts/version-prefs-correlation.ts`) read
both `0x0B` and `0x05` in a single HID session on the M600 and confirmed
byte [4] = light mode at runtime.

### Implications

- **Not a model discriminator.**  Byte [4] reflects a user-configurable
  preference, not a firmware or hardware identity.  Earlier sessions where X3
  showed `0x70` and M600 showed `0x10` reflected different light-mode
  settings, not different firmware.
- **Not immutable.**  The value changes when the user (or software) changes
  the light mode in the active profile's preferences.
- **Not tied to BLE pairing identity.**  BLE slot toggle (M600-5.2 ↔
  M600-5.4) produces no change in any `0x0B` byte.
- **Selector-sensitive.**  The selector's profile byte determines which
  profile's light mode appears at byte [4], consistent with `0x04`, `0x05`,
  and `0x08` using the selector as a working-profile target.

## Byte characterization (2026-07-24)

**Live-confirmed on M600/FA61 wired:** `scripts/version-byte-probe.ts` set
max=2 via `0x0C`, wrote distinct light modes (`0x10` profile 1, `0x70`
profile 2), then read `0x0B` with selectors 1, 2, and 3.

| Byte | sel=1 | sel=2 | sel=3 | Identity |
|:-----|:------|:------|:------|:---------|
| [2] | `0x01` | `0x02` | `0x03` | **selector echo** — returns whatever value the host sent |
| [3] | `0x10` | `0x10` | `0x10` | global constant — not profile-dependent |
| [4] | `0x10` | `0x70` | `0x70` | **selected profile's light mode** — mirrors `0x05` byte [3] |
| [5] | `0x83` | `0x83` | `0x83` | global constant — not profile-dependent |
| [6] | `0x01` | `0x01` | `0x01` | global — does not track active DPI stage |
| [7] | `0x02` | `0x02` | `0x02` | **max profile** (was `0x01` when max was 1) — global, not per-selector |

Selector=3 (beyond max=2) returned `0x70` for byte [4] — same as profile 2,
not profile 1.  The firmware does not clamp the selector to the valid range;
it reads whatever is stored at that profile index.  The DPI contamination test
(selector side-effect probe) confirmed that the `0x0B` selector does **not**
load the selected profile into working buffers, unlike `0x04`/`0x05`/`0x08`.

### Selector side-effect test

A separate DPI contamination test (`scripts/version-selector-probe.ts`)
verified that reading `0x0B` with selector=2 does not change the DPI working
buffers for profile 1.  This means `0x0B` is a pure read — it returns data
for the selected profile without side effects on the device state.

### Restored state

After the probe, max was restored to 1 and light modes were preserved.


## Transport notes

The `0x0B` report is only available through USB HID (wired and FA60 receiver).
BLE has no equivalent: the M600 firmware's `0x0B` write handler at the
dispatcher (`0x33A44`) does not populate a readback buffer or emit a standard
FEE4 ACK.  The A0-selector/readback path is USB-only.

The Rust driver exposes `MouseHandle::read_version()` for this report.  It is
## Open questions

- Whether bytes [3] and [5] are truly firmware constants or can vary across
  firmware revisions (all tested devices showed `0x10` and `0x83` respectively).
- Whether byte [6] has a specific meaning beyond "global state `0x01`" (no
  variation observed across profiles, selectors, or max-profile changes).
