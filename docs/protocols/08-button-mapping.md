# Button mapping (report `0x08`)

Report `0x08` writes the complete button-assignment table. The Rust codec implements it as `ButtonsReport`; custom macro event pages use the separate [`0x09`](09-custom-macros.md) report.

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Supported | implementation and existing samples |
| X3/M600 via FA60 receiver | USB HID | 59-byte X3 full report and `0xa0` readback | binary report + implementation |
| X3/FA61 wired | USB HID | Normal buttons live-confirmed; native Rust codec and selected-slot CLI are available | live-confirmed + capture-confirmed |
| X3/M600 BLE | BLE FEE3 | 16-bit checksum acceptance confirmed | live-confirmed |

## HID framing and payload

| Parameter | Value |
|:----------|:------|
| `wValue` | `0x0308` |
| `wIndex` | `0x0002` |
| Payload length | 59 bytes |

Writes use bytes 0–1 `08 3b`; FA60 prepared readbacks use `08 3d` while remaining 59 HID report bytes long. On X11, byte 2 is the fixed header value `01`; on X3/M600 it is the one-based target profile. Bytes 3–56 contain eighteen three-byte assignment slots. Each populated slot is encoded as:

```text
<firmware action> <modifier> <key code or action value>
```

A write replaces the full table. The native Rust FA61 CLI reads the complete
target table, changes one explicitly selected safe button slot, and writes the
resulting complete table back. This bounded-slot delta preserves all existing
slots while updating only the requested one. The CLI does not expose arbitrary
raw slots or scroll remaps.

An armed X3/M600 read also carries its one-based target at selector byte 4. Reading a
different target can replace the working button map without changing persistent
report-`0x0c` metadata, so profile changes and button read/write traffic must be
serialized.

The serialized FA60 hardware probe decoded and validated all eighteen slots
with the `0x3d` receiver readback declaration, then matched the complete final
button table to its pre-write backup. The probe did not modify any button slot.
\[live-confirmed]

## Logical button slots

The slot indices for common controls confirmed on X3/FA61 wired
(capture-confirmed 2026-07-24 for X3/FA60 receiver; DPI at index 3
live-confirmed):

| Button | Slot index | Byte offset |
|:-------|:-----------|:------------|
| Left | 0 | 3 |
| Right | 1 | 6 |
| Middle | 2 | 9 |
| DPI | 3 | 12 |
| Forward | 6 | 21 |
| Backward | 7 | 24 |

Scroll-up (index 4, offset 15) and scroll-down (index 5, offset 18) slots
exist in the 18-slot table but are intentionally not exposed by the CLI.
Direct scroll-wheel remaps remain unsafe because actions may repeat until
unplug or reboot. This concerns remapping the physical wheel slots
(indices 4 and 5). Binding Scroll Up or Scroll Down as an action on another
button is a normal button binding and is safe: it changes only that
button's slot and emits scroll events only while the button is held.

The DPI-button slot (index 3) is exposed through the safe CLI.
Interactive probe on FA61 wired and FA60 receiver (2026-07-28) confirmed
that slot 3 accepts arbitrary safe actions, including mouse buttons, DPI
controls, and profile navigation, without adverse firmware behavior.
\[live-confirmed] The native Rust CLI exposes left, right, middle, DPI,
forward, and backward slots.

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

## X3 firmware action codes

The following action bytes are confirmed on X3/FA61 wired (live-confirmed 2026-07-25):

| Action | Byte | Notes |
|:-------|:-----|:------|
| Disable | `0x01` | |
| Left click | `0x02` | |
| Right click | `0x03` | |
| Middle click | `0x04` | |
| Backward | `0x05` | |
| Forward | `0x06` | |
| Double click | `0x07` | |
| DPI cycle | `0x0d` | |
| DPI plus | `0x0e` | |
| DPI minus | `0x0f` | |
| Profile cycle | `0x34` | Wraps within 1..max |
| Profile plus | `0x35` | Clamps at max |
| Profile minus | `0x36` | Clamps at 1 |

Profile cycling actions are X3-specific and not present in the X11 `FirmwareAction` enum. Button bindings are per-profile: the cycle/plus/minus binding must be written to every profile that should respond to the physical button.

### Stock-app action palette — capture-confirmed 2026-08-14

The stock app's full button-assignment menu was captured on X3/FA61 wired
(2026-08-14) by rebinding button 4 (Forward slot, index 6) to each action
and recording the resulting `0x08` write. Every write is a complete 59-byte
table differing from the Forward baseline only at slot 6; all checksums
validate. Raw evidence: `docs/evidence/x3-fa61/captures/2026-08-14-button-actions.json`
and the paired session captures. \[capture-confirmed]

| Action | Byte | Notes |
|:-------|:-----|:------|
| Fire button | `0x08` | |
| Scroll up | `0x09` | factory wheel slots instead use `0x3c`; whether the encodings are interchangeable is unresolved |
| Scroll down | `0x0a` | |
| Easy aim (sniping DPI) | `0x10` | observed as `10 00 03`; the parameter's meaning is unconfirmed |
| Media player | `0x15` | |
| Previous track | `0x16` | |
| Next track | `0x17` | |
| Play / pause | `0x18` | |
| Stop | `0x19` | |
| Mute | `0x1a` | |
| Volume plus | `0x1b` | |
| Volume minus | `0x1c` | |
| Calculator | `0x1d` | |
| Email | `0x1e` | |
| Browser forward | `0x20` | |
| Browser backward | `0x21` | |
| Browser stop | `0x22` | |
| My computer | `0x23` | |
| Browser refresh | `0x24` | |
| Browser home | `0x25` | |
| Browser search | `0x26` | |
| Browser favorites | `11 03 12` | encoded as a keyboard shortcut: Ctrl+Shift+O |

The stock "Shortcut" presets are all keyboard-shortcut encodings (`0x11`,
modifier bits, keyboard-page HID usage), confirming the existing codec
encoding: \[capture-confirmed]

| Preset | Triplet | Keys |
|:-------|:--------|:-----|
| Cut | `11 01 1b` | Ctrl+X |
| Copy | `11 01 06` | Ctrl+C |
| Paste | `11 01 19` | Ctrl+V |
| Open | `11 01 12` | Ctrl+O |
| Save | `11 01 16` | Ctrl+S |
| Find | `11 01 09` | Ctrl+F |
| Redo | `11 01 1c` | Ctrl+Y |
| Select all | `11 01 04` | Ctrl+A |
| Print | `11 01 13` | Ctrl+P |
| Close window | `11 04 3d` | Alt+F4 |
| Swap windows | `11 04 2b` | Alt+Tab |
| Show desktop | `11 08 07` | Win+D |
| Run command | `11 08 15` | Win+R |
| Lock PC | `11 08 0f` | Win+L |
| Screen capture | `11 0a 16` | Win+Shift+S |

The captured triplets match the deployed web bundle's `FUN_*` wire table
(`FUN_BROWSER_FORWARD:[32,0,0]` ↔ `20 00 00`, `FUN_CUT:[17,1,27]` ↔
`11 01 1b`, …), cross-confirming both sources. The Rust codec types every
action above except Easy Aim (`0x10`, whose parameter is unconfirmed); a
custom recorded shortcut (for example Ctrl+Shift+A = `11 03 04`) emits the
same `0x11` encoding as the presets.

The Rust protocol crate exposes the verified encoding as
`X3ButtonAction`. It distinguishes parameterless confirmed actions from
keyboard shortcuts (`0x11`, modifier bits Ctrl=`0x01`, Shift=`0x02`,
Alt=`0x04`, Win=`0x08`, followed by a keyboard-page HID usage) and macro
references (`0x12`, zero modifier, reference in byte 2). `ButtonAssignment`
remains the lossless representation for unknown actions, unresolved firmware
revisions, and nonzero parameters that have not been confirmed. The manager's
safe API exposes the parameterless actions above plus the stock shortcut
presets and browser favorites; Easy Aim, custom macro references, and
arbitrary raw assignments remain outside it.

The stock native program's selector-to-wire table contains additional
media, browser, shortcut, and firmware-only entries. The media, browser,
and shortcut wire encodings are capture-confirmed on X3/FA61 wired
(2026-08-14, tables above) and typed in the Rust stack; the remaining
firmware-only entries still lack confirmed wire bytes. Native UI labels are
never a substitute for a captured wire byte — each entry must be observed
before it is typed.

Confirmed slot indices on X3/FA61 wired:

| Button | Slot index | Byte offset |
|:-------|:-----------|:------------|
| Left | 0 | 3 |
| Right | 1 | 6 |
| Middle | 2 | 9 |
| DPI | 3 | 12 |
| Forward | 6 | 21 |
| Backward | 7 | 24 |
