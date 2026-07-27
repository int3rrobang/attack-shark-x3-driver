# Hardware and firmware safety

Prefer offline packet builders, fixtures, and CLI `hex` commands. Hardware access is not required to inspect or document the protocol.

## Configuration writes

- Start from a known-good packet and change one field at a time.
- Record the exact bytes, model, transport, response or ACK, observable effect, and recovery result.
- Use conservative delays between configuration packets. Stock X3 reset captures use about 500 ms.
- Treat a BLE ACK as parser acceptance only; it does not prove application or persistence.  ACK status `0x00` means the firmware parsed the packet — it is not proof that the change took effect or was written to EEPROM.
- Over USB, `x3ctl` verifies configuration writes through immediate readback of the affected fields.  This readback proves only the device's current working state, not EEPROM persistence.  Report `0x06` (polling rate) persistence was separately verified across a power-cycle on one wired device. [live-confirmed]
- BLE has no configuration readback path. `x3ctl` therefore cannot verify BLE writes through readback. The CLI requires a complete durable-state baseline. `state init-defaults` creates an explicit desired-state baseline without reading or writing hardware; `--replace-defaults` authorizes using it for omitted fields. Without a trusted or explicitly authorized baseline, BLE DPI, preferences, button, and profile commands fail before writing.
- Do not fuzz arbitrary values or unchecked indices.
- The `--no-state` flag suppresses durable-state readback and writeback for a single invocation; `--stateless` also bypasses the broker (`--direct --no-state`).  In normal operation the broker merges each command delta into the durable-state file after every successful hardware write.
- The `--dry-run` flag validates and prints what would be sent without touching hardware.
- Every persistent write carries a provenance tag in durable state so the apply pipeline can distinguish readback-verified values from synthesised defaults, user writes, and imported exports.

## Known-dangerous operations

- Do not run firmware updater executables as part of ordinary protocol investigation.
- Do not access BLE characteristics FFC1 or FFC2 during normal probing; they are firmware-update/OAD characteristics, not configuration pipes.
- Valid BLE report `0x06` writes are permitted when built by the typed protocol encoder. Earlier rejected attempts used malformed legacy-shaped packets; acceptance and persistence through later USB readback are live-confirmed, while immediate effective BLE polling behavior remains unconfirmed.
- A well-formed X3 BLE preferences report `0x05` may use light-mode byte `0x00`. The prior crash attribution was not reproduced on the same physical mice over USB or BLE and is corrected.
- Treat experimental scroll-button remaps as unsafe because actions may repeat until unplug or reboot.
- Do not send a raw standalone report `0x0c`. The reset image clears active configuration state and must be followed by the complete model-correct sequence. Maximum-profile changes must go through `x3ctl profile max`, which preserves the current profile, rejects a maximum below it, observes the documented quiet period, and verifies metadata readback.

## Recovery preparation

Before a hardware test, record the current configuration and prepare a known-good recovery sequence. If a configuration write leaves the mouse unresponsive, stop further writes and use the previously verified recovery path or power-cycle the device.

Protocol-specific warnings remain next to the affected fields. This page is the shared safety checklist, not a substitute for those warnings.
