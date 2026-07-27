# Polling rate (report `0x06`)

Report `0x06` configures the mouse's global movement-reporting frequency. X11 USB modes and X3/FA61 wired use the same nine-byte packet. In the 2026-07-22 same-hardware probe, the exact X3 packet was accepted over BLE and changed the rate observed after USB reconnect; shorter legacy-shaped BLE packets were rejected.

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
| 2          | Sub-command  | `0x01`      | Internal sub-command identifier                         |
| 3          | Polling Rate | `0x01-0x08` | Encoded value for the frequency (see table below)       |
| 4          | Checksum     | `0xXX`      | Complement of Byte 3 (`0xFF - Byte[3]`)                 |
| 5-8        | Padding      | `0x00`      | Null padding bytes                                      |

The X3 FA60 receiver returns the same nine-byte functional image with byte 1
set to `0x0b` on a feature-report readback (`06 0b 01 rr ~rr 00 00 00 00`).
Writes retain the canonical `0x09` form. The receiver-specific decoder accepts
the readback variant without weakening wired validation. \[capture-confirmed +
implementation]

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

Stock FA61/X3 wired captures match this layout. Full 9-byte payloads for each rate:

| Rate (Hz) | Payload (hex)                       |
|-----------|-------------------------------------|
| 125 Hz    | `06090108f700000000`                |
| 250 Hz    | `06090104fb00000000`                |
| 500 Hz    | `06090102fd00000000`                |
| 1000 Hz   | `06090101fe00000000`                |

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

## Rust USB driver support

The native Rust driver treats polling rate as global USB state rather than
profile-targeted state. `MouseHandle::read_polling_rate()` arms the one-shot
`0xa0` mailbox with a fresh `0x06` selector, waits for readiness, fetches the
nine-byte report once, and validates the report ID, declared length, sub-command,
rate code, complement, and padding. Malformed observations are rearmed and
retried under the normal bounded read policy.

`MouseHandle::write_polling_rate(rate)` sends the validated nine-byte report,
waits for the configured write delay (500 ms by default), and verifies a fresh
readback. This proves immediate state only, not power-cycle persistence.
These operations are exposed by the Rust API over USB. The production BLE API
also accepts the exact nine-byte X3 packet and waits for the matching FEE4 ACK,
but it cannot provide configuration readback.

The CLI equivalents are:

```text
cargo run -p x3ctl -- rate get
cargo run -p x3ctl -- rate set 1000

# BLE (requires ble feature)
cargo run -p x3ctl --features ble -- --transport ble rate get
cargo run -p x3ctl --features ble -- --transport ble rate set 500
```

The BLE CLI command reports parser acceptance from the FEE4 ACK; it does not
verify persistence or effective polling behavior.

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

Both returned `10 50 01 06`. The exact packet therefore changes the global
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
