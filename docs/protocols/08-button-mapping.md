# Button mapping (report `0x08`)

Report `0x08` writes the complete button-assignment table. The driver implements it in `MacrosBuilder`; despite that historical class name, custom macro event pages use the separate [`0x09`](09-custom-macros.md) report.

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Supported | implementation and existing samples |
| X3/FA61 wired | USB HID | Normal buttons live-confirmed; checksum implementation gap | live-confirmed + capture-confirmed |
| X3/M600 BLE | BLE FEE3 | 16-bit checksum acceptance confirmed | live-confirmed |

## HID framing and payload

| Parameter | Value |
|:----------|:------|
| `wValue` | `0x0308` |
| `wIndex` | `0x0002` |
| Payload length | 59 bytes |

Bytes 0–1 are `08 3b`. On X11, byte 2 is the fixed header value `01`; on X3/M600 it is the one-based target profile. Bytes 3–56 contain eighteen three-byte assignment slots. Each populated slot is encoded as:

```text
<firmware action> <modifier> <key code or action value>
```

A write replaces the full table. The driver starts from model defaults and applies requested overrides; unspecified buttons do not preserve the current live device state.

An armed X3/M600 read also carries its one-based target at selector byte 4. Reading a
different target can replace the working button map without changing persistent
report-`0x0c` metadata, so profile changes and button read/write traffic must be
serialized.

## Logical button slots

The implemented X11 offsets for common controls are:

| Button | Offset |
|:-------|:-------|
| Left | 3 |
| Right | 6 |
| Middle | 9 |
| DPI | 18 |
| Forward | 21 |
| Backward | 24 |
| Scroll up | 51 |
| Scroll down | 54 |

The captured X3 default packet reverses the logical scroll assignments: scroll down is at offset 51 and scroll up at offset 54. The driver swaps these offsets in `x3-wired` mode. Direct scroll remaps remain unsafe because actions may repeat until unplug or reboot. DPI-button remaps appear ignored by stock FA61 firmware.

## Checksums

### X11 dialect

The production X11 builder sums bytes 2–57, subtracts one, masks to 8 bits, and writes the result at byte 58.

### X3 dialect

X3 uses a 16-bit big-endian sum over the eighteen assignment slots. The
target-profile byte is excluded:

```text
checksum = sum(bytes[3..56]) & 0xffff
byte[57] = checksum >> 8
byte[58] = checksum & 0xff
```

The live profile-2 packet ends in `00 bb`, exactly matching the slot-only sum.
The earlier `sum(bytes[2..56]) - 1` interpretation happened to match profile 1
because its target byte is `01`, but it produces `00 bc` for profile 2 and is
therefore corrected. \[live-confirmed + corrected]

FEE4 ACK observations distinguish the dialects:

| Packet | ACK | Evidence |
|:-------|:----|:---------|
| X3 16-bit checksum | `10 50 00 08` accepted | live-confirmed |
| Legacy low-byte-only checksum | `10 50 01 08` rejected | live-confirmed |

The Rust codec and X3-only production builder use this 16-bit checksum. The historical X11 formula remains documented above but is not emitted by the new Rust path.

## Custom macro binding

An assignment with firmware action `0x12` points a button at custom macro content identified by the third slot byte. The corresponding event pages must then be written with report [`0x09`](09-custom-macros.md).
