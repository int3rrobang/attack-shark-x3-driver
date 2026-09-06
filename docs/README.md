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
| [`logical-mouse-identity-spec.md`](logical-mouse-identity-spec.md) | Persistent physical identity design: watermark layout, ceremonies (Add another mouse / Restore / Adopt / BLE-associate), schema 5, identity rules |
| [`safety.md`](safety.md) | Shared hardware-test and recovery restrictions (polling-rate preflight, watermark-authenticated power-cycle verification) |

## Device identity and durable state

Manager durable state is schema 5 (`state.json`, sibling `state.lock`, plus a per-device `device-<id>.lock` operation lock; no migration from schema 4). Schema 5 adds installation-level identity mode (`Legacy`/`Persistent`), per-device physical watermark ids, and a durable resumable identity-setup journal. Event subscriptions hold the input session without holding the operation lock. Device keys are logical `mouse-N` (`N >= 1`, canonical, allocated via `nextDeviceNumber`); serial numbers and HID paths are endpoint metadata only and never identity.

- **Endpoint is a locator:** `DeviceLocator::UsbPath(path)` is the current verbatim HID path, `BlePlatformId` for BLE; `DeviceEndpoint` stores `vendor_id`/`product_id` and optional trimmed `serial_number` as metadata. In persistent identity mode the driver-owned watermark in the DPI tail is the per-unit identifier (see [`logical-mouse-identity-spec.md`](logical-mouse-identity-spec.md)).
- **Exact rediscovery is automatic:** discovery upserts a known `(transport, locator)` in place.
- **Cross-transport linkage comes from physical identity:** the same watermark observed over wired and receiver resolves to one logical mouse; a BLE endpoint attaches only through the explicit association ceremony. Discovery never auto-links by name/model/serial/VID-PID or connection timing; there is no `link_devices`-style merge API. No stable-serial or automatic-link claim.
- **A locator moves only with authentication:** in persistent identity mode a locator is replaced only by a reappearing endpoint whose current-profile watermark authenticates as the saved physical token (zero, multiple, malformed, unsupported, and foreign candidates are refused); legacy mode keeps the fuzzy same-locator or unique same-VID/PID behavior. Selection without an explicit `--device` and with multiple connected identities returns `AmbiguousDevice`.
- **Evidence-free shells are auto-dropped:** each discovery association removes identities with no configuration evidence whose every endpoint locator is now claimed by a *surviving* different identity — the residue of a port change resolved by a later locator update. Mutually claiming shells keep each other alive, and evidence-bearing identities are never auto-dropped; explicit removal is `forget` (refuses evidence-bearing identities without `--force`).
- **Display names are a lookup key:** `rename` sets `identity.display_name` (blank clears it); CLI device arguments accept a canonical `mouse-N` id or a unique case-insensitive display name. The `mouse-N` key itself never changes.
- **Receiver is treated as permanently paired absent contrary evidence:** `fa60` identifies the shared receiver, not the mouse model; disappearance does not auto-unpair.
- **GUI topology recovery is automatic:** operating-system USB hotplug notifications trigger rediscovery on receiver/wired arrival and removal (with a short Windows arrival settle delay); receiver radio connection events retain their input session across mouse power changes. A lightweight two-second endpoint scan is used only when the OS watcher cannot start. Battery input reports update telemetry only and never invalidate an open settings draft.

GUI-only preferences are separate: `gui-preferences.json` (schema 1) beside `state.json`, managed by `x3-gui/src/app_settings.rs` with coalescing atomic writes and backup on unreadable/unsupported version, no `state.lock` or hardware access — stores `validation_choice` (Transport/Readback), per-transport `baseline_choice_wired` (default Live) / `baseline_choice_receiver` (default Stored, BLE forced Stored), `allow_explicit_defaults`, appearance, DPI range, and last page. Slint pages (`ui/app-window.slint`) are kept resident only where user-relevant so drafts survive navigation. Non-rate GUI saves merge against the complete manager-originated profile snapshot already held by the draft, avoiding a duplicate pre-write hardware read; requested post-write confirmation and the mandatory polling-rate safety check remain unchanged.

## Physical identity (watermarks and ceremonies)

- **Identity mode:** `Legacy` (one fuzzy logical mouse, no per-unit claim, no
  watermark reads or writes) or `Persistent` (logical mice carry a
  driver-owned physical watermark). The first **Add another mouse** is the
  transition from Legacy to Persistent.
- **Watermark:** the 25-byte opaque tail of DPI report `0x04` — magic `X3ID`,
  format version 1, random 128-bit token, CRC-32 — see
  [`protocols/04-dpi.md`](protocols/04-dpi.md).
- **Resolution:** each discovered connection resolves to a logical mouse or to
  an unassociated connection with a specific reason (`Absent`, `Malformed`,
  `Unsupported`, `Unknown`, `Duplicate`, `Reserved`); discovery never
  manufactures a `mouse-N` for an unassociated connection.
- **Ceremonies** (`begin_identity_ceremony` / `identity_ceremony_action`):
  initial enrollment (Add another mouse, first time — both mice nearby), Add
  mouse, Restore (reassociate a mouse whose watermark was lost; rotates to a
  fresh token), adopt (add a foreign-tagged mouse with explicit confirmation),
  and BLE associate (explicit reconnect ceremony; BLE cannot verify a
  watermark).
- Power-cycle verification and locator updates authenticate by watermark in
  persistent mode — see [`safety.md`](safety.md).

The full design — storage and schema, stamping invariants, failure rules,
naming, lifecycle — lives in
[`logical-mouse-identity-spec.md`](logical-mouse-identity-spec.md).

## Packet reports

| Report | Purpose |
|:-------|:--------|
| [`0x04`](protocols/04-dpi.md) | DPI stages and sensor fields; carries the physical-identity watermark in persistent mode |
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
