# attack-shark-x11-driver

[![npm version](https://img.shields.io/npm/v/attack-shark-x11-driver.svg)](https://www.npmjs.com/package/attack-shark-x11-driver)
[![license](https://img.shields.io/npm/l/attack-shark-x11-driver.svg)](https://github.com/HarukaYamamoto0/attack-shark-x11-driver/blob/main/LICENSE)
[![Bun](https://img.shields.io/badge/Bun-%23000000.svg?style=flat&logo=bun&logoColor=white)](https://bun.sh)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/HarukaYamamoto0/attack-shark-x11-driver)
[![codecov](https://codecov.io/gh/HarukaYamamoto0/attack-shark-x11-driver/branch/main/graph/badge.svg?token=6GURT5NZJ3)](https://codecov.io/gh/HarukaYamamoto0/attack-shark-x11-driver)

A TypeScript driver for the **Attack Shark X11 gaming mouse**, providing cross-platform support (focused on Linux) to configure DPI, macros, lighting, and polling rates via USB HID.

The official software is Windows-only; this project provides a way to interact with the device on any platform supported by Node.js or Bun.

## Features

- ✅ **DPI Configuration**: Set stages and active stage.
- ✅ **Button Remapping**: Fully customizable button behavior.
- ✅ **Macros**: Support for custom macros and templates.
- ✅ **Lighting Control**: Change modes and speeds.
- ✅ **Polling Rate**: Support for 125 Hz to 1000 Hz.
- ✅ **Battery Status**: Real-time battery monitoring.
- ✅ **Cross-platform**: Works on Linux, macOS, and Windows.

## Installation

```bash
bun add attack-shark-x11-driver
# or
npm install attack-shark-x11-driver
```

## Quick Start

```typescript
import { AttackSharkX11, ConnectionMode, Rate } from 'attack-shark-x11-driver';

const driver = new AttackSharkX11({
	connectionMode: ConnectionMode.Adapter, // or Wired
	delayMs: 300, // Recommended safe delay between packets
});

try {
	await driver.open();

	// Set Polling Rate to 1000Hz (eSports)
	await driver.setPollingRate(Rate.eSports);

	// Configure DPI Stages
	await driver.setDpi({
		dpiValues: [800, 1600, 2400, 3200, 5000, 22000],
		activeStage: 2,
	});

	// Get Battery Level
	const battery = await driver.getBatteryLevel();
	console.log(`Battery: ${battery}%`);
} catch (error) {
	console.error('Driver error:', error);
} finally {
	await driver.close();
}
```

## CLI

A basic command-line interface is included. Run it directly with Bun or after building:

```bash
# via bun (no build needed)
bun run cli --help

# after build
bun run build
node dist/cli.js --help
```

If installed globally, the `attack-shark-x11` binary is available:

```bash
attack-shark-x11 --help
```

### Global options

| Option          | Default     | Description                          |
|-----------------|-------------|--------------------------------------|
| `--mode`        | `x3-wired`  | `x3-wired` / `x3` / `wired` / `adapter` |
| `--delay-ms`    | `500`       | Delay between packets in ms          |
| `--help`, `-h`  | —           | Show help                            |

### Commands

**List devices** (no device open):
```bash
attack-shark-x11 list
```

**Verify connectivity:**
```bash
attack-shark-x11 --mode wired open
```

**Battery level** (wired modes print `-1` / unavailable):
```bash
attack-shark-x11 --mode adapter battery
```

**Factory reset:**
```bash
attack-shark-x11 reset
```

**Configure DPI:**
```bash
attack-shark-x11 set-dpi --stages 400,800,1600 --active 3
attack-shark-x11 set-dpi --stages 800,1600,2400,3200,5000,26000 --lod 2 --motion-sync on
attack-shark-x11 set-dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8
attack-shark-x11 --mode wired set-dpi --stages 800,1600,2400,3200,5000,22000 --active 2
```

X3 wired accepts 1-8 DPI stages, maxes out at 26000 DPI, and supports `--lod 1|2`, `--ripple on|off`, `--angle-snap on|off`, and `--motion-sync on|off` in the DPI packet. X11 wired/adapter modes require exactly 6 stages and max out at 22000 DPI.

**Set polling rate:**
```bash
attack-shark-x11 set-rate --rate 1000
```

**Set preferences** (packet is built from driver defaults; unspecified fields use builder defaults — current device state is not read):
```bash
attack-shark-x11 set-prefs --light breathing --speed 5 --rgb 255,0,0 --key-response 4
```

**Bind buttons:**
```bash
attack-shark-x11 bind --button forward --action shortcut-copy
attack-shark-x11 bind --list-actions
attack-shark-x11 bind --list-buttons
```
`bind` sends a full model-default mapping packet; unspecified buttons reset to model defaults, not the current device state.

On `x3-wired`/FA61, DPI binds appear ignored by stock firmware. Scroll binds are experimental/unsafe and may repeat actions indefinitely until unplug/reboot.

### Hex previews (no device open)

Build packets and print hex — useful for debugging or scripting:

```bash
# DPI packet for X3 variant (default mode), 3 stages
attack-shark-x11 hex dpi --stages 400,800,1600 --active 3

# X3 sensor flags in the DPI packet
attack-shark-x11 hex dpi --stages 800,1600,2400,3200,5000,26000 --lod 2 --ripple off --angle-snap off --motion-sync on

# DPI packet for X3 variant, all 8 stages
attack-shark-x11 hex dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8

# X11 Wired mode requires exactly 6 stages
attack-shark-x11 hex dpi --stages 800,1600,2400,3200,5000,22000 --mode wired

# Polling rate hex
attack-shark-x11 hex rate --rate 1000

# Preferences hex
attack-shark-x11 hex prefs --light static --rgb 0,255,0 --mode adapter

# Button bind hex
attack-shark-x11 hex bind --button dpi --action shortcut-swap-window --mode x3-wired

# Internal state reset hex
attack-shark-x11 hex reset --mode adapter
```

The default mode for all commands is `x3-wired`. Use `--mode` to override.

## Linux Setup (udev)

To access the device without root permissions on Linux, you need to create an udev rule:

1. Create the rule file:
    ```bash
    sudo nano /etc/udev/rules.d/99-attack-shark-x11.rules
    ```
2. Add the following lines:
    ```udev
    SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa60", MODE="0660", GROUP="plugdev"
    SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa55", MODE="0660", GROUP="plugdev"
    SUBSYSTEM=="usb", ATTR{idVendor}=="1d57", ATTR{idProduct}=="fa61", MODE="0660", GROUP="plugdev"
    ```
3. Reload rules:
    ```bash
    sudo udevadm control --reload-rules
    sudo udevadm trigger
    ```

## Supported Hardware

| Device           | Mode            | Status     |
|------------------|-----------------|------------|
| Attack Shark X11 | Wired           | Supported  |
| Attack Shark X11 | 2.4GHz wireless | Supported  |
| Attack Shark X11 | Bluetooth       | Not tested |
| FA61 / Kysona M600 / X3-family | Wired (`x3-wired`) | Partial; see [`docs/x3-fa61-quirks.md`](docs/x3-fa61-quirks.md) |

_Note: Attack Shark R1 might be compatible but hasn't been verified yet._

## Important Warnings ⚠️

- **Packet Delay**: Sending configuration packets too quickly can cause the firmware to hang. Always maintain at least a **250 ms** (500 ms recommended) delay between commands.
- **Recovery**: If the mouse stops responding, switch it to Bluetooth mode for a few seconds, then back to 2.4 GHz/Wired.

## Contributing

This project is a reverse-engineering effort. Contributions such as protocol documentation, new features, or testing with different hardware are very welcome.

- **Protocol Docs**: See `docs/` for packet analysis.
- **Tools used**: Wireshark, USBPcap.

## Support the Project

This project exists because of many hours spent reverse engineering proprietary drivers, analyzing USB HID traffic, documenting protocols, and testing hardware behavior.

Recently, I gained partial access to the official driver codebase, which revealed support for dozens of additional mouse models and many undocumented features. While this opens the door for significantly broader hardware support, understanding and documenting these protocols requires a substantial amount of time and effort.

If this project is useful to you, consider supporting its development. Financial contributions help justify spending more time on reverse engineering, protocol research, testing, documentation, and implementing support for additional devices.

Your support directly contributes to:

* Expanding support for new mouse models
* Documenting undocumented protocol features
* Improving stability and reliability
* Developing configuration and tooling utilities
* Maintaining long-term compatibility across platforms

### Sponsor

* GitHub Sponsors: https://github.com/sponsors/HarukaYamamoto0
* Ko-fi: https://ko-fi.com/harukayamamoto0

Even if you cannot contribute financially, bug reports, protocol captures, testing, and documentation improvements are greatly appreciated.

## License

MIT © [HarukaYamamoto0](https://github.com/HarukaYamamoto0)

---

_Disclaimer: This project is not affiliated with Attack Shark. Use at your own risk._
