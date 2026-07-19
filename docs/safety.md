# Hardware and firmware safety

Prefer offline packet builders, fixtures, and CLI `hex` commands. Hardware access is not required to inspect or document the protocol.

## Configuration writes

- Start from a known-good packet and change one field at a time.
- Record the exact bytes, model, transport, response or ACK, observable effect, and recovery result.
- Use conservative delays between configuration packets. Stock X3 reset captures use about 500 ms.
- Treat a BLE ACK as parser acceptance only; it does not prove application or persistence.
- Do not fuzz arbitrary values or unchecked indices.

## Known-dangerous operations

- Do not run firmware updater executables as part of ordinary protocol investigation.
- Do not access BLE characteristics FFC1 or FFC2 during normal probing; their purpose and safety are unconfirmed.
- Do not send report `0x06` over X3 BLE. The firmware rejects it and the stock application explicitly skips it.
- Treat X3 BLE preferences report `0x05` with light-mode byte `0x00` as dangerous; it has crashed firmware in live testing.
- Treat experimental scroll-button remaps as unsafe because actions may repeat until unplug or reboot.
- Do not send report `0x0c` alone. It clears active configuration state and must be followed by a complete, model-correct configuration sequence.

## Recovery preparation

Before a hardware test, record the current configuration and prepare a known-good recovery sequence. If a configuration write leaves the mouse unresponsive, stop further writes and use the previously verified recovery path or power-cycle the device.

Protocol-specific warnings remain next to the affected fields. This page is the shared safety checklist, not a substitute for those warnings.
