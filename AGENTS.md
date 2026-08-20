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

- `src/main.rs` is the entire application: a Slint UI thread plus a dedicated
  manager worker thread that owns a current-thread Tokio runtime and the
  `DeviceManager`. The UI pushes `Command`s over a channel; the worker applies
  them and posts `UiEvent`s back through `slint::invoke_from_event_loop`.
- `ui/app-window.slint` defines the six-page window (overview, buttons,
  sensitivity, performance, device, settings) and the shared `Theme`; `assets/`
  holds the artwork.
- Holds no protocol codec logic. It edits drafts and calls `DeviceManager`
  deltas (`update_dpi_delta`, `update_button_slot`, `update_polling_rate`) with
  transport or readback verification.
- USB-only at runtime: apply operations reject BLE (configuration readback is
  unsupported) and reject combining polling-rate changes with DPI or button
  changes in one apply, so the report `0x06` preflight stays isolated.
- Deviations from workspace norms: the only crate using Tokio, and it
  `allow`s `unsafe_code` for Slint's generated item-tree glue (the sole
  carve-out from the workspace `forbid`).

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

- Edition 2024, `rust-version = "1.85"`, resolver 3.
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

### Durable state model

The manager persists device configuration to a JSON state file protected by a short-lived cross-process file lock (`fs2`). It never auto-applies desired state at startup, device discovery, or GUI launch; applying state is always an explicit user operation.

- `ResourceState<T>` carries `desired: Option<DesiredState<T>>` and `observed: Option<ObservedState<T>>`. Both are optional; an empty resource is valid.
- `DesiredState<T>` records the value, `DesiredSource` (`UserWrite`, `Imported`, `ExplicitDefaults`), `Verification`, and `updated_at` timestamp.
- `ObservedState<T>` records the value, `ObservationSource` (`UsbReadback`), and `observed_at` timestamp.
- `Verification` carries `ApplicationVerification` (write acknowledgement or readback evidence) and `PersistenceVerification` (whether the value survived a power cycle).
- `SCHEMA_VERSION` is bumped on every incompatible state change. The loader rejects unknown versions.

### Write pipeline

A resource write generally follows this sequence:
1. Merge the caller's delta into the existing baseline (or `--replace-defaults` baseline).
2. Send the complete packet over the transport.
3. On USB: readback the affected fields and compare. Record `ApplicationVerification::ReadbackVerified`.
4. On BLE: record `ApplicationVerification::Acknowledged` (no readback path exists).
5. Persist the updated desired state and verification evidence to the state file under the cross-process lock.
6. Return a `WriteOutcome<T>` with the final state and evidence.

The cross-process lock guards the state-file read-modify-write, not the transport I/O itself. Different resources may vary in their exact sequencing; consult the implementation.

### Polling-rate (report `0x06`) write model — mandatory invariant

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

- Preserve established X11 wired/adapter output unless the task explicitly changes it with independent evidence.
- Gate X3-specific layouts and checksums by the appropriate model/connection mode. Accidental compatibility from checksum overflow or default values is not evidence.
- Packet builders remain transport-independent. USB/BLE framing and device access belong in the driver module.
- Low-level experimental tools may expose raw writes, but production-facing APIs must validate ranges and block known-dangerous operations.
- Do not rename unknown fields based only on host UI labels. Describe how bytes are used when semantics are unresolved.
- Do not describe RF slots as firmware versions or profile "personas."
- Report `0x09` custom macros remain out of scope until a separately reviewed Rust implementation exists.
- Codec integration tests live under `crates/attack-shark-x3/tests/` using golden fixtures from `fixtures/protocol/`. Not every codec has a test yet; add one when modifying a codec.

## Evidence labels

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
