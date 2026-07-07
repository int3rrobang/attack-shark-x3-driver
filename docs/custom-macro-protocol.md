# Custom Macro Communication Protocol (Report 0x0308 / 0x0309)

This document describes the USB HID communication protocol used to configure custom macros for the Attack Shark X11 mouse, as implemented in the `CustomMacroBuilder` class.

## Overview

Applying a custom macro involves sending a sequence of four USB HID `SET_REPORT` packets. Each packet transmits different parts of the macro configuration, including button assignment, play options, macro events, and a validation checksum.

## USB HID Request Parameters

All packets are sent using the following base parameters:

- **bmRequestType**: `0x21` (Host-to-Device, Class-specific, Interface)
- **bRequest**: `0x09` (SET_REPORT)
- **wIndex**: `2` (Interface index)

The **wValue** differs between the first packet and the subsequent ones.

---

## Packet 1: Button Assignment

The first packet assigns a specific button to execute the custom macro. It uses a `MacrosBuilder` instance to generate the payload.

- **wValue**: `0x0308` (Report Type: Feature, Report ID: 0x08)
- **Payload size**: 64 bytes (usually the first 59 bytes are relevant)

The packet structure follows the standard button mapping protocol, where the target button's action is set to a specific firmware action that triggers macro playback.

---

## Packet 2: Macro Header & Events (Page 0)

This packet contains the macro configuration, playback options, and the first set of macro events.

- **wValue**: `0x0309` (Report Type: Feature, Report ID: 0x09)
- **Payload size**: 64 bytes

| Offset | Field              | Type    | Description                                                                 |
|:-------|:-------------------|:--------|:----------------------------------------------------------------------------|
| 0      | Header 1           | `uint8` | Fixed value `0x09`                                                          |
| 1      | Header 2           | `uint8` | Fixed value `0x40`                                                          |
| 2      | Button ID          | `uint8` | Target button (e.g., `0x08` for Extra Button 5)                             |
| 3      | Page Index         | `uint8` | Fixed value `0x00` (Page 0)                                                 |
| 4      | Play Mode          | `uint8` | Playback mode: `0x00` (Times), `0x01` (Toggle), `0x02` (Hold), `0xFF` (Loop)|
| 8      | Repeat Count       | `uint8` | Number of times to repeat (used if Play Mode is `0x00`; `0x01`–`0xFF`)      |
| 29     | Event Count        | `uint8` | Total number of macro events (2 bytes per event)                            |
| 30-63  | Macro Events (P0)  | `uint8` | Up to 17 macro events (34 bytes)                                            |

### Playback Modes (`MacroSettings`)

- **0x00**: Play N times (N is set in Offset 4).
- **0x01**: Play until any key is pressed.
- **0x02**: Play while the button is pressed, stop on release.
- **0xFF**: Special flag used when "Play N times" is selected with N > 1.

---

## Packet 3: Macro Events (Page 1)

This packet contains the continuation of the macro events.

- **wValue**: `0x0309` (Report Type: Feature, Report ID: 0x09)
- **Payload size**: 64 bytes

| Offset | Field              | Type    | Description                                                                 |
|:-------|:-------------------|:--------|:----------------------------------------------------------------------------|
| 0      | Header 1           | `uint8` | Fixed value `0x09`                                                          |
| 1      | Header 2           | `uint8` | Fixed value `0x40`                                                          |
| 2      | Button ID          | `uint8` | Target button ID                                                            |
| 3      | Page Index         | `uint8` | Fixed value `0x01` (Page 1)                                                 |
| 4-63   | Macro Events (P1)  | `uint8` | Up to 30 macro events (60 bytes)                                            |

---

## Packet 4: Validation & Footer (Page 2)

This packet serves as the termination of the macro configuration and includes a checksum for validation.

- **wValue**: `0x0309` (Report Type: Feature, Report ID: 0x09)
- **Payload size**: 64 bytes

| Offset | Field              | Type    | Description                                                                 |
|:-------|:-------------------|:--------|:----------------------------------------------------------------------------|
| 0      | Header 1           | `uint8` | Fixed value `0x09`                                                          |
| 1      | Header 2           | `uint8` | `0x0C` on X11/non-X3 devices; `0x40` on FA61/X3 wired hardware             |
| 2      | Button ID          | `uint8` | Target button ID                                                            |
| 3      | Page Index         | `uint8` | Fixed value `0x02` (Page 2)                                                 |
| 10     | Checksum High      | `uint8` | High byte of the 16-bit checksum (Big Endian)                               |
| 11     | Checksum Low       | `uint8` | Low byte of the 16-bit checksum (Big Endian)                                |

---

## Macro Event Encoding

Each macro event consists of **2 bytes**:

- **Byte 1 (Delay & Flags)**:
    - **Bits 0-6**: Delay value (calculated using the formula: `2 * Math.floor((ms + 5) / 20) + 1`).
    - **Bit 7 (0x80)**: Release flag. If set, the event signifies a key release.
- **Byte 2 (Key/Action)**:
    - Standard Keyboard Key Code (e.g., `0x04` for 'A').
    - Mouse Event Code (e.g., `0xF1` for Left Click).

### Delay Calculation Logic

The mouse uses a specific rounding logic for event delays to map milliseconds to the 7-bit delay field. Both keyboard and mouse events follow this rule:

```typescript
function computeDelayByte(ms: number): number {
    return 2 * Math.floor((ms + 5) / 20) + 1;
}
```

Common values:
- **10ms**: `0x01`
- **15ms**: `0x03`
- **20ms**: `0x03`
- **35ms**: `0x05`
- **55ms**: `0x07`
- **75ms**: `0x09`
- **95ms**: `0x0B`
- **110ms**: `0x0B`
- **115ms**: `0x0D`
- **255ms**: `0x1B`

### Long Delays (> 1070ms)

When a delay exceeds 1070ms, it is decomposed into a base delay and extra delay units using a special event type (`0x03`).

1.  **Extra Units**: `Math.floor(delayMs / 200)`
2.  **Remainder**: `delayMs % 200` (mapped using the `computeDelayByte` formula)
3.  **Event 1**: `[computeDelayByte(rem), KeyCode]`
4.  **Event 2 (Extra Delay)**: `[extraUnits, 0x03]`

### Mouse Event Codes

| Event         | Code   |
|:--------------|:-------|
| Left Click    | `0xF1` |
| Right Click   | `0xF2` |
| Middle Click  | `0xF3` |
| Backward Click| `0xF4` |
| Forward Click | `0xF5` |

---

## Checksum Calculation

The checksum is a 16-bit sum of specific byte ranges from Packet 2 and Packet 3.

```typescript
function calculateChecksum(packet2: Buffer, packet3: Buffer): number {
    let sum = 0;
    // Sum all bytes in Packet 2 from index 8 onwards
    for (let i = 8; i < 64; i++) {
        sum += packet2[i];
    }
    // Sum all bytes in Packet 3 from index 4 onwards
    for (let i = 4; i < 64; i++) {
        sum += packet3[i];
    }
    return sum & 0xFFFF;
}
```

The result is stored in Big Endian format in **Packet 4** at indices 10 and 11.

---

## Live Capture Example: FA61/X3 Wired – Forward Button → "Press A", Loop 1

This is a minimal confirmed macro captured from an FA61/PID `fa61` device in `x3-wired` mode.
- Target button: Forward (button ID `0x07`, `CUSTOM_MACRO_BUTTONS.EXTRA_BUTTON_4`)
- Mode: Play N times, N = 1
- Events: press `A` (KeyCode `0x04`), release `A` (KeyCode `0x04`)

**Bind packet** (wValue `0x0308`):
```
083b010200000300000400000d00003c00000f00001200070500003c00
000100000100000100000100000100000100000100000a000009000000d5
```

**Page 0** (wValue `0x0309`):
```
09 40 07 00  00 00 00 00  01 00 00 00  00 00 00 00
00 00 00 00  00 00 00 00  00 00 00 00  00 02
01 04  81 04  00 00 ... (zeros to byte 63)
```

- Offset 4 (play mode): `0x00` (THE_NUMBER_OF_TIME_TO_PLAY)
- Offset 8 (repeat count): `0x01`
- Offset 29 (event count): `0x02`
- Events: `[01, 04]` press A (10 ms delay), `[81, 04]` release A (10 ms delay)

**Page 1** (wValue `0x0309`): all zeros after header (all events fit in page 0).

**Page 2** (wValue `0x0309`):
```
09 40 07 02  00 00 00 00  00 00 8d 00  00 00 00 00 ... (zeros to byte 63)
```

- Header byte 1 is `0x40` on FA61 (not `0x0C` as on X11/non-X3).
- Checksum: `0x008D` = `0x01` (repeat) + `0x02` (count) + `0x01` + `0x04` + `0x81` + `0x04` (events).

**Loop 2** changes only offset 8 to `0x02` and checksum to `0x008E`:
