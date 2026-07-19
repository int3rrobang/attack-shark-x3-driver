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
| BLE GATT | FEE0 service | — | Experimental tooling; production transport planned |

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

Configuration readback and explicit working-profile APIs are the next implementation
milestones. The protocol evidence and experimental readback tools are already documented
under [`docs/`](docs/README.md), but they are not yet exposed as production driver methods.

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

The CLI defaults to the FA61 wired transport:

```text
attack-shark-x3 [--transport wired|receiver] <command>
```

Examples:

```bash
# Discover FA61 and FA60 HID collections
bun run cli list

# Verify that the FA61 configuration collection can be opened
bun run cli open

# Generate a three-stage wired DPI report without opening hardware
bun run cli hex dpi --stages 400,800,1600 --active 3

# Generate the padded FA60 receiver form
bun run cli --transport receiver hex dpi --stages 400,800,1600 --active 3

# Configure X3 sensor fields
bun run cli set-dpi \
	--stages 800,1600,2400,3200,5000,26000 \
	--lod 2 \
	--ripple off \
	--angle-snap off \
	--motion-sync on

# Set polling rate
bun run cli set-rate --rate 1000

# Read receiver battery telemetry
bun run cli --transport receiver battery
```

Run `bun run cli <command> --help` for command-specific options.

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
- Do not send polling-rate report `0x06` over BLE.
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
