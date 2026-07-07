# X3 Wired / FA61 Quirks

This covers live testing notes for devices that enumerate as USB PID `fa61` in `x3-wired` mode, including FA61/Kysona M600/X3-family hardware.

## Identity and Aliases

- `x3-wired` is the current driver mode for PID `fa61` devices.
- Tested hardware may be sold or reported as FA61, Kysona M600, or X3-family models.
- These aliases are treated as the same protocol target for now, but branding and firmware behavior can vary.

## Verified Working

- `open` can open and close the device.
- DPI stage configuration works with 1-8 stages.
- DPI RID `04` includes X3 sensor toggles for LOD, ripple, angle snap, and motion sync.
- Polling rate changes work. Stock app captures match the driver encoding and use the same RID 06 packet layout:

  | Rate    | Stock payload (hex)      |
  |---------|--------------------------|
  | 125 Hz  | `06090108f700000000`     |
  | 250 Hz  | `06090104fb00000000`     |
  | 500 Hz  | `06090102fd00000000`     |
  | 1000 Hz | `06090101fe00000000`     |
- Debounce/key response changes work.
- Normal button binds work for left, right, middle, forward, and backward.
- `reset` has been observed resetting DPI and normal button binds on FA61 hardware.

## Button Bind Caveats

`bind` sends a full model-default mapping packet. It is not a partial update and does not preserve the current live device state for unspecified buttons. Any unspecified buttons are reset to the driver's model defaults.

The captured stock FA61/X3 wired macro packet is:

```text
083b010200000300000400000d00003c00000f00000600000500003c00000100000100000100000100000100000100000100000a000009000000c2
```

In that stock packet, offset `51` is `0x0a` for scroll down and offset `54` is `0x09` for scroll up. This is reversed relative to the X11 logical scroll offsets. For `x3-wired`, logical scroll-up binds therefore write offset `54`, and logical scroll-down binds write offset `51`.

Direct scroll binds are experimental and unsafe. They can technically work, but firmware or OS repeat behavior may cause the assigned action to repeat indefinitely until the mouse is unplugged or the host is rebooted.

DPI button binds appear to be ignored by stock FA61 firmware. The driver still allows sending DPI binds for experimentation, but stock firmware does not appear to expose DPI remapping.

## RGB and Preferences

RGB/light commands are likely no-ops on hardware that lacks an RGB base. Other user preferences, including debounce/key response, have been observed working.

## DPI and Sensor Fields

X3 wired/FA61 uses RID `04` for DPI and sensor toggles. These toggles are not in RID `07` on captured stock software packets.

| Offset | Field       | Values                     |
|:-------|:------------|:---------------------------|
| 3      | LOD         | `0x00` = 1mm, `0x01` = 2mm |
| 4      | Ripple      | `0x00` off, `0x01` on      |
| 5      | Stage mask  | `(1 << stageCount) - 1`, or `0xff` for 8 stages |
| 6      | Angle snap  | `0x00` off, `0x01` on      |
| 7      | Motion sync | `0x00` off, `0x01` on      |
| 8-15   | DPI lows    | Low bytes for stages 1-8 using `dpi / 50 - 1` |
| 16-23  | DPI highs   | High bytes for stages 1-8 using `dpi / 50 - 1` |

Unused X3 stages are zeroed (`00`) and disabled by the stage mask at offset `5`.

## Reset

Reset has been observed working for DPI and normal button binds on FA61 hardware. Captured stock software sends a `0c`, `04`, `05`, `06`, `08` packet sequence when applying defaults — notably **no `09` custom-macro definition pages**. Stock app appears to emit macro-definition pages only when macro content is explicitly modified/dirty.

Stock X3 reset `05` (user preferences) packet: `050f010003a80000ff010401af`
- Light mode Off (`00`), LED speed 3, deep sleep 10 min (`a8`), RGB blue `(0,0,255)`, sleep 0.5 min (`01`), key response 8 ms (`04`), checksum `af`.

Stock captures show ~500 ms inter-packet delay between each reset report (example: t=0 `0c`, +517ms `04`, +516ms `05`, +502ms `06`, +503ms `08`). The driver now applies `delayMs` between each of its reset packets so the device has time to process each report before the next arrives. Without delays, reset packets arrive back-to-back and the device may miss some of them — the most common symptom is needing to run `reset` twice to get all settings restored.

The driver reset flow intentionally skips the legacy custom-macro reset (`09` pages) for X3, because sending an empty BACKWARD custom macro (report `08` binding backward to `[0x12,0x00,0x08]` + empty `09` pages) was observed to break the back button on FA61 hardware. The `08` report alone (via `resetMacro()`) correctly restores the stock backward binding.

## Custom Macros

Custom macros work on FA61/X3 wired hardware. The page 0 and page 1 headers use `09 40 <button> <page>`. Page 2 uses `09 40` (byte 1 = `0x40`) on FA61, unlike the X11 protocol which uses `09 0c` (byte 1 = `0x0c`).

A minimal confirmed "Press A" macro on Forward button (button ID `0x07`) with loop count 1 produces:

- **Bind** (wValue `0x0308`): X3-default button map with Forward slot set to `[0x12, 0x00, 0x07]` (custom macro).
- **Page 0**: `09 40 07 00 …` with mode `0x00` at offset 4, times `0x01` at offset 8, event count `0x02` at offset 29, events `[01, 04]` press A and `[81, 04]` release A.
- **Page 1**: all zeros after header.
- **Page 2**: `09 40 07 02 …` with checksum `0x008D` at offsets 10–11.

Checksum = sum(page 0 bytes 8..63) + sum(page 1 bytes 4..63). Increasing the loop count to 2 changes offset 8 to `0x02` and the checksum to `0x008E`.

A long live FA61 capture validated spill into page 1: the stock UI produced 46 events (`0x2e`), page 0 held 17 events, page 1 held the remaining 29 events plus two zero bytes, and the final checksum was `0x0e06`.
