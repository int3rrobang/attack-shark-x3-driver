# Corrections and retractions

This ledger is the canonical list of interpretations that were superseded by stronger evidence. Older research narratives may retain the original chronology, but the replacement below takes precedence.

| Retracted or corrected claim | Current finding | Evidence | Canonical reference |
|:-----------------------------|:----------------|:---------|:--------------------|
| `M600-5.2` and `M600-5.4` are firmware versions | Both names occur in one firmware payload and identify RF/host slots selected by `NVDS_TAG_RF_MODE` | corrected, static-analysis + live-confirmed | [BLE device identity](../transports/ble-gatt.md#device-identity) |
| FEE1 is a monotonic write counter | FEE1 is opaque and changes with reads or connection activity; FEE4 is the authoritative ACK path | corrected, live-confirmed | [FEE1](../transports/ble-gatt.md#fee1--opaque-readable-value-corrected) |
| Report `0x0c` changes the advertised RF identity | The profile parser reads RF mode to select layouts but does not write it; the physical pairing button changes the slot | corrected, static-analysis + live-confirmed | [`0x0c`](../protocols/0c-profile-reset.md) |
| Firmware can be updated through an ordinary FEE3 configuration write | No evidence supports firmware writes through FEE3; FFC0 may be update-related, but it was not accessed and remains unsafe | corrected, static-analysis + inference | [BLE services](../transports/ble-gatt.md#ffc0--data-service-handle-0x0059) |
| WebHID or WebUSB is a viable wired configuration transport | WebHID cannot issue the undeclared configuration feature reports; WebUSB cannot safely claim the OS-owned HID interface | corrected, live browser investigation | [Browser transport](../transports/browser.md) |
| The updater payload can be linearly disassembled as flat ARM code | The payload uses 32-byte data records plus two-byte CRC-16/CMS values; prior linear opcode interpretations were spurious | corrected, static-analysis | [Updater analysis](reverse-engineering-provenance.md#firmware-updater-payload-analysis) |

## Evidence precedence

More recent `live-confirmed` or `capture-confirmed` evidence overrides conflicting static analysis or inference. A correction does not erase provenance: the dated investigation and external-artifact summary remain available under this directory.
