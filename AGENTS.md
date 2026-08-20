# Repository Guide for Agents

## Project scope

This repository is a Rust workspace implementing a cross-platform protocol library, manager, CLI, and desktop GUI for Attack Shark X3 and M600-family mice. The architecture is a four-crate dependency chain:

```
x3-gui ──┐
x3ctl ───┼──→ attack-shark-x3-manager  →  attack-shark-x3
 (GUI)   │        (manager/state)           (protocol/driver)
 (CLI)   ┘
```

There is no TypeScript, Bun, or Node runtime in production.

## Sources of truth

- `Cargo.toml` (workspace root) and each crate's `Cargo.toml` define the build graph, features, and lint policy.
- Start with `docs/README.md` for protocol documentation, device selection, and the evidence legend.
- Treat `crates/attack-shark-x3/src/protocol/` as the canonical packet layouts and `crates/attack-shark-x3/src/driver/` as the hardware access layer.
- Treat `crates/attack-shark-x3-manager/src/state/` as the durable-state model and `crates/attack-shark-x3-manager/src/resources/` as the per-resource write/readback pipeline.
- Treat `fixtures/protocol/*.json` as golden codec fixtures.
- Treat files under `docs/evidence/`, `docs/samples/`, JSONL probe results, and packet captures as evidence. Do not rewrite raw evidence to match a theory.
- More recent live-confirmed or capture-confirmed evidence overrides static inference. Clearly label unconfirmed interpretations.
- Keep model/transport distinctions explicit. Do not generalize an X3 result to X11, or BLE behavior to USB, without evidence.

## Crate boundaries

### `attack-shark-x3` — protocol library and driver

The leaf crate. Owns protocol codecs (`protocol::dpi`, `protocol::preferences`, `protocol::buttons`, `protocol::polling_rate`, `protocol::profile`, `protocol::input`), the checksum module, the `model` types (`DpiValue`, `ProfileId`, `StageIndex`, `TransportKind`, `PollingRate`), protocol-level types such as `LiftOffDistance` (`protocol::dpi`), and the driver module (USB HID via `hidapi`, BLE via `bluest`).

- `lib.rs` re-exports selected types from submodules for convenience.
- Every public type derives `Debug`. Protocol types with `serde` feature derive `Serialize`/`Deserialize`.
- `#![forbid(unsafe_code)]` is enforced crate-wide.

### `attack-shark-x3-manager` — headless manager and durable state

The middle crate. Depends on `attack-shark-x3` with `default-features = false, features = ["serde"]`.

- `state/` owns `StateFile`, `StateStore`, `ResourceState<T>`, `DesiredState<T>`, `ObservedState<T>`, `Verification`, and the schema version constant.
- `resources/` contains per-resource modules (`dpi`, `settings`, `buttons`, `state`) that implement delta-merge, write, readback, and persistence logic against `DeviceManager`.
- `backend.rs` is the transport session abstraction (USB and BLE).
- `verification.rs` implements profile-reload and power-cycle verification workflows.
- `manager.rs` is the `DeviceManager` type: device discovery, session management, and the public typed-operation API.
- `operation.rs` defines `WriteOutcome<T>`, `DiscoveredDevice`, `DeviceStatus`, `ProfileVerificationOutcome`, `PowerCycleVerificationOutcome`, `UpdatePolicy`, and `DeviceEvent`.
- `offline_debug.rs` provides packet-building utilities that need no hardware.
- `#![forbid(unsafe_code)]` is enforced crate-wide.
- Feature-gated modules (`backend`, `manager`, `events`, `resources`, `verification`) are private implementation details that compile only under `usb` or `ble`. The public API surface is `DeviceManager` and the types re-exported from `lib.rs`.

### `x3ctl` — thin CLI frontend

A frontend. Depends on `attack-shark-x3-manager` with `default-features = false`.

- `main.rs` contains argument parsing (via `clap` derive), action construction, dispatch, and output formatting. The `x3ctl/args.rs` module defines the CLI structure; `x3ctl/output.rs` handles `--output human|json` formatting.
- The CLI builds a typed `Action` from parsed arguments, then calls `DeviceManager` methods. It does not contain protocol logic, state management, or driver code.
- Global flags: `--stateless`, `--dry-run`, `--replace-defaults`, `--output human|json`, `--transport auto|wired|receiver`, `--device <ID>`, `--profile <N>`.
- Commands: `devices`, `use`, `status`, `profile get|set`, `dpi get|set`, `rate get|set`, `prefs get|set`, `bind get|set`, `battery`, `verify`, `export`, `import`, `state selected|invalidate`, `debug dpi|prefs|buttons`.
- `#![forbid(unsafe_code)]` is enforced crate-wide.

### `x3-gui` — Slint desktop frontend

The desktop frontend. Depends on `attack-shark-x3-manager` with `default-features = false`.
`serde` is retained only for `app_settings` (`gui-preferences.json`); no protocol or manager state is duplicated.

- `src/main.rs` bootstraps Slint, installs callbacks, and owns the `Command` queue and `UiEvent` projection. It never touches protocol codecs or hardware directly.
- `src/worker.rs` owns the manager worker thread: a current-thread Tokio runtime plus the `DeviceManager`. The UI pushes `Command`s over a channel; the worker applies them through the single composite `DeviceManager::apply_profile_update` path and posts `UiEvent`s back via `slint::invoke_from_event_loop`.
- `src/presentation.rs` is pure display helpers (labels, status strings, validation shims) with no hardware or lock access.
- `src/projection.rs` projects `LiveSnapshot`/`DeviceListEntry` into Slint models and handles draft-preserving versus draft-replacing snapshot application.
- `src/app_settings.rs` persists GUI-only preferences to a sibling `gui-preferences.json` with its own `schemaVersion = 1` (beside `state.json`, no `state.lock` or hardware access, atomic coalescing write).
- `ui/app-window.slint` defines the six-page window (overview, buttons, sensitivity, performance, device, settings) and the shared `Theme`; `assets/` holds artwork. Pages are kept resident (visibility toggles, not recreation) only where user-relevant — draft and scroll state survive navigation, but pages are not duplicated or speculatively preloaded beyond what the UI needs.
- Holds no protocol codec logic. Drafts are validated through `presentation` helpers and applied as typed deltas via the composite manager operation; polling-rate changes are never coalesced with DPI or button changes so the report `0x06` preflight stays isolated.
- At runtime the GUI discovers USB (`wired`/`receiver`) and BLE devices. BLE shows saved desired settings (not live readback) and supports profile activation with ACK-only evidence; most configuration writes, `profile refresh-all`, polling-rate changes, and both verification workflows remain USB-only because configuration readback is unsupported over BLE.
- Deviations from workspace norms: the only crate using Tokio, and it `allow`s `unsafe_code` for Slint's generated item-tree glue (the sole carve-out from the workspace `forbid`).
## Feature matrix

| Feature | `attack-shark-x3` | `attack-shark-x3-manager` | `x3ctl` | `x3-gui` |
|:--------|:------------------|:--------------------------|:--------|:---------|
| `usb` (default) | `hidapi` driver | forwards | forwards | forwards |
| `ble` | `bluest` + `windows` driver | forwards | forwards | forwards |
| `serde` | protocol type derives | always-on via dependency | — | — |

Each crate re-exports the feature to its dependency. Consumers use `default-features = false` and select only what they need.

## Development commands

```bash
# Full workspace
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# Single crate
cargo build -p attack-shark-x3
cargo test -p attack-shark-x3-manager --all-features
cargo clippy -p x3ctl --all-targets --all-features -- -D warnings

# Run the CLI
cargo run -p x3ctl -- --help
cargo run -p x3ctl -- devices
cargo run -p x3ctl -- debug dpi --stages 800,1600,2400 --active-stage 2

# Run the desktop GUI
cargo run -p x3-gui

# Focused test
cargo test -p attack-shark-x3 --test dpi_codec
```

Run focused tests while developing, then run the full workspace suite before completion. CI runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on Linux; on Windows CI runs `cargo check` and `cargo test` (no fmt or clippy step).

## Rust and Clippy conventions

- Edition 2024, `rust-version = "1.92"` (workspace), `rust-toolchain.toml` pins `channel = "1.97.1"`, resolver 3.
- `#![forbid(unsafe_code)]` is workspace policy enforced at crate root. Never weaken it.
- Workspace-level Clippy: `all = { level = "warn", priority = -1 }`. CI treats all warnings as errors (`-D warnings`).
- Prefer `thiserror` for error enums. Use `#[error(…)]` messages that include context (paths, values, transport).
- Use `serde` derives on protocol and state types; prefer `#[serde(rename_all = "camelCase")]` for JSON field naming matching existing convention.
- Prefer `#[must_use]` on pure query methods.
- Keep packet offsets, fixed bytes, checksums, and model-specific branches named and testable.
- Prefer readonly inputs and explicit unions/enums for constrained protocol values.
- Never use `unsafe` blocks. The entire workspace forbids them.
- Match `cargo fmt` output. Do not hand-format.

## Manager, state, and verification invariants

### Device identity — schema 4 logical model

Schema 4 uses a manager-generated stable key `mouse-N` (`DeviceId`, `N >= 1`, canonical without leading zeros, allocated via `StateFile::next_device_number`). No serial or HID path is exposed as identity.

- `DeviceEndpoint` is an endpoint locator, not an identity. USB keeps `DeviceLocator::UsbPath` (the current verbatim HID path for `hidapi` open) plus `vendor_id`/`product_id` and optional `serial_number` metadata (trimmed, blank → `None`). BLE keeps `DeviceLocator::BlePlatformId`. Serial is metadata only and never used to compose `DeviceId`.
- `DeviceIdentity` owns `BTreeMap<TransportKind, DeviceEndpoint>` and an optional `preferred_transport`. Human `display_name` is presentation only.
- `StateFile` is `state.json` (schemaVersion 4) beside derived `state.lock` (cross-process `fs2` lock). GUI preferences are **not** in this file — see `x3-gui` crate boundary; they live in sibling `gui-preferences.json` schema 1 with coalescing atomic writes and no hardware/lock access.
- **Exact endpoint rediscovery is automatic.** `DeviceManager::list_devices` associates discovered endpoints by exact `(transport, locator)` equality and upserts the endpoint in place (refreshing `display_name` only when previously absent). No new `mouse-N` is allocated for a known locator.
- **Cross-transport linkage is explicit.** Discovery never auto-merges transports by VID/PID, name, or serial. Adding a second transport to an existing logical mouse requires `DeviceManager::link_devices(source, target)` which moves all endpoints from `source` into `target` only when transports do not overlap and only when at most one side carries configuration evidence; otherwise it fails. There is no stable-serial or automatic-link claim.
- **Controlled unique replug can update the locator.** `DeviceManager::rebind_missing_endpoint` (and sync `try_rebind_with_candidates`) replaces a missing endpoint's locator when exactly one connected candidate exists for that transport with the same VID/PID (USB) and no stored locator matches. Ambiguity refuses to guess: zero candidates or multiple candidates return `ManagerError::InvalidUpdate` describing the candidate count, and `resolve_device` without an explicit `--device` and without a valid selected device returns `AmbiguousDevice` when multiple connected logical identities exist.
- **Receiver is treated as permanently paired absent contrary evidence.** PID `fa60` identifies the shared receiver, not the mouse model; the receiver endpoint is not auto-unpaired on disconnect. Removal requires explicit state management, not transport disappearance.
- Validation: `DeviceIdentity::validate` checks endpoint-key coherence, non-blank locators, and trims; `StateFile::validate` calls it and rejects unknown `SCHEMA_VERSION`. Checked `serde` deserialization rejects non-canonical `mouse-N`, unknown schema versions, and incoherent endpoints with backup preservation for GUI preferences; see coverage notes below.

### Durable resource state

The manager persists device configuration to that `state.json` file protected by the short-lived cross-process file lock. It never auto-applies desired state at startup, device discovery, or GUI launch; applying state is always an explicit user operation.

- `ResourceState<T>` carries `desired: Option<DesiredState<T>>` and `observed: Option<ObservedState<T>>`. Both are optional; an empty resource is valid.
- `DesiredState<T>` records the value, `DesiredSource` (`UserWrite`, `Imported`, `ExplicitDefaults`), `Verification`, and `updated_at` timestamp.
- `ObservedState<T>` records the value, `ObservationSource` (`UsbReadback`), and `observed_at` timestamp.
- `Verification` carries `ApplicationVerification` (write acknowledgement or readback evidence) and `PersistenceVerification` (whether the value survived a power cycle).
- `SCHEMA_VERSION` is bumped on every incompatible state change. The loader rejects unknown versions (currently 4; schema 3 documents are rejected with no migration).

### Write pipeline — composite `apply_profile_update`

A resource write goes through the single composite `DeviceManager::apply_profile_update(device, profile, ProfileUpdate, UpdatePolicy)` which owns one `DeviceOperationGuard` and one transport session for non-rate resources. The GUI `worker` and CLI both use this path; there is no frontend-specific polling isolation.

1. Validate the update is non-empty and that polling-rate is never mixed with DPI/preferences/buttons (rejected before any hardware open).
2. If the update contains only `polling_rate`, run the mandatory `0x06` preflight (see next section) and write the rate last; no other resources are touched.
3. Otherwise open one session and, for each present field in `ProfileUpdate { dpi, preferences, buttons }`, load the baseline (`Live` re-read or `Stored` per policy; BLE forced stored-baseline with `--replace-defaults` when needed), merge deltas, send the complete packet, collect `SessionWrite` evidence, and coalesce button slots into one write.
4. On USB: readback the affected fields and compare. Record `ApplicationVerification::ReadbackVerified` on match, `Mismatch` otherwise. On BLE: record `Acknowledged` (no readback path).
5. Persist the updated desired state and verification evidence to the state file under the cross-process lock (one `mutate_async` for all non-rate outcomes).
6. Return a `ProfileUpdateOutcome { dpi, preferences, buttons, polling_rate }` where only included resources are `Some(WriteOutcome<T>)`.

The cross-process lock guards the state-file read-modify-write, not the transport I/O itself. Per-operation locks (`*.operation.lock` per `mouse-N`) guard transport I/O.

Report `0x06` **skips the profile loader**; byte 2 is a save alias, not a load
target. The deferred writer serializes the complete *live* image into the slot
named by byte 2, so a `0x06` write can persist the current live DPI,
preferences, and buttons under the target alias. Targeted `0x06` reads mutate
the alias instead of loading: they return the live profile's rate, and the
readback's byte 2 is a wire-shape check, never a content proof. A `0x06` ACK
or rate readback never proves the non-rate save image or persistence.

Safe rate updates go through `DeviceManager::update_polling_rate(device, profile, rate, policy)`:

- **USB:** requires complete desired DPI/preferences/buttons for the target,
  requires persistent metadata `current == target`, freshly reads the complete
  profile in the same session, and compares every non-rate section — any
  mismatch aborts before a hardware write. The current rate is then read; an
  already-equal rate is a **no-op with no hardware write** (redundant writes
  would otherwise schedule a deferred flash save). Otherwise the rate is
  written last with the requested post-write validation (`Transport` or
  `Readback`).
- **BLE:** fails with `ManagerError::ExplicitAuthorizationRequired` before any
  session call.

The explicitly dangerous BLE-only escape hatch is
`DeviceManager::update_polling_rate_unverified_ble(...)`: it rejects non-BLE
transports and `Readback` validation, sends the direct packet, and returns
ACK-only evidence with persistence `Unknown`. The CLI reaches it only through
the command-specific `--allow-unverified-ble-rate-write` flag on `rate set`;
there is no broad danger flag. Low-level driver methods are named `unchecked`
and carry no safety precondition: `MouseHandle::{send_polling_rate_unchecked,
write_polling_rate_unchecked}` and `BleHandle::write_polling_rate_unchecked`.

Authorization changes what may be sent, never what may be claimed as verified.

### Verification workflows

- **Profile reload** (`verify` command): reads a target profile, switches away and back, compares. USB-only; returns `ProfileVerificationOutcome`.
- **Power cycle** (`verify` command): waits for device disconnect, then reconnect, then compares all resources. Returns `PowerCycleVerificationOutcome`.
- Both workflows are in `verification.rs` and execute through `DeviceManager`.

## Thin-frontend rule

`x3ctl` and `x3-gui` are presentation layers only. They MUST NOT:
- Contain protocol codec logic (byte offsets, checksums, packet construction).
- Manage state files or locks directly.
- Access hardware outside of `DeviceManager` calls.

If a new feature needs protocol knowledge, implement it in `attack-shark-x3` or `attack-shark-x3-manager` and expose it through `DeviceManager`. The CLI builds a delta, calls the manager, and formats the result; the GUI does the same from its worker thread (`x3-gui` constructs the manager from the default `StateStore` and reads state through `DeviceManager::store()`, but never performs its own read-modify-write or lock handling).

## Protocol implementation conventions

- The device supports 1–8 configurable DPI stages (`StageIndex` 1..=8, `DpiValue` 50..=26000 step 50). The physical DPI button is a separate auxiliary HID input report `03 00 10 <stage> 00` (six positions observed in captures); do not conflate the two.
- Targeted reads for `0x04`/`0x05`/`0x08` load the target working profile via byte 2 / selector byte 4 and can change live behavior without necessarily changing persistent `0x0c` current metadata. Report `0x06` skips that loader: it is a live-rate read and a save-alias write (byte 2 names the slot the deferred writer serializes the complete live image into). `MouseHandle::read_live_polling_rate(alias)` treats the alias as a wire side effect — the returned rate is always the live image, verified by `last_polling_alias` side-effect tests.
- USB configuration readback is supported via the armed `0xa0` selector (`MouseHandle::read_*` on `wired`/`receiver`); BLE has no configuration readback path and returns ACK-only `10 50 00 <report>`.
- Battery level on X3/M600 FA60 is 1–10 (×10 = percentage) per X3.exe disassembly and `03 10 40 01 <level>` captures; reject 0 and >10. X11 legacy is 0–100 directly.
- Preserve established X11 wired/adapter output unless the task explicitly changes it with independent evidence.
- Gate X3-specific layouts and checksums by the appropriate model/connection mode. Accidental compatibility from checksum overflow or default values is not evidence.
- Packet builders remain transport-independent. USB/BLE framing and device access belong in the driver module.
- Low-level experimental tools may expose raw writes, but production-facing APIs must validate ranges and block known-dangerous operations.
- Do not rename unknown fields based only on host UI labels. Describe how bytes are used when semantics are unresolved.
- Do not describe RF slots as firmware versions or profile "personas."
- Report `0x07` (wakeup mode) and `0x09` (custom macros) remain explicitly unsupported in the Rust driver/manager/`x3ctl`/GUI — format known from static analysis/captures, no exposed API, no claim of device effect, explicitly tested as ignored/rejected (`fixtures/protocol/input.json` `unsupported-wakeup-0708`/`unsupported-macro-0900` with `decoded:null` and `tests/input_codec.rs::unknown_and_unsupported_reports_remain_unsupported`), not implemented.
- Checked `serde`: `StateFile`, `DeviceId` (`mouse-N` canonical), `DeviceEndpoint` coherence, and `GuiPreferences` reject unknown `schemaVersion`, non-canonical identities, and incoherent locators; invalid documents are not silently migrated.
- Live polling alias: the polling-rate read path is tested as side-effect-only via `ScriptedFakeSession::last_polling_alias`; a bare alias read without a preceding `read_profile(target)` is never profile-scoped proof.
- Coverage: `0x04`/`0x05`/`0x08`/`0x0c` have golden fixtures (`fixtures/protocol/dpi.json`, `preferences.json`, `buttons.json`, `profile.json`); `0x03` input events have evidence-labeled golden integration via `fixtures/protocol/input.json` + `tests/input_codec.rs` (`golden_fixtures_decode_as_labeled`, battery/malformed/unsupported including `0x07`/`0x09` rejected); 16-bit checksums have boundary/wrapping integration via `tests/checksum_codec.rs`; `0x06` polling rate has no golden fixture (live-alias side-effect only via `last_polling_alias`); `0x07`/`0x09` remain explicitly unsupported with no fixtures and are tested as ignored/rejected, not implemented. Do not claim fixture coverage beyond this; add a fixture when modifying a codec.

Use the vocabulary from `docs/README.md`:

| Tag | Meaning |
|:----|:--------|
| **live-confirmed** | Observed on a live device through a controlled probe or operation |
| **capture-confirmed** | Established from a packet capture or byte-for-byte export |
| **static-analysis** | Derived from binary or firmware analysis without executing the artifact |
| **implementation** | Describes current production source behavior; not automatically hardware proof |
| **historical** | Preserved from prior session records but not independently reproduced |
| **inference** | Reasonable interpretation not directly observed |
| **corrected** | Supersedes an earlier interpretation; see `docs/research/corrections.md` |

More recent live or capture evidence overrides conflicting static analysis or inference. A successful BLE ACK proves parser acceptance only; it does not establish that a setting took effect or persisted.

## Hardware and firmware safety

Prefer offline builders, fixtures, `debug` commands, and `--dry-run`. Do not access hardware unless the user explicitly requests a hardware test.

When hardware testing is authorized:

- Change one field at a time from a known-good packet.
- Back up or record the current state and prepare a known-good recovery sequence first.
- Use conservative delays between configuration packets.
- Record exact bytes, transport, response/ACK, observable effect, and recovery outcome.
- Do not fuzz arbitrary values or unchecked indices.
- Do not run firmware updater executables.
- Do not access or write firmware-update characteristics such as FFC1/FFC2 during normal probing.
- Treat experimental scroll-wheel remaps as unsafe because they may repeat indefinitely until unplug/reboot. This concerns remapping the physical wheel slots (button-table indices 4 and 5); binding Scroll Up or Scroll Down as an action on another button is a normal button binding and is safe.
- An ACK means the parser accepted a packet; it does not by itself prove application or persistence.

Full safety checklist is in `docs/safety.md`. Read it before any hardware write.

## User-facing copy

- **Read `docs/ui-copy.md` and apply it whenever you add or edit any
  user-facing text** — GUI strings (`x3-gui/ui/app-window.slint`,
  `x3-gui/src/main.rs`), `x3ctl` human output, and `ManagerError` messages that
  can surface to a user.
- The guide defines the voice, a confidence ladder (applied → confirmed →
  survives switching → survives power-off), a vocabulary table (e.g. never
  "readback", "persistence", "baseline", "evidence", "preflight" in user copy),
  and a blocklist. Keep every control and option; only the wording changes.
- Engineering vocabulary stays in `docs/`, code comments, and JSON/debug
  output; it is not exposed as normal user-facing prose.

## Documentation rules

- Update the relevant protocol document under `docs/protocols/` when implementation behavior or confirmed understanding changes.
- Include exact packet bytes and variant/transport context for important protocol claims.
- Use evidence labels defined above.
- Preserve provenance for copied samples. Do not include private session databases, machine-local paths, or third-party reports wholesale; summarize relevant evidence in-repo.
- Keep relative Markdown links valid after documentation changes.
- Give stable technical facts one canonical home; summaries should link rather than duplicate.

## Repository hygiene

- Do not edit generated artifacts directly.
- Do not modify `Cargo.lock` unless dependencies intentionally change.
- Do not commit probe logs, captures, binaries, or extracted artifacts without explicit direction.
- Do not commit, amend, push, or create a pull request unless requested.
- Remote names and URLs are checkout-specific. Before pushing, inspect the configured remotes and target branch, and push only to an explicitly authorized destination.
- This repository is an independent Rust implementation for the X3/M600 protocol family. The historical X11 repository is reference material, not an active implementation upstream or a runtime dependency.
- If the historical X11 remote is retained for provenance, name it `historical-x11` rather than `upstream` to avoid implying an active synchronization target.
- Keep unrelated existing working-tree changes intact.
