# attack-shark-x3-driver

A TypeScript USB HID driver and CLI for the Attack Shark X3, Kysona M600, and related X3-family mice.

This project began as a fork of
[`HarukaYamamoto0/attack-shark-x11-driver`](https://github.com/HarukaYamamoto0/attack-shark-x11-driver)
and now deliberately targets the X3/M600 protocol family. The original Git history, MIT
license, and attribution are preserved, but X11 is no longer a production target.

## Current transports

| Transport | Host identity | CLI value | Status |
|:----------|:--------------|:----------|:-------|
| USB wired | VID `1d57`, PID `fa61` | `wired` | Supported |
| 2.4 GHz receiver | VID `1d57`, PID `fa60` | `receiver` | Supported |
| BLE GATT | FEE0 service | `ble` | Connected-device discovery and typed ACK-confirmed writes; configuration readback is unavailable |

FA61 and FA60 use the same X3/M600 packet dialect. Transport selection controls device
discovery and compact versus padded feature-report lengths.

## Features

- One to eight DPI stages from 50 to 26,000 DPI in 50-DPI increments
- Lift-off distance, ripple control, angle snapping, and motion sync
- Preferences, sleep timers, debounce, and packet-resident lighting fields
- Polling rates from 125 Hz to 1000 Hz
- Button mapping and custom macro pages
- Model-correct reset sequencing
- Receiver battery telemetry
- Offline packet generation through `hex` commands

The Rust crate now exposes serialized, profile-targeted FA61 readback plus
readback-verified DPI, preferences, and complete button-table writes. The
native CLI supports discovery, reads, read-modify-write DPI/preferences updates,
safe selected-button updates, polling-rate updates, and conservative profile
activation over USB.
With `--transport ble`, the CLI sends the same validated
X3 reports over FEE3 and reports their FEE4 ACKs. Because BLE has no
configuration readback, BLE DPI, preferences, button, and profile commands
require a stored baseline and fail before writing when none is available
or authorized with `--replace-defaults`; BLE reads display cached desired
state rather than live values.
USB readback verification generally proves immediate state only; report `0x06`
persistence was separately verified across a power-cycle on one wired device. [live-confirmed]

## Development

The repository uses the Bun version recorded in `package.json`.

```bash
bun install
bun test
bun run typecheck
bun run build
```

Run the CLI directly from source:

```bash
bun run cli --help
```

The production CLI is `x3ctl`, a resource-oriented binary built from the same Rust
crate.  It is installed automatically with `cargo install --path crates/attack-shark-x3`
or run from source:

```bash
cargo run -p attack-shark-x3 --bin x3ctl -- --help
```

**Device discovery and selection**

```bash
# List discoverable devices on auto-detected transport
cargo run -p attack-shark-x3 --bin x3ctl -- devices

# Select a specific device for subsequent commands
cargo run -p attack-shark-x3 --bin x3ctl -- use '<device-path-or-name>'
```

**Inspect configuration**

```bash
# Show device, broker, and durable-state status
cargo run -p attack-shark-x3 --bin x3ctl -- status

# Read current DPI configuration for the default profile
cargo run -p attack-shark-x3 --bin x3ctl -- dpi

# Read preferences for profile 1
cargo run -p attack-shark-x3 --bin x3ctl -- -p 1 prefs

# Read the current polling rate
cargo run -p attack-shark-x3 --bin x3ctl -- rate

# Read button bindings for profile 2
cargo run -p attack-shark-x3 --bin x3ctl -- -p 2 bind
```

**Write commands**

```bash
# DPI: six stages, active slot 2, 1 mm lift-off, motion sync on
cargo run -p attack-shark-x3 --bin x3ctl -- dpi 800,1600,2400,3200,5000,26000 \
    --active 2 --lod 1 --ripple-control off --motion-sync on

# Preferences: debounce, light mode, sleep timers
cargo run -p attack-shark-x3 --bin x3ctl -- prefs --debounce 8 --light-mode static
cargo run -p attack-shark-x3 --bin x3ctl -- prefs --light-mode-raw 0x70

# Bind a button
cargo run -p attack-shark-x3 --bin x3ctl -- bind forward profile-cycle

# Polling rate (Hz)
cargo run -p attack-shark-x3 --bin x3ctl -- rate 1000

# Profile management
cargo run -p attack-shark-x3 --bin x3ctl -- profile use 2
cargo run -p attack-shark-x3 --bin x3ctl -- profile max 3
```

**Durable state: apply, export, and manage**

```bash
# Export the current device state as JSON
cargo run -p attack-shark-x3 --bin x3ctl -- export state.json

# Apply durable state (with selector, applies the full stored config)
cargo run -p attack-shark-x3 --bin x3ctl -- apply

# Apply from a previously-exported JSON file
cargo run -p attack-shark-x3 --bin x3ctl -- apply state.json

# Seed an explicit desired-state baseline (does not read or write hardware)
cargo run -p attack-shark-x3 --bin x3ctl -- state init-defaults

# Forget a device from durable state
cargo run -p attack-shark-x3 --bin x3ctl -- state forget
```

**Invocation modes**

```bash
# Dry run: validate and print what would be sent (no hardware)
cargo run -p attack-shark-x3 --bin x3ctl -- --dry-run dpi 800,1600

# JSON output for scripting
cargo run -p attack-shark-x3 --bin x3ctl -- --json status

# Direct: talk to hardware, skip the IPC broker
cargo run -p attack-shark-x3 --bin x3ctl -- --direct dpi

# No-state: skip durable-state readback and writeback
cargo run -p attack-shark-x3 --bin x3ctl -- --no-state bind forward profile-cycle

# Stateless (--direct --no-state): fire-and-forget, no broker, no state
cargo run -p attack-shark-x3 --bin x3ctl -- --stateless prefs
```

**BLE transport**

```bash
# List already-connected BLE configuration devices
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble devices

# Write config over BLE (ACK-confirmed, no configuration readback)
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble dpi --active 2
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble rate 500
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble prefs --debounce 8
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble bind forward profile-cycle
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble profile use 2

# BLE baseline: select the device, then seed explicit desired defaults
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble use M600-5.2
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble state init-defaults
cargo run -p attack-shark-x3 --bin x3ctl --features ble -- --transport ble --replace-defaults dpi --active 2
```

USB hardware commands target only the FA61 interface-2 configuration collection.
Use `--device <PATH>` when more than one matching mouse is connected.  For BLE,
`--device <NAME>` selects a connected FEE0 device by its OS-visible name; omit it
only when exactly one connected FEE0 device is available.  BLE commands never
invoke pairing or unpairing.

FA61 profile-targeted reads are not passive: reading DPI, preferences, buttons, or a
complete profile can load that target's working buffers and change live mouse behavior
without changing persistent profile metadata.

The Rust FA61 handle also exposes the capture-confirmed physical DPI-button
pulse and resulting one-based active stage through
`subscribe_dpi_button_events()`. It is an observation hook for OSD/application
integrations and does not automatically rewrite DPI state.

## Broker

`x3ctl` uses a local IPC broker that auto-starts on first invocation and persists
across CLI calls.  The broker serialises requests one at a time and maintains a
device session with idle release at 120 s.  When no client connects and no
session is active for 600 s, the broker exits.  Start-up never auto-applies
configuration — every write is user-triggered.

```bash
# Inspect or stop the broker
x3ctl daemon status
x3ctl daemon stop
```

## Durable state

`x3ctl` records every configuration change in a per-device, per-transport durable
state file stored under the platform-local data directory. Each section carries
a source tag (`usb-readback`, `explicit-defaults`, `locally-written`, or
`imported`) and a separate verification status, so the apply pipeline can
distinguish readback-verified values from synthesized defaults, local writes,
and imported exports.

Because BLE has no configuration readback path, durable state for BLE devices
lacks `usb-readback` provenance and the apply pipeline requires an explicit
baseline. `state init-defaults` seeds desired state without reading or writing
hardware. Pass `--replace-defaults` on a BLE command to authorize using that
baseline for omitted fields:

```bash
# Seed explicit defaults for a BLE device
x3ctl --transport ble state init-defaults
# Write partial config with explicit default fallback
x3ctl --transport ble --replace-defaults dpi --active 2
```

BLE writes are ACK-confirmed through FEE4 notification (`10 50 00 <report_id>`),
but ACK status `0x00` proves parser acceptance only — it does not prove
application or EEPROM persistence.  Over USB, `x3ctl` verifies writes through
immediate readback of the affected fields.  Report `0x06` (polling rate)
persistence was separately verified across a power-cycle on one wired device.
[live-confirmed]

The `--no-state` flag suppresses durable-state readback and writeback for a
single invocation, useful for one-off probes.  `--direct` bypasses the broker
and talks to hardware in-process.  `--stateless` combines both: fire-and-forget,
no broker, no state.  Without `--direct`, `--stateless`, or `--no-state`, the
broker reads the current durable-state snapshot, merges the command delta, and
writes back the updated state after every successful hardware write.

### Full test suite

The self-run Windows suite is `scripts/fa61-test-suite.ps1`. It separates offline
validation, non-mutating hardware discovery, and USBPcap captures. It follows the
physical X3/M600 limits: exactly six DPI slots, no lighting-settings panel, and no
useful configuration readback. It creates timestamped logs and artifacts under
`test-artifacts/fa61-suite` without deleting prior runs.

```powershell
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode plan
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode offline
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode hardware -Transport wired
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode hardware -Transport receiver
pwsh -NoProfile -File scripts/fa61-test-suite.ps1 -Mode capture-manual -IncludeAppControls
```

The manual capture covers motion, all five normal buttons, an isolated DPI-button
event, six sequential DPI-button presses, and the RF/wireless control. The optional
stock-app steps inspect the six DPI slots and do not look for lighting controls. No
configuration writes, targeted reads, reset, firmware, BLE, scroll remaps, or arbitrary
button-table writes are automated. The capture helper defaults to the sibling
`tshark_mouse` repository; override it with `-CaptureRepo` if necessary.


## Library usage

```typescript
import { AttackSharkX3, Rate, TransportKind } from 'attack-shark-x3-driver';

const mouse = new AttackSharkX3({
	transport: { kind: TransportKind.Wired },
	delayMs: 500,
});

try {
	await mouse.open();
	await mouse.setPollingRate(Rate.eSports);
	await mouse.setDpi({
		dpiValues: [800, 1600, 2400, 3200, 5000, 26000],
		activeStage: 2,
	});
} finally {
	await mouse.close();
}
```

Use `TransportKind.Receiver` for the FA60 2.4 GHz receiver. An exact HID path can be
supplied when multiple matching devices are present:

```typescript
const mouse = new AttackSharkX3({
	transport: {
		kind: TransportKind.Receiver,
		path: 'platform-specific-hid-path',
	},
});
```

## CLI

The production CLI is the Rust `x3ctl` binary described above.  A TypeScript
development wrapper (`bun run cli`) is also available and defaults to the FA61
wired transport:

```text
bun run cli [--transport wired|receiver] <command>
```

For the current production syntax, see the **x3ctl** examples in the Development
section above, or run:

```bash
cargo run -p attack-shark-x3 --bin x3ctl -- help
```

Run `bun run cli <command> --help` for TypeScript CLI command-specific options.
## Linux permissions

USB access normally requires udev rules for both X3 transports:

```udev
SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa60", MODE="0660", GROUP="plugdev"
SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa61", MODE="0660", GROUP="plugdev"
```

Reload the rules after installing them:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Hardware safety

- Use conservative delays between configuration packets.
- Preserve current settings before experimental writes.
- Valid BLE polling-rate report `0x06` writes are permitted through the typed protocol encoder (legacy-shaped packets were rejected). ACK proves parser acceptance; immediate effective BLE polling behavior remains unconfirmed.
- Do not access BLE characteristics FFC1 or FFC2.
- Treat direct scroll remaps as unsafe.
- Never run receiver or mouse firmware updaters as part of ordinary configuration work.

Read [`docs/safety.md`](docs/safety.md) before hardware experiments.

## Protocol documentation

- [Documentation index](docs/README.md)
- [X3/FA61 device guide](docs/devices/x3-fa61.md)
- [USB HID transport](docs/transports/usb-hid.md)
- [BLE GATT transport](docs/transports/ble-gatt.md)
- [Report layouts](docs/protocols/README.md)
- [Evidence and corrections](docs/research/README.md)

Historical X11 documentation and evidence remain model-qualified references. They are not
proof of X3 behavior and are not part of the production support promise.

## Attribution and license

This fork preserves the original project history and the MIT license from
[HarukaYamamoto0](https://github.com/HarukaYamamoto0). X3/M600 protocol research and the
current fork are maintained independently at
[`int3rrobang/attack-shark-x3-driver`](https://github.com/int3rrobang/attack-shark-x3-driver).

This project is not affiliated with Attack Shark or Kysona. Use it at your own risk.
