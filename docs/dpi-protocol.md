# DPI Communication Protocol (Report 0x0304)

This document describes the USB HID communication protocol used to configure DPI settings for the Attack Shark X11
mouse, as implemented in the `DpiBuilder` class.

## USB HID Request Parameters

To apply DPI settings, a `SET_REPORT` request is sent via USB with the following parameters:

- **bmRequestType**: `0x21` (Host-to-Device, Class-specific, Interface)
- **bRequest**: `0x09` (SET_REPORT)
- **wValue**: `0x0304` (Report Type: Feature, Report ID: 0x04)
- **wIndex**: `2` (Interface index)

## Data Buffer Structure

The payload consists of a 56-byte buffer. In wired mode, only the first 52 bytes are typically sent.

| Offset | Field          | Type        | Description                                                      |
|:-------|:---------------|:------------|:-----------------------------------------------------------------|
| 0      | Header 1       | `uint8`     | Fixed value `0x04`                                               |
| 1      | Header 2       | `uint8`     | Fixed value `0x38`                                               |
| 2      | Header 3       | `uint8`     | Fixed value `0x01`                                               |
| 3      | Angle Snap / LOD | `uint8`   | X11 angle snap; X3 LOD (`0x00` = 1mm, `0x01` = 2mm)              |
| 4      | Ripple Control | `uint8`     | `0x01` to enable, `0x00` to disable                              |
| 5      | Stage Enable   | `uint8`     | X11 fixed `0x3F`; X3 enabled-stage mask for 1-8 stages           |
| 6      | Stage Mask A / X3 Angle Snap | `uint8` | X11 high-DPI mask; X3 angle snap (`0x01` enabled)       |
| 7      | Stage Mask B / X3 Motion Sync | `uint8` | X11 duplicate mask; X3 motion sync (`0x01` enabled)     |
| 8      | Stage 1 DPI    | `uint8`     | X11 encoded value; X3 low byte of `dpi / 50 - 1`                 |
| 9      | Stage 2 DPI    | `uint8`     | Encoded value for the 2nd DPI stage                              |
| 10     | Stage 3 DPI    | `uint8`     | Encoded value for the 3rd DPI stage                              |
| 11     | Stage 4 DPI    | `uint8`     | Encoded value for the 4th DPI stage                              |
| 12     | Stage 5 DPI    | `uint8`     | Encoded value for the 5th DPI stage                              |
| 13     | Stage 6 DPI    | `uint8`     | Encoded value for the 6th DPI stage                              |
| 14-15  | Stage 7-8 DPI  | `uint8[2]`  | X11 fixed `0x00, 0x00`; X3 low bytes for stages 7 and 8          |
| 16     | High Flag 1 / X3 High 1 | `uint8` | X11 high flag; X3 high byte of `dpi / 50 - 1`             |
| 17     | High Flag 2    | `uint8`     | `0x01` if Stage 2 DPI > 10000, else `0x00`                       |
| 18     | High Flag 3    | `uint8`     | `0x01` if Stage 3 DPI > 10000, else `0x00`                       |
| 19     | High Flag 4    | `uint8`     | `0x01` if Stage 4 DPI > 10000, else `0x00`                       |
| 20     | High Flag 5    | `uint8`     | `0x01` if Stage 5 DPI > 10000, else `0x00`                       |
| 21     | High Flag 6    | `uint8`     | `0x01` if Stage 6 DPI > 10000, else `0x00`                       |
| 22-23  | High Flag 7-8 / X3 High 7-8 | `uint8[2]` | X11 fixed `0x00, 0x00`; X3 high bytes for stages 7 and 8 |
| 24     | Active Stage   | `uint8`     | Index of the currently active DPI stage (X11 1-6, X3 1-8)        |
| 25-49  | Fixed Data     | `uint8[25]` | Internal fixed values and reserved space                         |
| 50     | Checksum High  | `uint8`     | High byte of the 16-bit checksum                                 |
| 51     | Checksum Low   | `uint8`     | Low byte of the 16-bit checksum                                  |
| 52-55  | Padding        | `uint8[4]`  | Wireless mode padding (fixed `0x00`)                             |

## DPI Value Encoding

DPI values are not stored as literal integers. X11 wired/adapter values are mapped to specific hexadecimal values defined in
`src/tables/dpi-map.ts`. X3 wired uses the captured stock encoding directly: `raw = dpi / 50 - 1`, offset 8-15 store
`raw & 0xff`, and offsets 16-23 store `raw >> 8`.

- **Range**: X11 wired/adapter support up to 22,000 DPI. X3 wired supports 50 to 26,000 DPI.
- **Steps**: X3 wired requires integer multiples of 50. X11 wired/adapter use the existing map behavior.

## X3 Variable Stages

X3 wired mode accepts 1 to 8 DPI stages. Offset 5 stores the enabled-stage mask: `(1 << n) - 1` for fewer than 8 stages,
or `0xFF` for 8 stages. Offsets 8-15 always contain 8 low bytes and offsets 16-23 always contain 8 high bytes. Unused X3
stages are zeroed (`00`) and disabled by the offset 5 stage mask.

## X3 Sensor Toggles

For `ConnectionMode.X3Wired`, captured stock software stores sensor toggles in RID `04`, not RID `07`:

| Offset | Field       | Values                    |
|:-------|:------------|:--------------------------|
| 3      | LOD         | `0x00` = 1mm, `0x01` = 2mm |
| 4      | Ripple      | `0x00` off, `0x01` on      |
| 6      | Angle snap  | `0x00` off, `0x01` on      |
| 7      | Motion sync | `0x00` off, `0x01` on      |

## Checksum Calculation

The checksum is a simple 16-bit sum of the bytes in the buffer from index 3 to 49.

```typescript
function calculateChecksum(buffer: Buffer): number {
    let sum = 0;
    for (let i = 3; i <= 49; i++) {
        sum += buffer[i];
    }
    return sum & 0xFFFF;
}
```

The result is then stored in big-endian format:

- `buffer[50] = (checksum >> 8) & 0xFF;`
- `buffer[51] = checksum & 0xFF;`
