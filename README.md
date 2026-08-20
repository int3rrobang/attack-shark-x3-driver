# attack-shark-x3-driver

A Rust desktop app, CLI, and library for configuring Attack Shark X3, Kysona M600, and related X3/M600-family mice.

This project began as a fork of
[`HarukaYamamoto0/attack-shark-x11-driver`](https://github.com/HarukaYamamoto0/attack-shark-x11-driver)
and now deliberately targets the X3/M600 protocol family. The original Git history, MIT
license, and attribution are preserved, but X11 is no longer a production target.

## Transports

| Transport | Host identity | CLI value | Status |
|:----------|:--------------|:----------|:-------|
| USB wired | VID `1d57`, PID `fa61` | `wired` | Supported; USB configuration readback via armed `0xa0` selector (live-confirmed) |
| 2.4 GHz receiver | VID `1d57`, PID `fa60` | `receiver` | Supported; USB configuration readback via armed `0xa0` selector (live-confirmed) |
| BLE GATT | FEE0 service | `ble` | Requires `--features ble`; ACK-confirmed writes only (`10 50 00 <report>`), no configuration readback; ACK is parser acceptance only |

FA61 and FA60 share the same X3/M600 packet dialect. Transport selection controls
discovery and compact versus padded feature-report lengths. The device supports
**1–8 configurable DPI stages**; the physical DPI button is a separate auxiliary HID input
report (`03 00 10 <stage> 00`) — six physical positions were observed in captures, which is
not the same as the 1–8 stage limit.


### Capability truth table

| Capability | USB (`wired`/`receiver`) | BLE (`ble`) |
|:-----------|:-------------------------|:------------|
| Device discovery | `devices` | `devices` (connected FEE0 only) |
| DPI read | `dpi get` (live readback, loads working profile) | Unsupported |
| DPI write | `dpi set` + readback verify | ACK-confirmed; no readback (ACK = parser acceptance only) |
| Preferences read | `prefs get` (live readback, loads working profile) | Unsupported |
| Preferences write | `prefs set` + readback verify | ACK-confirmed; no readback (ACK = parser acceptance only) |
| Button table read | `bind get` (live readback, loads working profile) | Unsupported |
| Button table write | `bind set` + readback verify | ACK-confirmed; no readback (ACK = parser acceptance only) |
| Polling rate read | `rate get` (live-rate read; `0x06` skips loader, byte 2 is save alias) | Unsupported |
| Polling rate write | `rate set` + readback verify (USB-only safe path; `0x06` save alias) | ACK-confirmed; no readback — requires `--allow-unverified-ble-rate-write` (ACK = parser acceptance only) |
| Profile read/activate | `profile get` / `profile set` | ACK-confirmed; no readback |
| Battery | `battery` (USB receiver interrupt report `03 10 40 01 <level>` 1–10 ×10%) | BLE Battery Service `0x180f`/`0x2a19` (not via `battery` CLI); `battery` command is receiver-only |
| Durable state | Full (readback-verified provenance) | Requires `--replace-defaults` when no baseline exists |
| Persistence verification | `verify --method profile-reload`, `verify --method power-cycle` | Not available |

USB readback verification proves the device's current working state only; it does not
prove EEPROM persistence. Targeted DPI/preferences/button reads load that target's working
profile without necessarily changing persistent `0x0c` current metadata; `0x06` polling-rate
skips that loader — it is a live-rate read and a save-alias write (see
[`06-polling-rate.md`](docs/protocols/06-polling-rate.md)). Report `0x06` (polling rate) persistence was separately
verified across a power-cycle on one wired device. [live-confirmed]

## Workspace architecture

```
attack-shark-x3-rust/
├── crates/
│   ├── attack-shark-x3/          # Low-level protocol library: HID framing,
│   │                              #   DPI/prefs/buttons/polling-rate codecs,
│   │                              #   USB and BLE transport drivers
│   ├── attack-shark-x3-manager/  # Headless operations manager: durable state,
│   │                              #   read-modify-write pipeline, safety policy,
│   │                              #   verification workflows, offline debug encoders
│   ├── x3ctl/                    # CLI frontend: Clap-based resource-oriented commands
│   └── x3-gui/                   # Slint desktop frontend: six-page control window,
│                                  #   worker-thread DeviceManager access
├── scripts/
│   ├── persistence-probe.ps1               # Wired true power-cycle persistence probe
│   └── profile-switch-transport-probe.ps1  # Profile-switch transport comparison probe (wired/receiver)
│   # historical scripts/fa61-test-suite.ps1 runner is not included — see docs/evidence/x3-fa61/captures/README.md
└── fixtures/protocol/            # Protocol fixture JSONs (dpi, preferences, buttons, profile, input — evidence-labeled golden vectors; checksums via boundary integration)
```

- **`attack-shark-x3`** — protocol codecs (DPI/prefs/buttons/profile golden fixtures plus `protocol::input` evidence-labeled golden integration via `fixtures/protocol/input.json` + `tests/input_codec.rs` and `protocol::checksum` boundary/wrapping integration via `tests/checksum_codec.rs`; `0x07`/`0x09` remain explicitly unsupported and tested as ignored/rejected `decoded:null`, not implemented), USB HID and BLE GATT transport drivers, DPI-button event subscription, polling-rate live alias (`read_live_polling_rate(alias)` side-effect only).
- **`attack-shark-x3-manager`** — stateful operations: per-device logical `mouse-N` identity (schema 4, `nextDeviceNumber`), multi-transport endpoints, cross-process `state.lock` plus per-device `*.operation.lock`, read-modify-write pipeline with single composite `apply_profile_update` (DPI+preferences+buttons coalesced in one session; polling rate isolated with mandatory preflight), provenance tracking, verification workflows, and offline packet generation through `debug` commands.
- **`x3ctl`** — the CLI binary. Talks to hardware through the manager crate; `--stateless` keeps state only in memory for the current invocation. Uses `DeviceManager::apply_profile_update` for all typed writes; polling-rate is never coalesced with other resources. Human output is friendly labels; raw protocol detail remains in `--output json` / `debug` / `--dry-run`.
- **`x3-gui`** — the Slint desktop frontend. Split modules: `main.rs` (bootstrap + callbacks), `worker.rs` (Tokio manager worker owning `DeviceManager`), `presentation.rs` (pure display helpers), `projection.rs` (Slint model projection), `app_settings.rs` (separate `gui-preferences.json` schema 1, coalescing atomic writes, no `state.lock` or hardware). `ui/app-window.slint` renders the six-page window; pages are kept resident only where user-relevant so draft and scroll state survive navigation without speculative preloading. The GUI edits drafts and applies them via the composite manager operation with transport or readback verification. `serde` is retained only for `app_settings`.

All crates default to USB. BLE requires `--features ble` on each crate in the
dependency chain.

## Build and test

Toolchain: workspace `rust-version = "1.92"` (edition 2024, resolver 3); `rust-toolchain.toml` pins `channel = "1.97.1"` (CI installs 1.97.1 on Linux and Windows).

```bash
# Build (USB only)
cargo build

# Build x3ctl with BLE support
cargo build -p x3ctl --features ble

# Run tests
cargo test

# Run x3ctl tests with BLE
cargo test -p x3ctl --features ble

# Clippy lint
cargo clippy

# Install the CLI
cargo install --path crates/x3ctl

# Run the desktop GUI (discovers USB and BLE; most writes and all polling-rate changes require USB)
cargo run -p x3-gui
```

## CLI

All commands use the package-run form. Replace with `x3ctl` after `cargo install`.

```bash
cargo run -p x3ctl -- <OPTIONS> <COMMAND>
```

### Global flags

| Flag | Purpose |
|:-----|:--------|
| `--transport <auto\|wired\|receiver[\|ble]>` | Select transport (default: `auto`; `ble` requires `--features ble`) |
| `--device <ID>` | Exact stable device ID from `x3ctl devices` |
| `--profile <N>` | Target profile number (default: `1`) |
| `--stateless` | Keep state only in memory for this invocation |
| `--dry-run` | Validate and print without hardware or state access |
| `--replace-defaults` | Authorize evidence-qualified captured defaults when no baseline exists |
| `--output <human\|json>` | Output format (default: `human`) |

### Device discovery and selection

```bash
# List discoverable devices
cargo run -p x3ctl -- devices

# List devices on a specific transport
cargo run -p x3ctl -- --transport receiver devices

# Select a device for subsequent commands
cargo run -p x3ctl -- use '<device-id>'
```

### Status

```bash
# Read device and state status
cargo run -p x3ctl -- status

# JSON output
cargo run -p x3ctl -- --output json status
```

### Reads

```bash
# Read DPI configuration
cargo run -p x3ctl -- dpi get

# Read preferences for profile 2
cargo run -p x3ctl -- --profile 2 prefs get

# Read the complete button table
cargo run -p x3ctl -- bind get

# Read polling rate
cargo run -p x3ctl -- rate get

# Read current profile
cargo run -p x3ctl -- profile get

# Rebuild USB observations for all five profile slots, restoring the original
# current profile and maximum afterward
cargo run -p x3ctl -- profile refresh-all

# Read battery (USB receiver only)
cargo run -p x3ctl -- battery
```

Targeted reads for DPI (`0x04`), preferences (`0x05`), and button table (`0x08`) carry the
one-based profile in byte 2 and selector byte 4 of the armed `0xa0` read — they load that
target's working buffers and can change live mouse behavior without necessarily changing
persistent `0x0c` current metadata. Report `0x06` (polling rate) skips that loader: byte 2 is a
save alias (the deferred writer serializes the complete live DPI/preferences/buttons image into
that slot), so a `0x06` read is a live-rate read and a `0x06` write is a save-alias write.
Polling-rate reads therefore return the currently live rate regardless of the requested profile.

`profile refresh-all` is an explicit USB-only reconciliation workflow, not a
passive read. It temporarily raises the enabled maximum to five when necessary,
activates and captures each profile's DPI, preferences, buttons, and polling
rate, then restores the exact original current/maximum metadata. Fresh
observations never replace desired values; differences are reported as drift.
The workflow clears older persistence claims because it does not include a
power cycle.


### Typed writes

```bash
# DPI: six stages (example; device supports 1–8 configurable stages), active slot 2, 1 mm LOD, motion sync on
cargo run -p x3ctl -- dpi set \
    --stages 800,1600,2400,3200,5000,26000 \
    --active-stage 2 --lod one --ripple-control false --motion-sync true

# Preferences: debounce and light mode
cargo run -p x3ctl -- prefs set --debounce 8 --light-mode 2

# Bind a button (safe slot and action)
cargo run -p x3ctl -- bind set --slot forward --action profile-cycle

# Polling rate
cargo run -p x3ctl -- rate set 1000

# Activate profile 2
cargo run -p x3ctl -- profile set 2
```

### Stateless, dry-run, and JSON

```bash
# Dry run: validate and print what would be sent (no hardware or state)
cargo run -p x3ctl -- --dry-run dpi set --stages 800,1600

# JSON output for scripting
cargo run -p x3ctl -- --output json dpi get

# Stateless: in-memory state only, no durable-state readback or writeback
cargo run -p x3ctl -- --stateless bind set --slot forward --action profile-cycle

# Combine flags
cargo run -p x3ctl -- --stateless --output json --dry-run prefs set --debounce 8
```

### Portable import/export and state invalidation

```bash
# Export the current desired configuration as JSON
cargo run -p x3ctl -- export

# Import a portable configuration (writes to state, not hardware)
cargo run -p x3ctl -- import state.json

# Inspect the selected device's state entry
cargo run -p x3ctl -- state selected

# Invalidate persistence evidence while preserving desired and observed values
cargo run -p x3ctl -- state invalidate
```

### Offline debug packet generation

Build protocol packets offline through manager encoders without touching hardware:

```bash
# Generate a DPI report packet
cargo run -p x3ctl -- debug dpi --stages 800,1600,2400,3200,5000,26000 --active-stage 2

# Generate a preferences report packet
cargo run -p x3ctl -- debug prefs --debounce 8 --light-mode 2

# Generate a button table report packet
cargo run -p x3ctl -- debug buttons --slots <54-comma-separated-bytes>
```

### Explicit verification workflows

```bash
# Verify persistence by reloading the profile and comparing readback
cargo run -p x3ctl -- verify --method profile-reload

# Verify persistence by power-cycling the device and comparing readback
cargo run -p x3ctl -- verify --method power-cycle
```

### BLE transport

BLE requires compiling with `--features ble`:

```bash
# List connected BLE configuration devices
cargo run -p x3ctl --features ble -- --transport ble devices


# First write with replace-defaults (authorizes captured defaults for omitted fields)
cargo run -p x3ctl --features ble -- --transport ble --replace-defaults dpi set --active-stage 2

# Write over BLE (ACK-confirmed, no readback)
cargo run -p x3ctl --features ble -- --transport ble rate set 500
cargo run -p x3ctl --features ble -- --transport ble prefs set --debounce 8
cargo run -p x3ctl --features ble -- --transport ble --replace-defaults bind set --slot forward --action profile-cycle
cargo run -p x3ctl --features ble -- --transport ble profile set 2
```

For BLE, `--device <ID>` selects the exact stable ID reported by `devices`; omit it
only when exactly one connected FEE0 device is available. BLE commands never invoke
pairing or unpairing.

## Device identity and endpoints

Schema 4 uses a stable logical key `mouse-N` (`N >= 1`, canonical, allocated via `nextDeviceNumber` in `state.json`). No serial number or HID path is exposed as identity; `serial_number` is endpoint metadata only (trimmed, blank normalized to `None`).

- **Endpoint is a locator, not an identity.** `DeviceEndpoint { transport, vendor_id, product_id, serial_number, locator, display_name }` keeps the current openable HID path verbatim as `DeviceLocator::UsbPath(path)` (or `BlePlatformId` for BLE). The path can change on replug; the logical `mouse-N` does not.
- **Exact endpoint rediscovery is automatic.** `devices` / `list_devices` matches discovered endpoints by exact `(transport, locator)` equality and upserts the endpoint in place. No new logical device is created for a known locator.
- **Cross-transport linkage is explicit.** Discovery never auto-merges wired/receiver/BLE by VID/PID or name. Adding a second transport to the same logical mouse requires the explicit `link_devices(source, target)` operation, which only succeeds when transports do not overlap and only one side carries configuration evidence. No stable-serial or automatic-link claim is made.
- **Controlled unique replug can update the locator.** When a stored locator disappears (USB replug path change), `rebind_missing_endpoint` updates it only if exactly one connected candidate exists for that transport with the same VID/PID. Zero or multiple candidates fail with an ambiguity error — the implementation refuses to guess.
- **Ambiguity refuses to guess.** Device selection without an explicit `--device <ID>` (and without a valid selected device) returns `AmbiguousDevice` when multiple connected logical identities exist. Rebind with ambiguous candidates is rejected the same way.
- **Receiver treated as permanently paired absent contrary evidence.** PID `fa60` identifies the shared 2.4 GHz receiver, not the mouse model; a receiver endpoint is not auto-unpaired on disconnect. Removal is explicit state management, not transport disappearance.

The GUI resolves the same model: `DeviceIdentity::selected_endpoint()` prefers `preferred_transport` when present, otherwise deterministically `Wired → Receiver → BLE`. Presentation helpers never unwrap missing endpoints.

## Durable state

`x3ctl` and `x3-gui` share the same platform-local state directory. The manager stores durable device configuration in `state.json` (schemaVersion 4, sibling `state.lock` via `fs2` plus per-device `*.operation.lock` for transport I/O). Each resource keeps desired and observed values separately. Desired values record whether they came from a user write, portable import, or explicitly authorized captured defaults (`--replace-defaults`). Observations record USB readback independently. Application verification (`Acknowledged`, `ReadbackVerified`, or mismatch) is separate from persistence verification (`Unknown`, profile reload, or power cycle). Checked `serde` rejects unknown schemas and non-canonical `mouse-N` identities.

GUI-only presentation state is **not** in `state.json`. It lives in a sibling `gui-preferences.json` with its own `schemaVersion = 1` (`x3-gui/src/app_settings.rs`): coalescing atomic writes, backup on unreadable/unsupported version, no `state.lock` and no hardware access. `serde` is retained in `x3-gui` only for this file.

`state invalidate` preserves desired and observed values but clears their persistence verification. `--stateless` uses an in-memory store for the current invocation and does not read or write the durable state file.

BLE has no configuration readback path. BLE writes are ACK-confirmed through FEE4 notification (`10 50 00 <report_id>`), but ACK status `0x00` proves parser acceptance only — it does not prove application or EEPROM persistence. Over USB, `x3ctl` verifies writes through immediate readback of the affected fields. Report `0x06` (polling rate) persistence was separately verified across a power-cycle on one wired device. [live-confirmed] Polling-rate reads are live-alias only (`read_live_polling_rate(alias)` is a wire side effect); profile-scoped meaning requires a preceding `read_profile(target)` in the same session, tested via `last_polling_alias`

## Hardware safety

- Use conservative delays between configuration packets.
- Preserve current settings before experimental writes.
- Do not access BLE characteristics FFC1 or FFC2.
- Treat direct scroll remaps as unsafe.
- Never run receiver or mouse firmware updaters as part of ordinary configuration work.
- `--dry-run` validates and prints what would be sent without touching hardware.

Read [`docs/safety.md`](docs/safety.md) before hardware experiments.

## Protocol documentation

- [Documentation index](docs/README.md)
- [X3/FA61 device guide](docs/devices/x3-fa61.md)
- [USB HID transport](docs/transports/usb-hid.md)
- [BLE GATT transport](docs/transports/ble-gatt.md)
- [Report layouts](docs/protocols/README.md)
- [Evidence and corrections](docs/research/README.md)

## Hardware test suite

The historical Windows test suite `scripts/fa61-test-suite.ps1` is not included in this repository and is not currently runnable.
Its USBPcap sessions are preserved as provenance under `docs/evidence/x3-fa61/` (see `docs/evidence/x3-fa61/captures/README.md`).

Current hardware probes in `scripts/` are:

- `scripts/persistence-probe.ps1` — wired true power-cycle persistence probe. Writes a distinctive DPI stage value, waits for a configurable dwell, then performs a true power cycle (unplug → switch OFF → wait → switch ON → replug) and verifies whether the value survived. Supports repeated dwell values to gather multiple trials.
- `scripts/profile-switch-transport-probe.ps1` — profile-switch transport comparison probe. Compares profile-related reads and switches across `wired` and `receiver` transports (read-only and switch suites) with optional USBPcap capture; the switch suite requires explicit authorization.

The preserved historical capture preset exercised six DPI stages and six physical DPI-button presses as an example;
the device itself supports **1–8 configurable DPI stages**. The physical DPI button is a separate
auxiliary HID input (`03 00 10 <stage> 00`), not the stage configuration. USB configuration readback
is supported via the armed `0xa0` selector (`dpi get`/`prefs get`/`bind get`/`rate get` on `wired`/`receiver`);
historical claims of "no useful readback" are corrected. Lighting/RGB fields are present in the packet
but have no confirmed visible effect on X3/M600 hardware.

## Linux permissions

USB access normally requires udev rules:

```udev
SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa60", MODE="0660", GROUP="plugdev"
SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa61", MODE="0660", GROUP="plugdev"
```

Reload rules after installing:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Attribution and license

This fork preserves the original project history and the MIT license from
[HarukaYamamoto0](https://github.com/HarukaYamamoto0). X3/M600 protocol research and the
current fork are maintained independently at
[`int3rrobang/attack-shark-x3-driver`](https://github.com/int3rrobang/attack-shark-x3-driver).

This project is not affiliated with Attack Shark or Kysona. Use it at your own risk.
