# Hardware and firmware safety

Prefer offline packet builders, fixtures, and CLI `hex` commands. Hardware access is not required to inspect or document the protocol.

## Device identity and selection — no guessing

- Device identity is logical `mouse-N` (`N >= 1`) stored in `state.json` schema 4 (`nextDeviceNumber` allocation). No serial number or HID path is exposed as a stable identifier; `DeviceEndpoint.serial_number` is trimmed metadata only and HID `UsbPath` is a locator that can change on replug.
- **Exact endpoint rediscovery is automatic.** Matching `(transport, locator)` is upserted in place during discovery; no new logical device is created for a known locator.
- **Cross-transport linkage is explicit.** Discovery never auto-merges wired/receiver/BLE by model or serial. Adding a second transport to the same mouse requires explicit `link_devices(source, target, precedence)` with non-overlapping transports. `Refuse` (the CLI default) fails rather than discard evidence; `KeepTarget`/`KeepSource` explicitly discard one side's saved configuration; `Merge` copies a resource only into a completely empty target slot group (never mixing desired/observed across devices) and reports skipped source evidence. Do not claim stable-serial or automatic-link behavior.
- **Controlled unique replug can update the locator.** `rebind_missing_endpoint` (CLI `rebind`) replaces a missing USB locator only when exactly one connected candidate exists for that transport with the same VID/PID. Zero or multiple candidates return `InvalidUpdate` with the candidate count — the implementation refuses to guess. CLI `use <device>` selects an exact logical ID or a unique display name; `--device <ID>` is required when ambiguity exists.
- **Identity removal is explicit or provably redundant.** `forget` removes a saved identity and refuses evidence-bearing ones without `--force`. Discovery auto-drops only identities with no configuration evidence whose every locator is claimed by a surviving different identity; mutually claiming shells keep each other alive, and evidence-bearing identities are never auto-removed.
- **Ambiguity refuses to guess.** Resolving a device without an explicit ID when multiple connected logical identities exist returns `AmbiguousDevice` (list of candidates) rather than picking one. Rebind with ambiguous candidates is rejected the same way.
- **Power-cycle verification matches by model, not by unit.** The X3/M600 expose an empty serial number and the HID path can change on replug, so after a power cycle the verification workflow accepts the sole same-VID/PID candidate as the returning device. `PowerCycleVerified` therefore proves model-level persistence; it cannot distinguish two identical units.
- **Receiver is treated as permanently paired absent contrary evidence.** PID `fa60` identifies the shared receiver, not the mouse model. A receiver endpoint is not auto-removed on disconnect; removal is explicit state management.

## Configuration writes

- Start from a known-good packet and change one field at a time.
- Record the exact bytes, model, transport, response or ACK, observable effect, and recovery result.
- Use conservative delays between configuration packets. Stock X3 reset captures use about 500 ms. The FA60 receiver production path holds five seconds after writes because targeted traffic can perturb deferred profile loading and persistence.
- Treat a BLE ACK as parser acceptance only; it does not prove application or persistence. ACK status `0x00` means the firmware parsed the packet — it is not proof that the change took effect or was written to EEPROM.
- Over USB, `x3ctl` validates set/update writes by the transport acknowledgment by default (`--validation transport`): a USB `SET_REPORT` that succeeds at the HID API proves submission, not application or persistence. `--validation readback` (opt-in) adds an immediate armed readback of the affected fields, which proves only the device's current working state, not EEPROM persistence. Report `0x06` (polling rate) persistence was separately verified across a power-cycle on one wired device. [live-confirmed] Polling-rate live reads are alias-only (`read_live_polling_rate(alias)`); see [Polling-rate writes](#polling-rate-writes-report-0x06).
- BLE has no configuration readback path, so `--validation readback` is rejected over BLE; the default `--validation transport` accepts the BLE parser ACK, which proves parsing, not application or persistence. Full-state resources still require a complete durable-state baseline. On the first run for a new device, `--replace-defaults` authorizes the manager's evidence-qualified captured defaults as the baseline for omitted fields. Without a trusted or explicitly authorized baseline, BLE DPI, preferences, button, and profile commands fail before writing. Polling-rate writes follow a separate, stricter rule because report `0x06` skips the profile loader and can persist the complete live image under its target alias; see [Polling-rate writes (report `0x06`)](#polling-rate-writes-report-0x06).
- Do not fuzz arbitrary values or unchecked indices.
- The `--stateless` flag keeps state only in memory for the current invocation: the manager still reads, merges, and verifies within that run, but nothing is persisted to disk.
- The `--replace-defaults` flag is a per-operation authorization: it tells the manager to accept evidence-qualified captured defaults when no durable baseline exists for a field. It does not seed or write defaults into persistent state by itself.
- The `--dry-run` flag validates and prints what would be sent without touching hardware.
- Durable state separates desired values (what was written) from observed values (what was read back). A USB readback confirms application but not EEPROM persistence. `x3ctl verify --method profile-reload` or `--method power-cycle` tests persistence explicitly. `x3ctl state invalidate` preserves the desired and observed values but clears the persistence evidence tags, so the next operation re-verifies before trusting cached state. GUI preferences are separate (`gui-preferences.json` schema 1, no `state.lock`, atomic coalescing): they never affect hardware or durable device state.
### All-profile state refresh

`x3ctl profile refresh-all` is USB-only and is not a passive read. It may
temporarily raise the maximum enabled profile to five and activates every slot
so the live polling rate can be associated with the correct profile. The
manager captures DPI, preferences, buttons, and rate in one session, then
restores the exact original current/maximum metadata before committing any
fresh observations. A failure triggers the same restoration attempt; if
restoration also fails, the device may remain switched or expanded and the
error says so explicitly.

The operation preserves desired values, reports fresh desired/observed drift,
and invalidates older persistence claims. A successful refresh proves current
USB observations only; use profile-reload or power-cycle verification for
persistence evidence. Do not run it automatically at startup or behind an
ordinary refresh control.

## Polling-rate writes (report `0x06`)

Report `0x06` **skips the profile loader**: byte 2 is a save alias naming the
slot the deferred writer serializes the complete *live* profile image into.
A plain `0x06` write is therefore never a "just the rate" operation — it can
persist the current live DPI, preferences, and buttons under the target alias.
Targeted `0x06` reads mutate the alias too: they return the live profile's
rate, not the requested profile's stored rate. A `0x06` ACK or rate readback
proves only the rate field of the live alias; it never proves the non-rate
save image or EEPROM persistence. Authorization changes what may be sent,
never what may be claimed as verified.

### Safe USB procedure

`DeviceManager::update_polling_rate` is the only safe rate path. On USB it
performs this preflight and aborts **before any hardware write** when any
step fails:

1. Complete desired DPI, preferences, and buttons must exist for the target
   profile in durable state; a missing section fails with `MissingBaseline`.
2. Persistent metadata must have `current == target`; otherwise the write
   would save into a slot the user is not editing (`MissingBaseline`).
3. The complete profile is freshly read in the same session, and every
   non-rate section must exactly equal its desired value. Any mismatch aborts
   before the hardware write — `0x06` would otherwise persist the live image,
   not the intended profile image.
4. The current rate is read; an already-equal rate is a **no-op with no
   hardware write**, so no deferred flash save is scheduled.
5. Otherwise the rate is written **last** with the requested post-write
   validation: `--validation transport` (USB `SET_REPORT` submission success)
   or `--validation readback` (armed `0xa0` rate readback; proves immediate
   rate only, not persistence).

On BLE the same safe method fails with
`ManagerError::ExplicitAuthorizationRequired { operation: "update_polling_rate" }`
before any session call — BLE cannot freshly read the complete profile, so the
preflight is impossible there.

### Explicit BLE override policy

The dangerous BLE-only escape hatch is
`DeviceManager::update_polling_rate_unverified_ble`, reachable from the CLI
only through the command-specific flag:

```text
cargo run -p x3ctl -- --transport ble --profile 1 rate set 1000 --allow-unverified-ble-rate-write
```

- The flag authorizes exactly one `rate set` invocation; it has no effect on
  other commands. There is no broad danger flag.
- The override accepts transport validation only (`--validation readback` is
  rejected over BLE), performs the direct packet write, and returns **ACK-only
  evidence**: the BLE parser ACK (`10 50 00 06`) proves acceptance, not
  application or persistence. Persistence is recorded as `Unknown`.
- The safe `update_polling_rate` never downgrades to the override; the caller
  must choose the dangerous method explicitly. GUI/UI consumers must gate the
  same choice behind an explicit advanced toggle with a visible warning, not a
  hidden fallback.

### Evidence limits

- BLE ACK `10 50 00 06` = validated and queued, not durably committed.
- USB rate readback after `0x06` = the live alias's rate field only; it does
  not prove the non-rate save image and is not EEPROM proof.
- No-op (already-equal rate) writes produce no write evidence at all.
- The only persistence evidence remains the explicit diagnostics
  `x3ctl verify --method profile-reload|power-cycle`; nothing in the write
  path may claim persistence.

### Recovery guidance

Because a `0x06` write can persist the complete live image under the alias,
record the complete profile image (all sections) before rate experiments, and
recover by writing the complete model-correct profile image, not by sending
another standalone `0x06`. A complete capture-confirmed factory image is an
acceptable recovery source when user settings may be replaced; never treat a
partial section or a raw standalone `0x0c` as a backup. If the mouse becomes
unresponsive after a rate write, stop further writes and use the verified
recovery path or power-cycle the device.

## Known-dangerous operations

- Do not run firmware updater executables as part of ordinary protocol investigation.
- Do not access BLE characteristics FFC1 or FFC2 during normal probing; they are firmware-update/OAD characteristics, not configuration pipes.
- BLE report `0x06` writes are permitted only with explicit per-invocation authorization (`rate set --allow-unverified-ble-rate-write`); they skip the profile loader and can persist the complete live image under the target alias, so the safe manager path rejects BLE without that override. Earlier rejected attempts used malformed legacy-shaped packets; acceptance and persistence through later USB readback are live-confirmed, while immediate effective BLE polling behavior remains unconfirmed.
- A well-formed X3 BLE preferences report `0x05` may use light-mode byte `0x00`. The prior crash attribution was not reproduced on the same physical mice over USB or BLE and is corrected.
- Treat experimental scroll-wheel remaps as unsafe because actions may repeat until unplug or reboot. This concerns remapping the physical wheel slots (button-table indices 4 and 5); binding Scroll Up or Scroll Down as an action on another button is a normal button binding and is safe.
- Do not send a raw standalone report `0x0c`. The reset image clears active configuration state and must be followed by the complete model-correct sequence. Maximum-profile changes must go through `x3ctl profile max`, which preserves the current profile, rejects a maximum below it, observes the documented quiet period, and verifies metadata readback.

## Recovery preparation

Before a hardware test, record the current configuration and prepare a known-good recovery sequence. A backup is required when user settings must be preserved; when full replacement is acceptable, a complete capture-confirmed factory image can instead be the recovery source. Never treat a partial section or a raw standalone `0x0c` packet as a complete backup. If a configuration write leaves the mouse unresponsive, stop further writes and use the previously verified recovery path or power-cycle the device.

Protocol-specific warnings remain next to the affected fields. This page is the shared safety checklist, not a substitute for those warnings.
