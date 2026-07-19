# Web Bluetooth Probe — Attack Shark X3 / Kysona M600

A framework-free, dependency-free localhost Web Bluetooth probe for the
Attack Shark X3 / Kysona M600 (M600-5.2 / M600-5.4) mouse.

**Read-only by default.** Loading the page performs no FEE3/configuration
writes.  Connecting subscribes to FEE4 notifications, which may write the
standard CCCD descriptor (a standard GATT operation).  No FEE3 payload write
occurs without the user passing a dual confirmation gate (checkbox + typing
`WRITE`), and known-dangerous packets are hard-blocked.

## Quick start

From the repo root, serve the folder with any static HTTP server:

```
npx serve web-bluetooth-probe
```

Or from within the folder:

```
python -m http.server 8080
```

Open the displayed URL (e.g. `http://localhost:3000`) in **Chrome** or **Edge**.
Web Bluetooth requires a [secure context][secure-context]; `localhost` satisfies
this. A `file://` URL will **not** work.

## Safe manual test sequence (first time)

1. Make sure the mouse is **normally paired and connected** to the OS.
2. Open the probe page in Chrome/Edge served from localhost.
3. Verify the status line shows "Web Bluetooth available".
4. Click **Choose device** — the browser chooser opens. The mouse may need to
   advertise on the selected RF slot (long-hold the pairing button until it
   appears). This does **not** require unpairing from the OS.
5. Select your M600 device. The page shows "Ready".
6. Click **Connect**. The page connects GATT, discovers characteristics, and
   subscribes to FEE4 notifications.
7. Click **Read FEE1** — observe the opaque 10-byte state hex.
8. Click **Read Battery** — observe the battery level.
9. **Do not write** for this initial feasibility test. Writing is optional and
   gated.

The probe attempts `navigator.bluetooth.getDevices()` as best-effort feature
detection for the **Use permitted device** button. However, tested Chromium
builds **lacked** usable `getDevices()` — the control was unavailable even
after a successful chooser grant. Page refresh **loses** the `BluetoothDevice`
reference; the browser chooser and an advertising device are generally
required each session. The `getDevices()` path is best-effort: if the API is
present and returns a known device it skips the chooser. Otherwise, click
**Choose device** to open the standard `requestDevice()` chooser.

## Browser support

| Browser  | Supported |
|----------|-----------|
| Chrome   | Yes (desktop + Android) |
| Edge     | Yes (Chromium-based) |
| Firefox  | No |
| Safari   | No |
| Opera    | Yes (Chromium-based) |

The `getDevices()` API (used by "Use permitted device") is feature-detected at
runtime; it may not be usable even in browsers that declare support. Chromium
builds tested in this investigation lacked functional `getDevices()` after
chooser grants. If **Use permitted device** finds nothing, click **Choose
device** manually.

## Architecture

- **`index.html`** — standalone page with inline CSS, dark UI.
- **`app.js`** — all Web Bluetooth logic, loaded as a deferred script.
  No frameworks, no TypeScript, no build step, no external CDN assets.

### UUIDs used

| Name           | UUID                                   | Purpose          |
|----------------|----------------------------------------|------------------|
| FEE0 service   | `0000fee0-0000-1000-8000-00805f9b34fb` | Primary service  |
| FEE1           | `0000fee1-0000-1000-8000-00805f9b34fb` | Read (state)     |
| FEE3           | `0000fee3-0000-1000-8000-00805f9b34fb` | Write (commands) |
| FEE4           | `0000fee4-0000-1000-8000-00805f9b34fb` | Notify (ACKs)    |
| Battery svc    | `0000180f-0000-1000-8000-00805f9b34fb` | Battery service  |
| Battery level  | `00002a19-0000-1000-8000-00805f9b34fb` | Battery percent  |

**FFC0/FFC1/FFC2 are never exposed or requested.**

### ACK format

FEE4 notifications carry 4-byte ACKs: `10 50 <status> <report_id>`.

Correlation is by report ID only (the ACK has no transaction ID). Writes are
serialized; a pending waiter waits for a matching RID; unrelated ACKs are
logged and ignored. This does **not** provide transaction-perfect correlation.

ACK timeout defaults to 2 seconds (configurable).

### Write safety gates

- Read-only controls are always available after connect.
- The write panel controls are shown when connected, but the **Send** button
  remains disabled until all safety gates are satisfied.
- Enabling the Send button requires:
  1. The risk-acknowledgment checkbox checked.
  2. The text `WRITE` typed into the confirmation field.
- Hard-blocked packets:
  - **Report 0x06** — the stock app explicitly skips BLE for this report.
  - **Report 0x05 with byte 3 = 0x00** — writes `LightMode.Off`, which is
    known to crash X3 firmware over BLE. Use LED mode >= 0x10 instead.
- Packet preview shows length, report ID, and hex before sending.
- Writes prefer `writeValueWithResponse` and fall back to `writeValue` with
  a visible warning.

### FEE1 semantics

FEE1 is an opaque 10-byte state blob. Bytes 0 and 9 have been observed to
change across reads and connection activity even without intervening FEE3
writes. FEE1 snapshots before/after writes are **observational only,
not write proof**.

## Notes

- No automatic reconnect loops — reconnect only on user click.
- No automatic device chooser on page load.
- No network requests beyond localhost page assets.
- All state is local; no logging to remote servers.
- This probe never requests or accesses FFC0/FFC1/FFC2.

[secure-context]: https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts
