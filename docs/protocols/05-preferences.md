# Preferences (report `0x05`)

Report `0x05` carries lighting fields, sleep timers, and debounce/key-response configuration. X11 and X3 share the field area but use different checksum layouts.

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Supported | implementation |
| X3/FA61 wired | USB HID | Device behavior confirmed; builder checksum gap | live-confirmed + capture-confirmed |
| X3/M600 BLE | BLE FEE3 | Confirmed with safety restriction | live-confirmed |

## HID Report Parameters

| Parameter | X11 Wired | X11 Wireless (Adapter) | X3 Wired |
|:----------|:----------|:-----------------------|:---------|
| **Request Type** | `0x21` | `0x21` | `0x21` |
| **Request ID** | `0x09` | `0x09` | `0x09` |
| **Value** | `0x0305` | `0x0305` | `0x0305` |
| **Index** | `0x0002` | `0x0002` | `0x0002` |
| **Packet size** | 13 bytes | 15 bytes | 13 bytes |

## Payload Layout — X11

X11 uses a 13-byte (wired) or 15-byte (wireless) payload with an 8-bit checksum and a state-flag byte.

| Index | Name          | Description                                          |
|:------|:--------------|:-----------------------------------------------------|
| 0     | Header 1      | `0x05`                                               |
| 1     | Header 2      | `0x0F`                                               |
| 2     | Header 3      | `0x01`                                               |
| 3     | Light Mode    | Selects the LED animation mode.                      |
| 4     | Configuration | Combined byte: `(Deep Sleep Bucket << 4) \| (LED Speed & 0x0F)` |
| 5     | Deep Sleep    | Encoded deep sleep timer: `0x08 + (Minutes * 0x10)`. |
| 6     | Red           | RGB Red component (0-255).                           |
| 7     | Green         | RGB Green component (0-255).                         |
| 8     | Blue          | RGB Blue component (0-255).                          |
| 9     | Sleep Timer   | Sleep timer in half-minutes: `Minutes * 2`.          |
| 10    | Debounce      | Encoded key response time: `((ms - 4) / 2) + 2`.     |
| 11    | State Flag    | Dynamic flag based on colors and mode (X11 only).    |
| 12    | Checksum      | 8-bit: sum of bytes from index 3 to 10, modulo 256.  |
| 13-14 | Padding       | `0x00 0x00` (Wireless mode only).                    |

## Payload Layout — X3

X3 uses a fixed 13-byte payload with a **16-bit big-endian checksum** at bytes 11–12 and **no state-flag byte**.

| Index | Name          | Description                                          |
|:------|:--------------|:-----------------------------------------------------|
| 0     | Header 1      | `0x05`                                               |
| 1     | Header 2      | `0x0F`                                               |
| 2     | Target Profile | X3 one-based working-profile target (`0x01`–`0x05`) |
| 3     | Light Mode    | Selects the LED animation mode. **Warning: `0x00` crashes firmware over BLE.** \[live-confirmed] |
| 4     | Configuration | Combined byte: `(Deep Sleep Bucket << 4) \| (LED Speed & 0x0F)` |
| 5     | Deep Sleep    | Encoded deep sleep timer: `0x08 + (Minutes * 0x10)`. |
| 6     | Host color byte 1 | Stock UI labels this as red; no X3 hardware effect is confirmed. |
| 7     | Host color byte 2 | Stock UI labels this as green; no X3 hardware effect is confirmed. |
| 8     | Host color byte 3 | Stock UI labels this as blue; no X3 hardware effect is confirmed. |
| 9     | Sleep Timer   | Sleep timer in half-minutes: `Minutes * 2`.          |
| 10    | Debounce      | Encoded key response time: `((ms - 4) / 2) + 2`.     |
| 11–12 | Checksum      | **16-bit big-endian**: sum of bytes 3..10, masked to 16 bits. |
|       | *(no padding)* | X3 wired is always 13 bytes.                       |

For X3/M600, the same target profile must be supplied at byte 4 of an armed `0xa0`
preferences read. A read targeted at another profile can load that profile's working
image without changing persistent report-`0x0c` metadata.

### X3 checksum formula

```
checksum = sum(bytes[3..10]) & 0xffff   // 16-bit big-endian at bytes[11..12]
```

### Live ACK evidence (X3 over BLE)

| Packet type | Checksum variant | FEE4 ACK | Evidence |
|:------------|:-----------------|:---------|:---------|
| X3 0x05 prefs | 16-bit BE (bytes 11–12) | `10 50 00 05` (accepted) | live-confirmed |
| X3 0x05 prefs | 8-bit / state-byte (X11 format) | `10 50 01 05` (rejected) | live-confirmed |

> Source: [dated browser investigation](../research/2026-07-browser-investigation.md#4-checksum-facts) §4.1–4.3

> The production X3 builder emits this 16-bit big-endian checksum. Parser acceptance does not establish that the host-labeled lighting fields have a hardware effect.

---

## Field Details

### 1. Light Modes (Index 3)

| Mode            | Hex Value | Description                               |
|:----------------|:----------|:------------------------------------------|
| Off             | `0x00`    | LEDs disabled. **Unsafe over BLE on X3** — `0x00` causes firmware crash. Use `0x10` or higher. \[live-confirmed] |
| Static          | `0x10`    | Fixed color.                              |
| Breathing       | `0x20`    | Pulse animation with single color.        |
| Neon            | `0x30`    | Cycling rainbow effect.                   |
| Color Breathing | `0x40`    | Pulse animation cycling through colors.   |
| Static DPI      | `0x50`    | Color based on current DPI stage (Fixed). |
| Breathing DPI   | `0x60`    | Pulsing color based on current DPI stage. |

> **X3 hardware note**: This hardware has **no configurable RGB lighting**. The host-labeled color bytes are still present in the packet but have no confirmed visible effect. Treat them as opaque preserved bytes on X3 rather than meaningful RGB state. Light/sleep field semantics are **not fully characterized**. \[inference]

### 2. Deep Sleep Configuration (Index 4 & 5)

The mouse enters a deep power-saving mode after a period of inactivity.

> **X3 caveat**: Sleep and deep-sleep behaviour is **not fully characterized** on X3. The fields follow the same encoding but actual sleep/deep-sleep transitions have not been confirmed via live testing on X3 hardware. \[inference]

- **Minutes Range**: 1 to 60.
- **Index 5 Formula**: `0x08 + (Minutes * 0x10)`
- **Bucket (Index 4 High Nibble)**:
    - `0`: 1–16 minutes
    - `1`: 17–32 minutes
    - `2`: 33–48 minutes
    - `3`: 49–60 minutes
    - Formula: `floor((minutes - 1) / 16)`

### 3. LED Speed (Index 4 Low Nibble)

- **Range**: 1 (Slowest) to 5 (Fastest).
- **Hardware Encoding**: The value sent to the device is inverted: `6 - UserSpeed`.
    - Speed 1 (Slowest) -> `5`
    - Speed 5 (Fastest) -> `1`
- The current builder uses value `3`, matching the captured empty-profile image. This is not established as a factory or firmware default.
- **X3 note**: No visible effect on this hardware. Preserve the field when updating an existing profile.

### 4. Host-labeled color bytes (Index 6, 7, 8)

The stock UI labels these bytes as RGB and they control color on X11. On X3 they are present in the profile image but have no confirmed visible effect. A decoder should preserve the raw three-byte value; exposing it as meaningful X3 RGB configuration would overstate the evidence.

### 5. Sleep Timer (Index 9)

Normal sleep (standby) before deep sleep.
- **Minutes Range**: 0.5 to 30.0.
- **Value**: `Minutes * 2` (e.g., 2 mins = `4`, 0.5 mins = `1`).

### 6. Debounce / Key Response (Index 10)

- **Range**: 4 ms to 50 ms (Must be an even number).
- **Formula**: `((ms - 4) / 2) + 2`
    - 4ms -> `2`
    - 8ms -> `4`
    - 50ms -> `25`

### 7. State Flag (Index 11) — X11 only

This byte acts as a status indicator for the firmware on X11 hardware. **X3 does not have a state-flag byte** — bytes 11–12 are the 16-bit checksum.

- **Threshold**: A color component is "active" if it is `>= 100` (`0x64`).
- **Count**: Number of active components (0 to 3).
- **Base Logic**:
    - If Mode is `Breathing DPI` (`0x60`): `State = Count + 1`
    - Otherwise: `State = Count`

### 8. Checksum — X11 (Index 12)

Calculated as the sum of bytes from index 3 to 10, modulo 256.
`Checksum = (Byte[3] + Byte[4] + ... + Byte[10]) & 0xFF`

### 9. Checksum — X3 (Index 11–12)

Calculated as the sum of bytes from index 3 to 10, masked to 16 bits, stored big-endian at bytes 11–12.
`Checksum = (Byte[3] + Byte[4] + ... + Byte[10]) & 0xFFFF`

X3 rejects packets with an 8-bit or state-byte checksum (X11 format) — see the live ACK evidence table above. \[live-confirmed]
