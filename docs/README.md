# Attack Shark mouse protocol documentation

This directory separates model support, packet formats, transports, research provenance, and raw evidence. Start with the device you have; report pages remain canonical for packet bytes shared across models.

## Documentation layers

| Layer | What it is | Source of truth | Status |
|:------|:-----------|:----------------|:-------|
| **Canonical** | Protocol pages, transport pages, device pages, and Rust source | Current Rust implementation (`attack-shark-x3`, `attack-shark-x3-manager`, `x3ctl`, `x3-gui`) | Active |
| **Evidence** | Raw descriptors, captures, packet dumps under `evidence/` | Immutable byte-for-byte hardware artifacts | Immutable |
| **Historical** | Deleted TypeScript/Bun tooling, old scripts, prior session records | Preserved for provenance only; not runnable | Deleted/archived |

**Current Rust sources of truth.** The workspace is `attack-shark-x3` (low-level driver), `attack-shark-x3-manager` (stateful manager), `x3ctl` (CLI), and `x3-gui` (Slint desktop frontend). No TypeScript/Bun product, broker, daemon, IPC layer, or direct mode remains. CLI invocations use `cargo run -p x3ctl -- ...`. Where historical TypeScript tooling is referenced for provenance, it is explicitly labeled deleted and the raw evidence is the authoritative record.

## Choose a device

- **[Attack Shark X11](devices/x11.md)** — X11 wired (`0xfa55`) and historical X11 2.4 GHz adapter evidence. PID `0xfa60` identifies a shared receiver and is not, by itself, an X11 model identifier.
- **[X3 / FA61 protocol target](devices/x3-fa61.md)** — X3, FA61, and Kysona M600-family findings for USB PID `0xfa61`, X3 through the shared `0xfa60` receiver, and experimental BLE. This is the current live-tested hardware path.

Brand aliases do not by themselves prove identical firmware. Each technical claim must remain qualified by model, transport, and evidence.

## Documentation map

| Area | Purpose |
|:-----|:--------|
| [`devices/`](#choose-a-device) | Model identity, support status, caveats, and reading paths |
| [`protocols/`](protocols/README.md) | Canonical packet layouts organized by report ID |
| [`transports/`](transports/README.md) | USB HID, BLE GATT, and browser transport behavior |
| [`research/`](research/README.md) | Dated investigations, binary-analysis provenance, and corrections |
| [`evidence/`](evidence/README.md) | Raw descriptors, captures, packet dumps, and model-specific analyses |
| [`ui-driver-spec.md`](ui-driver-spec.md) | Rust FA61 user-interface integration contract (composite `apply_profile_update`, resident pages) |
| [`safety.md`](safety.md) | Shared hardware-test and recovery restrictions (polling-rate preflight, rebind ambiguity) |

## Device identity and durable state

Manager durable state is schema 4 (`state.json`, `state.lock` plus per-device `*.operation.lock`). Device keys are logical `mouse-N` (`N >= 1`, canonical, allocated via `nextDeviceNumber`); serial numbers and HID paths are endpoint metadata only and never identity.

- **Endpoint is a locator:** `DeviceLocator::UsbPath(path)` is the current verbatim HID path, `BlePlatformId` for BLE; `DeviceEndpoint` stores `vendor_id`/`product_id` and optional trimmed `serial_number` as metadata.
- **Exact rediscovery is automatic:** discovery upserts a known `(transport, locator)` in place.
- **Cross-transport linkage is explicit:** `link_devices(source, target)` merges transports; discovery never auto-links by name/model/serial. No stable-serial or automatic-link claim.
- **Unique replug can update the locator:** `rebind_missing_endpoint` succeeds only with exactly one matching candidate (same VID/PID for USB); zero or multiple candidates fail and refuse to guess. Selection without an explicit `--device` and with multiple connected identities returns `AmbiguousDevice`.
- **Receiver is treated as permanently paired absent contrary evidence:** `fa60` identifies the shared receiver, not the mouse model; disappearance does not auto-unpair.

GUI-only preferences are separate: `gui-preferences.json` (schema 1) beside `state.json`, managed by `x3-gui/src/app_settings.rs` with coalescing atomic writes and backup on unreadable/unsupported version, no `state.lock` or hardware access. Slint pages (`ui/app-window.slint`) are kept resident only where user-relevant so drafts survive navigation.

## Packet reports

| Report | Purpose |
|:-------|:--------|
| [`0x05`](protocols/05-preferences.md) | Preferences, sleep, debounce, and lighting fields |
| [`0x06`](protocols/06-polling-rate.md) | Polling rate |
| [`0x07`](protocols/07-wakeup-mode.md) | Partially characterized wakeup mode |
| [`0x08`](protocols/08-button-mapping.md) | Button mapping |
| [`0x09`](protocols/09-custom-macros.md) | Custom macro event pages |
| [`0x0b`](protocols/0b-version.md) | Version and profile-state composite; byte [4] mirrors light mode |
| [`0x0c`](protocols/0c-profile-reset.md) | Profile actions and reset preparation |
| [Battery](protocols/battery.md) | X11 adapter interrupt report; unconfirmed FA60 X3/M600 candidate; X3 BLE battery path |

Coverage: `fixtures/protocol/` holds evidence-labeled golden vectors for DPI/prefs/buttons/profile and `input.json` (`disassembly+live-confirmed`/`live-confirmed`/`implementation`, with `decoded:null` for `0x07`/`0x09` explicitly unsupported — tested as ignored/rejected via `tests/input_codec.rs`). Checksums have boundary/wrapping integration via `tests/checksum_codec.rs`. `0x07`/`0x09` remain explicitly unsupported, not implemented; `0x06` has no golden fixture (live-alias side-effect only). See [`protocols/README.md`](protocols/README.md) for the matrix.

## Evidence vocabulary

| Tag | Meaning |
|:----|:--------|
| **live-confirmed** | Observed on a live device through a controlled probe or operation |
| **capture-confirmed** | Established from a packet capture or byte-for-byte export |
| **static-analysis** | Derived from binary or firmware analysis without executing the artifact |
| **implementation** | Describes current Rust production source behavior; not automatically hardware proof |
| **historical** | Preserved from prior session records but not independently reproduced |
| **inference** | Reasonable interpretation not directly observed |
| **corrected** | Supersedes an earlier interpretation; see the [correction ledger](research/corrections.md) |

More recent live or capture evidence overrides conflicting static analysis or inference. A successful BLE ACK proves parser acceptance only; it does not establish that a setting took effect or persisted. **Raw evidence under `evidence/` is immutable** — it must not be rewritten to match a theory or updated implementation.


## Contribution rules

- Current implementation claims must be grounded in the Rust source. Historical TypeScript/Bun references are provenance only and do not describe runnable tooling.
- X11 evidence is historical and unsupported by the current Rust implementation. It is preserved for dialect comparison and provenance, not for runtime behavior.
- Do not generalize X3 behavior to X11, BLE behavior to USB, or branding to protocol compatibility.
- Keep raw evidence byte-for-byte intact and put interpretation in protocol or research pages. Raw evidence is immutable.
- Give stable technical facts one canonical home; summaries should link rather than duplicate formulas.
- Read [`safety.md`](safety.md) before any hardware write.
