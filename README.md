# attack-shark-x3-driver

A Rust CLI and library for configuring Attack Shark X3, Kysona M600, and related X3/M600-family mice.

This project began as a fork of
[`HarukaYamamoto0/attack-shark-x11-driver`](https://github.com/HarukaYamamoto0/attack-shark-x11-driver)
and now deliberately targets the X3/M600 protocol family. The original Git history, MIT
license, and attribution are preserved, but X11 is no longer a production target.

## Transports

| Transport | Host identity | CLI value | Status |
|:----------|:--------------|:----------|:-------|
| USB wired | VID `1d57`, PID `fa61` | `wired` | Supported |
| 2.4 GHz receiver | VID `1d57`, PID `fa60` | `receiver` | Supported |
| BLE GATT | FEE0 service | `ble` | Requires `--features ble`; ACK-confirmed writes only, no configuration readback |

FA61 and FA60 share the same X3/M600 packet dialect. Transport selection controls
discovery and compact versus padded feature-report lengths.

### Capability truth table

| Capability | USB (`wired`/`receiver`) | BLE (`ble`) |
|:-----------|:-------------------------|:------------|
| Device discovery | `devices` | `devices` (connected FEE0 only) |
| DPI read | `dpi get` (live readback) | Unsupported |
| DPI write | `dpi set` + readback verify | ACK-confirmed; no readback |
| Preferences read | `prefs get` (live readback) | Unsupported |
| Preferences write | `prefs set` + readback verify | ACK-confirmed; no readback |
| Button table read | `bind get` (live readback) | Unsupported |
| Button table write | `bind set` + readback verify | ACK-confirmed; no readback |
| Polling rate read | `rate get` (live readback) | Unsupported |
| Polling rate write | `rate set` + readback verify | ACK-confirmed; no readback |
| Profile read/activate | `profile get` / `profile set` | ACK-confirmed; no readback |
| Battery | `battery` (USB receiver interrupt report) | Not available |
| Durable state | Full (readback-verified provenance) | Requires `--replace-defaults` when no baseline exists |
| Persistence verification | `verify --method profile-reload`, `verify --method power-cycle` | Not available |

USB readback verification proves the device's current working state only; it does not
prove EEPROM persistence. Report `0x06` (polling rate) persistence was separately
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
│   └── x3ctl/                    # CLI frontend: Clap-based resource-oriented commands
├── scripts/fa61-test-suite.ps1   # Windows hardware test suite
└── fixtures/protocol/            # Protocol fixture JSONs
```

- **`attack-shark-x3`** — protocol codecs, USB HID and BLE GATT transport drivers,
  DPI-button event subscription.
- **`attack-shark-x3-manager`** — stateful operations: per-device durable state,
  cross-process state lock, read-modify-write pipeline, provenance tracking,
  verification workflows, and offline packet generation through `debug` commands.
- **`x3ctl`** — the CLI binary. Talks to hardware through the manager crate;
  `--stateless` keeps state only in memory for the current invocation.

All crates default to USB. BLE requires `--features ble` on each crate in the
dependency chain.

## Build and test

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

# Read battery (USB receiver only)
cargo run -p x3ctl -- battery
```

FA61 profile-targeted reads are not passive: reading DPI, preferences, buttons, or a
complete profile can load that target's working buffers and change live mouse behavior
without changing persistent profile metadata.

### Typed writes

```bash
# DPI: six stages, active slot 2, 1 mm LOD, motion sync on
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

## Durable state

`x3ctl` records per-device, per-transport state under the platform-local state
directory. Each resource keeps desired and observed values separately. Desired
values record whether they came from a user write, portable import, or explicitly
authorized captured defaults. Observations record USB readback independently.
Application verification (`Acknowledged`, `ReadbackVerified`, or mismatch) is also
separate from persistence verification (`Unknown`, profile reload, or power cycle).

`state invalidate` preserves desired and observed values but clears their persistence
verification. `--stateless` uses an in-memory store for the current invocation and
does not read or write the durable state file.

BLE has no configuration readback path. BLE writes are ACK-confirmed through FEE4
notification (`10 50 00 <report_id>`), but ACK status `0x00` proves parser acceptance
only — it does not prove application or EEPROM persistence. Over USB, `x3ctl` verifies
writes through immediate readback of the affected fields. Report `0x06` (polling rate)
persistence was separately verified across a power-cycle on one wired device.
[live-confirmed]

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

The Windows test suite separates offline validation, non-mutating hardware discovery,
and USBPcap captures:

```powershell
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode plan
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode offline
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode hardware -Transport wired
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode hardware -Transport receiver
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode capture-manual -IncludeAppControls
```

The suite follows the physical X3/M600 limits: exactly six DPI slots, no
lighting-settings panel, and no useful configuration readback. It creates timestamped
logs and artifacts under `test-artifacts/fa61-suite` without deleting prior runs.

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
