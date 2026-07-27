# DPI (report `0x04`)

Report `0x04` configures DPI stages and model-specific sensor fields. The driver implements it in `DpiBuilder`; X3 uses the same report bytes over USB HID and BLE FEE3.
 
## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Supported | implementation + X11 samples |
| X3/M600 via FA60 receiver | USB HID | 52-byte compact writes, 56-byte full readbacks, `0xa0` selector | capture-confirmed + live-confirmed |
| X3/FA61 wired | USB HID | 1–8 stages and sensor fields confirmed | live-confirmed + capture-confirmed |
| X3/M600 BLE | BLE FEE3 | Same X3 payload confirmed | live-confirmed |
| X11 Bluetooth | — | Not tested | — |

## USB HID Request Parameters

To apply DPI settings, a `SET_REPORT` request is sent via USB with the following parameters:

- **bmRequestType**: `0x21` (Host-to-Device, Class-specific, Interface)
- **bRequest**: `0x09` (SET_REPORT)
- **wValue**: `0x0304` (Report Type: Feature, Report ID: 0x04)
- **wIndex**: `2` (Interface index)

## Data Buffer Structure

The internal report is 56 bytes. Writes always send the first 52 bytes (compact framing) regardless of transport — live-confirmed for both FA61 wired and FA60 receiver via stock-software USB captures. The 56-byte full framing (with 4 trailing zero bytes) appears only in `hid_get_feature_report` readbacks. The firmware tolerates a 56-byte write without corrupting state (live-confirmed via probe), but the stock app never sends one. Field definitions below combine evidence from stock-software captures, live testing, and the driver codebase.

| Offset | Field          | Type        | Description                                                      | Source |
|:-------|:---------------|:------------|:-----------------------------------------------------------------|:-------|
| 0      | Header 1       | `uint8`     | Fixed value `0x04`                                               | static |
| 1      | Header 2       | `uint8`     | Canonical write/wired declaration `0x38`; FA60 prepared readback `0x3a` | live-confirmed + static-analysis |
| 2      | Profile / Header 3 | `uint8` | X11 fixed `0x01`; X3 one-based target profile (`0x01`–`0x05`) | static-analysis |
| 3      | Angle Snap / LOD | `uint8`   | X11 angle snap; X3 LOD (`0x00` = 1mm, `0x01` = 2mm)              | live-confirmed |
| 4      | Ripple Control | `uint8`     | `0x01` to enable, `0x00` to disable                              | live-confirmed |
| 5      | Stage Enable   | `uint8`     | X11 fixed `0x3F`; X3 enabled-stage mask for 1-8 stages           | live-confirmed |
| 6      | Stage Mask A / X3 Angle Snap | `uint8` | X11 high-DPI mask; X3 angle snap (`0x01` enabled)       | live-confirmed |
| 7      | Stage Mask B / X3 Motion Sync | `uint8` | X11 duplicate mask; X3 motion sync (`0x01` enabled)     | live-confirmed |
| 8      | Stage 1 DPI    | `uint8`     | X11 encoded value; X3 low byte of `dpi / 50 - 1`                 | live-confirmed |
| 9      | Stage 2 DPI    | `uint8`     | Encoded value for the 2nd DPI stage                              | live-confirmed |
| 10     | Stage 3 DPI    | `uint8`     | Encoded value for the 3rd DPI stage                              | live-confirmed |
| 11     | Stage 4 DPI    | `uint8`     | Encoded value for the 4th DPI stage                              | live-confirmed |
| 12     | Stage 5 DPI    | `uint8`     | Encoded value for the 5th DPI stage                              | live-confirmed |
| 13     | Stage 6 DPI    | `uint8`     | Encoded value for the 6th DPI stage                              | live-confirmed |
| 14-15  | Stage 7-8 DPI  | `uint8[2]`  | X11 fixed `0x00, 0x00`; X3 low bytes for stages 7 and 8          | static |
| 16     | High Flag 1 / X3 High 1 | `uint8` | X11 high flag; X3 high byte of `dpi / 50 - 1`             | live-confirmed |
| 17     | High Flag 2    | `uint8`     | `0x01` if Stage 2 DPI > 10000, else `0x00`                       | live-confirmed |
| 18     | High Flag 3    | `uint8`     | `0x01` if Stage 3 DPI > 10000, else `0x00`                       | live-confirmed |
| 19     | High Flag 4    | `uint8`     | `0x01` if Stage 4 DPI > 10000, else `0x00`                       | live-confirmed |
| 20     | High Flag 5    | `uint8`     | `0x01` if Stage 5 DPI > 10000, else `0x00`                       | live-confirmed |
| 21     | High Flag 6    | `uint8`     | `0x01` if Stage 6 DPI > 10000, else `0x00`                       | live-confirmed |
| 22-23  | High Flag 7-8 / X3 High 7-8 | `uint8[2]` | X11 fixed `0x00, 0x00`; X3 high bytes for stages 7 and 8 | static |
| 24     | Active Stage   | `uint8`     | Index of the currently active DPI stage (X11 1-6, X3 1-8)        | live-confirmed |
| 25-49  | Fixed Data     | `uint8[25]` | Captured from stock software; individual field meanings unknown. X11 last byte = `0x02`, X3 last byte = `0x01`. | static |
| 50     | Checksum High  | `uint8`     | High byte of the 16-bit checksum                                 | static |
| 51     | Checksum Low   | `uint8`     | Low byte of the 16-bit checksum                                  | static |
| 52-55  | Padding        | `uint8[4]`  | Readback-only trailing zeros (fixed `0x00`); not sent in writes  | live-confirmed |

The FA60 readback remains 56 HID report bytes; `0x3a` is the length including
the WebDriver's two-byte method/model envelope. Receiver writes retain the
canonical `0x38` declaration. A serialized hardware probe read this dialect,
changed the active stage from 800 to 850 DPI, verified it, restored 800 DPI,
and matched the complete final profile snapshot to the backup. \[live-confirmed]

### X3 target-profile behavior

For X3/M600, byte 2 selects the working profile for both writes and armed reads. Before
handling the DPI section, firmware stores `byte 2 - 1` as its working-profile alias and
loads that profile when it differs from persistent current metadata. An `0xa0` read
selector carries the same one-based value at selector byte 4. Consequently, reading with
profile `01` can move live buffers back to profile 1 even while report `0x0c` metadata
continues to identify profile 2. Always serialize profile changes and use the intended
target byte.

## DPI Value Encoding

DPI values are not stored as literal integers. X11 wired/adapter values are mapped to specific hexadecimal values defined in
`src/tables/dpi-map.ts`. X3 wired uses the captured stock encoding directly: `raw = dpi / 50 - 1`, offset 8-15 store
`raw & 0xff` (low byte), and offsets 16-23 store `raw >> 8` (high byte). \[live-confirmed via stock software capture]

- **Range**: X11 wired/adapter support up to 22,000 DPI. X3 wired supports 50 to 26,000 DPI.
- **Steps**: X3 wired requires integer multiples of 50. X11 wired/adapter use the existing map behavior.

### Fixed-tail distinction (offsets 25–49)

The 25-byte fixed-data region (offsets 25–49) differs between X11 and X3:
- **X11**: last byte (offset 49) = `0x02`
- **X3**: last byte (offset 49) = `0x01`

The remaining bytes in this region are captured from stock software; individual field meanings are unknown and should not be invented. \[static-analysis via stock capture]

## X3 Variable Stages

X3 wired mode accepts 1 to 8 DPI stages. Offset 5 stores the enabled-stage mask: `(1 << n) - 1` for fewer than 8 stages,
or `0xFF` for 8 stages. Offsets 8-15 always contain 8 low bytes and offsets 16-23 always contain 8 high bytes. Unused X3
stages are zeroed (`00`) and disabled by the offset 5 stage mask. \[live-confirmed via stock software capture]

## X3 Sensor Toggles

For `ConnectionMode.X3Wired`, captured stock software stores sensor toggles in RID `04`, not RID `07`. \[static-analysis via stock capture]

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
