# Browser transport viability

This page records the stable transport conclusions for browser-based configuration. Experimental chronology and exact observations remain in the dated [`2026-07 browser investigation`](../research/2026-07-browser-investigation.md).

## Compatibility

| Approach | Configuration writes | Practical status |
|:---------|:---------------------|:-----------------|
| Web Bluetooth | Yes, for supported X3 BLE reports | Session-based, one-shot configurator |
| WebHID | No | Can enumerate/open, but tested collections expose no usable configuration feature reports |
| WebUSB | No | HID interfaces remain claimed by the operating system |
| Native `node-hid` | Yes | Production wired path; suitable for Electron main process or a native sidecar |
| Native OS BLE | Expected to be viable | Experimental; persistent bonded-device access remains untested here |

## Web Bluetooth constraints

A user must select an advertising device through the browser chooser. In the tested Chromium builds, a retained device could not be recovered reliably after refresh, and reconnect failed once the mouse stopped advertising. Web Bluetooth is therefore suitable for an in-session configurator, not a persistent background driver.

Web Bluetooth writes X3 report bytes to FEE3 and receives firmware ACKs from FEE4. Report support and restrictions are defined in [`ble-gatt.md`](ble-gatt.md); packet fields remain canonical in [`../protocols/`](../protocols/README.md).

## WebHID and WebUSB

The browser chooser returned logical devices for the known PIDs, but the relevant interface-2 collections declared no feature or output reports that Chromium could use for configuration. Native HID can still issue those feature reports. WebUSB is not a safe workaround because replacing the OS HID driver would disrupt normal mouse operation.

## Product boundary

A public website can offer session-only X3 BLE configuration. A robust desktop application should keep packet builders transport-independent and use native HID for USB plus a native OS BLE adapter if persistent BLE support is required.
