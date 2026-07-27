# Transport reference

Packet builders and transports are separate concerns: report documents define the model-specific byte dialect, while these pages define how those bytes reach the device.

- [`usb-hid.md`](usb-hid.md) — production USB HID feature-report transport for X11 and X3/FA61.
- [`ble-gatt.md`](ble-gatt.md) — X3/M600 GATT service, typed writes, and ACK behavior; configuration readback is unavailable.
- [`browser.md`](browser.md) — Web Bluetooth, WebHID, and WebUSB feasibility.

Transport compatibility does not imply model compatibility. Always select the packet dialect first, then use a transport that supports that report.
