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
| 6     | Red           | RGB Red component (0-255).                           |
| 7     | Green         | RGB Green component (0-255).                         |
| 8     | Blue          | RGB Blue component (0-255).                          |
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

> ⚠️ **Implementation gap**: Protocol behaviour is confirmed, but the current production TypeScript `UserPreferencesBuilder` still emits the legacy 8-bit/state-byte checksum format (X11). X3 0x05 packets require a 16-bit big-endian checksum at bytes 11–12. Default packets can accidentally appear valid when the high byte or legacy state byte happens to coincide with the correct 16-bit value. Model-specific checksum fixes in the builder are **not yet implemented**.

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

> **X3 hardware note**: This hardware has **no configurable RGB lighting**. Light-mode and RGB-color fields are still present in the packet and must contain valid values, but they have no visible effect. Light/sleep field semantics are **not fully characterized** on X3. \[inference]

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
- **Default**: 3.
- **X3 note**: No visible effect on this hardware (no configurable RGB). Must still contain a valid value.

### 4. RGB Colors (Index 6, 7, 8)

Used for Static and Breathing modes (X11). On X3, these fields are present but have no visible effect — the hardware has no configurable RGB lighting. For modes like Neon or Color Breathing, these values might be ignored by the device but are usually sent as default or last used color.

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
