# Wakeup mode (report `0x07`)

Report `0x07` is a partially characterized wakeup-mode command. Its packet format comes from static analysis of a WebHID factory-tool bundle; its effect has not been live-confirmed on the supported devices.

## Compatibility

| Variant | USB HID | X3 BLE | Evidence |
|:--------|:--------|:-------|:---------|
| X11 | Unknown | Not tested | No model-specific confirmation |
| X3/FA61 | Format known; behavior untested | Untested | static-analysis |

## Known write format

```text
07 08 <mode> <~mode> 00 ff 00 00
```

| Offset | Meaning |
|:-------|:--------|
| 0 | Report ID `0x07` |
| 1 | Command byte `0x08` |
| 2 | Mode: `0x01` for button wake, `0x02` for movement wake |
| 3 | One-byte complement of mode |
| 4–7 | Fixed bytes `00 ff 00 00` |

The complement pair is an integrity guard: `mode + ~mode == 0xff`.

## Read path

The analyzed factory bundle requests wakeup state through report `0x0c` with sub-command bytes `07 08`, not by reading report `0x07` directly. The response path and supported model set remain unconfirmed. See [`0c-profile-reset.md`](0c-profile-reset.md).

## Safety and implementation status

The production driver does not expose this report. Do not infer that the known packet shape makes the command safe or supported; preserve it as static evidence until a controlled, model-specific test confirms the effect and recovery behavior.
