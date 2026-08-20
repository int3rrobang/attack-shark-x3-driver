# Polling rate (report `0x06`)

Report `0x06` configures the movement-reporting frequency of the selected
profile. Byte 2 is the one-based target profile consumed by the shared
dispatcher prelude; polling rate is **per-profile and persistent**, not a
device-global tag. X11 USB modes and X3/FA61 wired use the same nine-byte
packet. In the 2026-07-22 same-hardware probe, the exact X3 packet was accepted
over BLE and changed the rate observed after USB reconnect; shorter legacy-shaped
BLE packets were rejected. \[corrected]

> **Warning — `0x06` is a save alias, not a safe standalone write.**
> Report `0x06` **skips the profile loader**: byte 2 names the slot the
> deferred writer serializes the complete *live* profile image into, so a
> `0x06` write can persist the current live DPI, preferences, and buttons
> under the target alias. A `0x06` ACK or rate readback never proves the
> non-rate save image or persistence. Packet encoding (this page) is
> transport-independent and safe to describe; safe-by-default behavior is a
> manager policy on top (see
> [Packet encoding vs. safe manager policy](#packet-encoding-vs-safe-manager-policy)
> and [`docs/safety.md`](../safety.md)). Driver primitives are named
> `*_unchecked`, and the BLE override requires explicit per-operation
> authorization (`--allow-unverified-ble-rate-write`).

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Supported | implementation + capture |
| X3/M600 via FA60 receiver | USB HID | 9-byte packet and `0xa0` readback | binary report + implementation |
| X3/FA61 wired | USB HID | Live-confirmed; Rust read/write path and persistence verified on one device | capture + live-confirmed |
| X3/M600 BLE | BLE FEE3 | Exact nine-byte X3 packet accepted in the same-hardware probe; stock app skips the report | live-confirmed + corrected |

## Technical Specifications

The protocol uses a standard HID Feature Report (`SET_REPORT`).

| Parameter          | Value    | Description                             |
|--------------------|----------|-----------------------------------------|
| **Request Type**   | `0x21`   | Host-to-Device, Class, Interface        |
| **Request**        | `0x09`   | SET_REPORT                              |
| **Value (wValue)** | `0x0306` | Feature Report (0x03), Report ID (0x06) |
| **Index (wIndex)** | `0x0002` | Interface Index 2                       |

## Data Packet Structure

The payload consists of a 9-byte buffer. The structure is identical for both **Wired** and **Wireless** connection modes.

### Byte-by-Byte Analysis

| Byte Index | Field        | Value (Hex) | Description                                             |
|------------|--------------|-------------|---------------------------------------------------------|
| 0          | Report ID    | `0x06`      | Must match the low byte of `wValue`                     |
| 1          | Command      | `0x09`      | Internal command identifier                             |
| 2          | Target Profile | `0x01-0x05` | One-based target profile; the slot the deferred writer serializes into |
| 3          | Polling Rate | `0x01-0x08` | Encoded value for the frequency (see table below)       |
| 4          | Checksum     | `0xXX`      | Complement of Byte 3 (`0xFF - Byte[3]`)                 |
| 5-8        | Padding      | `0x00`      | Null padding bytes                                      |

The X3 FA60 receiver returns the same nine-byte functional image with byte 1
set to `0x0b` on a feature-report readback (`06 0b <profile> rr ~rr 00 00 00 00`).
Writes retain the canonical `0x09` form. The receiver-specific decoder accepts
the readback variant without weakening wired validation. \[capture-confirmed +
implementation]

Byte 2 is validated as a one-based `ProfileId` (`1..=5`) on both encode and
decode. Firmware static analysis of the M600-family image shows the shared
dispatcher consumes byte 2 as the target profile for reports `0x04`, `0x05`,
`0x06`, and `0x08`; report `0x06` **skips the profile loader** and its deferred
writer serializes the complete *live* profile image into the slot named by
byte 2. \[static-analysis, 2026-08-10 report section 25]

## Polling Rate Encoding

The mouse supports four polling rate levels. The value sent at index 3 determines the frequency:

| Rate (Hz) | Hex Value | Profile Name |
|-----------|-----------|--------------|
| 125 Hz    | `0x08`    | Power Saving |
| 250 Hz    | `0x04`    | Office       |
| 500 Hz    | `0x02`    | Gaming       |
| 1000 Hz   | `0x01`    | eSports      |

### Checksum Calculation
The checksum at index 4 is calculated using the formula: `0xFF - buffer[3]`.

## Complete Packet Payloads

Stock FA61/X3 wired captures match this layout; byte 2 is the one-based target
profile (captures and the factory-recovery sequence used profile 1). Full
9-byte payloads for each rate at profile 1:

| Rate (Hz) | Payload (hex)                       |
|-----------|-------------------------------------|
| 125 Hz    | `06090108f700000000`                |
| 250 Hz    | `06090104fb00000000`                |
| 500 Hz    | `06090102fd00000000`                |
| 1000 Hz   | `06090101fe00000000`                |

The same rates at profile 2 replace byte 2 with `02`:

| Rate (Hz) | Payload (hex)                       |
|-----------|-------------------------------------|
| 125 Hz    | `06090208f700000000`                |
| 250 Hz    | `06090204fb00000000`                |
| 500 Hz    | `06090202fd00000000`                |
| 1000 Hz   | `06090201fe00000000`                |

Byte 3 is the encoded rate, byte 4 is `0xFF - Byte[3]`, bytes 5-8 are zero-pad.

## Wireshark Analysis Example

To observe this protocol in action using Wireshark with USBPcap:

1.  **Filter**: Apply the filter `usb.setup.wValue == 0x0306` to see only Polling Rate requests.
2.  **Request**: Look for `SET_REPORT` Request (Control Out).
3.  **Data**: The "HID Data" field will contain the 9-byte payload.

**Example Capture (1000 Hz / eSports):**
```text
Setup Data
    bmRequestType: 0x21
    bRequest: 9 (SET_REPORT)
    wValue: 0x0306 (Report Type: Feature, Report ID: 6)
    wIndex: 2 (Interface 2)
    wLength: 9
Data (Hex):
    06 09 01 01 fe 00 00 00 00
```

Polling rate is **profile-scoped**, not global. `MouseHandle::read_polling_rate(profile)`
arms the one-shot `0xa0` mailbox with a fresh `0x06` selector carrying the
requested profile, waits for readiness, fetches the nine-byte report once, and
validates the report ID, declared length, profile byte, rate code, complement,
and padding. Malformed observations are rearmed and retried under the normal
bounded read policy.

**Targeted-read vs. live-rate distinction:** `0x04`/`0x05`/`0x08` reads carry the one-based
target in byte 2 and selector byte 4 and **load that target's working buffers**,
which can change live mouse behavior without necessarily updating persistent `0x0c`
current metadata. Report `0x06` **skips that loader**: byte 2 is a **save alias**
(the deferred writer serializes the complete live DPI/preferences/buttons image into that
slot), so a `0x06` read is a **live-rate read** and a `0x06` write is a save-alias write.

**Live-read limitation:** report `0x06` skips the profile loader, so the
readback always reflects the profile that is *currently live* on the device.
The readback's byte 2 mirrors the armed working alias/selector, not the loaded
image; validating it is a wire-shape check, not a live-content proof. Reading a
non-live profile therefore returns the live profile's rate, and the read's alias
is mutated as a side effect. The manager reads
the rate of the persistent current profile (see `status`), and callers must not
treat a `0x06` read as a profile load or as persistence evidence.
**Driver primitives are unchecked.** The low-level methods carry no safety
precondition — they emit the packet directly and are named to say so:

- `MouseHandle::send_polling_rate_unchecked(profile, rate)` submits exactly
  one nine-byte feature report and completes on the transport-level
  acknowledgment (USB `SET_REPORT` transfer success or BLE parser ACK
  `10 50 00 06`). Neither is a readback or persistence proof.
- `MouseHandle::write_polling_rate_unchecked(profile, rate)` sends the report,
  holds the configured quiet period (500 ms wired or five seconds through the
  FA60 receiver), re-arms a fresh `0xa0` read, and requires the returned rate
  to match. It proves the immediate rate field only, not the non-rate save
  image and not power-cycle persistence. On the FA60 receiver this is the
  costly path: the prepared-read transaction temporarily suppresses pointer
  traffic, while a send-only transport write does not (see the freeze-probe
  timing in
  [`usb-hid.md`](../transports/usb-hid.md#profile-switch-transport-freeze-probe)).
  Readback is unsupported over BLE, which has no byte-for-byte readback path.
- `BleHandle::write_polling_rate_unchecked(profile, rate)` sends the report
  over BLE and returns the parser ACK; see the unchecked-BLE section below.

Because byte 2 names the target profile while report `0x06` skips the profile
loader, the deferred writer serializes the complete *live* image into the slot
named by byte 2: the packet must carry the intended profile, and a `0x06`
readback reflects the profile that is currently live, not a loaded image (see
the live-read limitation above). ACK or rate readback of `0x06` never proves
the non-rate save image or persistence; explicit stronger verification remains
`x3ctl verify --method profile-reload|power-cycle`, which is unchanged.

## Packet encoding vs. safe manager policy

This page documents the packet encoding (layout, checksum, rate table,
transport variants) and the driver primitives. Encoding knowledge is
transport-independent and safe to describe; it does not make a write safe.
Safe-by-default behavior is a **manager policy** layered on top, in
[`docs/safety.md`](../safety.md#polling-rate-writes-report-0x06):

- **Safe USB write** (`DeviceManager::update_polling_rate`): requires complete
  desired DPI/preferences/buttons for the target, requires persistent metadata
  `current == target`, freshly reads the complete profile in the same session,
  and compares every non-rate section — any mismatch aborts **before any
  hardware write**. The current rate is then read; an already-equal rate is a
  **no-op with no hardware write**. Otherwise the rate is written last with
  the requested post-write validation (`Transport` or `Readback`).
- **Safe BLE write** of the same method fails with
  `ManagerError::ExplicitAuthorizationRequired` before any session call: BLE
  cannot freshly read the complete profile, so the preflight is impossible.
- **Unchecked BLE override** (`DeviceManager::update_polling_rate_unverified_ble`,
  CLI `rate set --allow-unverified-ble-rate-write`): BLE-only, transport
  validation only, direct packet write, **ACK-only evidence** (`10 50 00 06`),
  persistence recorded as `Unknown`. This is an explicit escape hatch for
  informed users; authorization changes what may be sent, never what may be
  claimed as verified.

**Flash wear:** every complement-valid `0x06` write schedules a deferred
complete-profile save that erases the shared `0x0007C000`-sector; an unchanged
rate write is not idempotent with respect to flash wear, so avoid redundant
writes. The manager's safe path already skips writes when the current rate
equals the desired rate (a no-op), which avoids this wear; unchecked paths
still emit every requested packet. The ACK (`10 50 00 06`) means "validated
and queued", not "durably committed", and is not a batch/flush barrier.
Configuration remains recoverable from a recorded backup or known-good packet;
readback and verify are optional evidence-gathering, not a mandatory safety
gate. \[static-analysis, 2026-08-10 report section 25]

The CLI equivalents operate on the profile selected by the global `--profile`
flag:

```text
cargo run -p x3ctl -- --profile 1 rate get
cargo run -p x3ctl -- --profile 2 rate set 1000
cargo run -p x3ctl -- --profile 2 rate get
```

`rate set` routes through the manager's safe `update_polling_rate` preflight
(complete desired non-rate image for the target, persistent metadata
`current == target`, fresh same-session complete-profile read with every
non-rate section equal, no-op when the current rate already matches). Over USB
the default `--validation transport` accepts the transport-level
acknowledgment (`SET_REPORT` submission success); `--validation readback`
adds the armed `0xa0` rate readback, which proves the immediate rate field
only. Over BLE the safe path fails with `ExplicitAuthorizationRequired`.

The BLE override is an explicit per-invocation choice on `rate set`:

```text
cargo run -p x3ctl -- --transport ble --profile 2 rate set 1000 --allow-unverified-ble-rate-write
```

It calls `update_polling_rate_unverified_ble`: BLE-only, `--validation
readback` rejected, direct packet write, ACK-only evidence (`10 50 00 06`),
persistence `Unknown`. BLE `rate get` remains unsupported because BLE has no
read path for report `0x06`.

## Hardware qualification

**Live-confirmed on one X3/FA61 wired mouse, 2026-07-20:** the Rust CLI read
returned 1000 Hz (`rr = 0x01`), `rate set 500` sent
`06090102fd00000000` and passed immediate readback, and a physical power-cycle
returned 500 Hz. The original 1000 Hz setting was then restored and read back
successfully with `06090101fe00000000`.

This confirms immediate application and power-cycle persistence for report
`0x06` on the tested wired device. It does not generalize to X11 hardware,
FA60 receiver firmware, or other BLE firmware.

### Same-hardware BLE qualification (2026-07-22)

The same physical X3/M600 mouse was at `1000 Hz` when connected through FA61
USB. The exact nine-byte X3 packet for 500 Hz was then sent over BLE:

```text
06 09 01 02 fd 00 00 00 00
```

BLE returned `10 50 00 06`. After reconnecting the same mouse through USB,
`read rate` returned `500 Hz` (`code: 0x02`). The rate was restored to
`1000 Hz` over USB.

Two shorter legacy-shaped packets were also rejected over BLE:

```text
06 01 00 00 00 00 00 00 fe
06 02 00 00 00 00 00 00 fd
```

Both returned `10 50 01 06`. The exact packet therefore changes the
working polling-rate state across BLE-to-USB transport switching, while the
shorter form does not. The stock application still skips BLE report `0x06`;
the reason for that policy is unresolved. \[corrected, live-confirmed]


Follow-up value sweep on the same mouse accepted all three additional exact
packets and produced matching USB readback:

| BLE packet | BLE ACK | USB readback |
|:-----------|:--------|:-------------|
| `06090108f700000000` | `10 50 00 06` | `125 Hz` (`0x08`) |
| `06090104fb00000000` | `10 50 00 06` | `250 Hz` (`0x04`) |
| `06090101fe00000000` | `10 50 00 06` | `1000 Hz` (`0x01`) |

The mouse was left at `1000 Hz`. \[live-confirmed]

### BLE rate-ceiling measurement (2026-07-22)

The same X3 Bluetooth HID mouse was measured through Windows Raw Input while
moving continuously. Each capture lasted 10.001 seconds and was filtered to
the X3 Bluetooth HID mouse collection:

| Configured rate | Raw reports | Effective OS-delivered rate |
|:----------------|------------:|----------------------------:|
| `125 Hz`        | 971         | `121.29 Hz`                  |
| `500 Hz`        | 1128        | `112.81 Hz`                  |
| `1000 Hz`       | 1299        | `129.93 Hz`                  |

The 125 Hz state came from the preceding BLE configuration sequence; 500 Hz
and 1000 Hz were set over USB before unplugging and reconnecting the existing
Windows Bluetooth HID pairing. The observed stream therefore stayed near a
125–130 Hz ceiling even while the configured state changed to 500 and 1000 Hz.

This supports the hypothesis that the BLE HID path cannot deliver meaningful
mouse input above roughly 125 Hz, which would explain the stock UI greying out
higher values rather than presenting a misleading control. It is not a proof
of the link-layer connection interval: these are OS-delivered reports and can
also reflect host scheduling, report coalescing, dropped reports, or movement
generation. \[live-confirmed measurement + inference]
## Technical Summary

-   **Report ID**: 0x06
-   **Interface**: 2
-   **Payload Length**: 9 bytes
-   **Checksum**: `0xFF - RateValue`
-   **Compatibility**: Wired, Wireless (Dongle), and X3 Wired / FA61
