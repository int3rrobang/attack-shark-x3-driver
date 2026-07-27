# USB HID feature-report transport

USB HID is the production transport for X11 wired (`0xfa55`), the shared X11/X3
2.4 GHz receiver (`0xfa60`), and X3/FA61 wired (`0xfa61`). Packet bytes are
model-specific and are defined under [`../protocols/`](../protocols/README.md).

## Device modes

| Driver mode | PID | Model | Status |
|:------------|:----|:------|:-------|
| `wired` | `0xfa55` | X11 wired | Supported |
| `adapter` | `0xfa60` | Shared 2.4 GHz receiver (X11/X3) | Supported |
| `x3-wired` | `0xfa61` | X3/FA61 wired | Partially supported |

## Feature-report framing

Configuration writes use HID `SET_REPORT` on interface 2:

| Parameter | Value |
|:----------|:------|
| `bmRequestType` | `0x21` — host-to-device, class, interface |
| `bRequest` | `0x09` — `SET_REPORT` |
| `wValue` | `0x0300 | reportId` — feature report |
| `wIndex` | `0x0002` — interface 2 |

The report ID is byte 0 of the payload. Wired and adapter payload lengths may differ for reports that add adapter padding; each report page gives the exact lengths.

## Runtime behavior

The production TypeScript driver uses native HID feature-report writes. USB writes are effectively fire-and-forget: unlike X3 BLE, the transport provides no per-report firmware ACK. A successful host write therefore does not prove that the firmware applied or persisted the setting.

Use the configured inter-packet delay for multi-report operations. Stock X3 reset captures use about 500 ms between reports; sending the reset sequence back-to-back can leave only part of the configuration applied.

## FA61 configuration readback

**Live-confirmed, corrected by static analysis on 2026-07-17:** FA61 wired
configuration readback works on interface-2 `Col04`, but a command-specific `0xa0`
selector must arm each read. A direct `GET_REPORT` without this selector fails.

For report ID `RR`, total report length `LL`, and one-based target profile `PP`, send:

```text
a0 RR LL 00 PP 00 00 00
```

Then:

1. allow the selector operation to complete;
2. read eight bytes from feature report `0xa0`;
3. require status `a0 01 00 00 00 00 00 00`;
4. read `LL` bytes from feature report `RR`.

### Rust transaction implementation

The Rust crate's default `usb` feature uses `hidapi` and supports both the
FA61 wired configuration collection and the shared FA60 receiver. On Windows
both use the confirmed `Col04` collection; elsewhere interface 2 is accepted.
Automatic opening requires exactly one match, while multi-device setups must
select an exact enumerated path.

One worker thread exclusively owns the HID handle. Every async operation enters
its queue as one command. A read sends a fresh selector, polls the validated
`0xa0` mailbox, fetches the selected report exactly once, and passes it through
the report-specific decoder. Wired reads use the bounded default readiness
policy; receiver reads use a two-second RF-tolerant readiness deadline. A
malformed status or report is discarded and rearmed, with four total attempts
by default. HID I/O errors are returned directly rather than disguised as
protocol mismatches.

`read_profile(profile)` keeps metadata, DPI, preferences, and buttons inside one
queue item. Its result exposes persistent metadata separately from the explicit
working-profile target. DPI, preferences, and complete button-table writes are
also single queue commands: the worker sends the model-correct packet, waits
500 ms, rearms a fresh targeted read, and requires the decoded state to match
before reporting success. This proves immediate readback only, not persistence.

Polling-rate reads and writes use the same serialized worker and fresh
readback verification, but are global rather than profile-targeted:
`MouseHandle::read_polling_rate()` and `MouseHandle::write_polling_rate(rate)`.
The exact nine-byte report `0x06` was also accepted over BLE in the
same-hardware probe and changed the value observed after USB reconnect; BLE is
not part of the production Rust transport.

Profile activation preserves the reported maximum profile, rejects idempotent or
disabled targets, writes the model-correct `0x0c` edge, holds a 500 ms
targeted-traffic quiet period, and verifies metadata without using a
target-section selector. The same serialized worker exposes a bounded
maximum-profile update: it preserves the current profile, rejects a lower
maximum, treats an unchanged maximum as a no-op, and verifies the resulting
metadata after the same quiet period. Neither operation proves persistence.
Fake-transport tests cover write bytes, fresh readback, mismatches, profile policy,
successful profile-2 reads, malformed-target retry, bounded exhaustion,
transport failure, and polling-rate read/write verification. The native Rust
path has been independently exercised against hardware for report `0x06`,
including power-cycle persistence; the remaining operations are qualified
separately. \[implementation + live-confirmed]

### X3 through the FA60 receiver

The X3 2.4 GHz receiver enumerates as VID `0x1d57`, PID `0xfa60`. That PID
identifies the shared receiver, not the paired mouse model; the receiver's
application reports use the X3/M600 packet dialect documented on the protocol
pages.

The native Rust driver selects this path with `UsbDeviceKind::Receiver` (or the
CLI's `--transport receiver`). It opens the receiver's interface-2 configuration
collection, uses the wireless report lengths, and preserves the X3 `0xa0`
selector/readiness transaction:

```text
SET_FEATURE  a0 RR LL 00 PP 00 00 00
GET_FEATURE  a0 00 00 00 00 00 00 00
GET_FEATURE  RR ...
```

`PP` is the one-based target profile for reports `0x04`, `0x05`, and `0x08`.
The receiver forwards ordinary configuration writes to the mouse; a successful
write or immediate readback proves acceptance/live state, not deferred flash
persistence. Persistence requires the profile switch-away/switch-back barrier
described in [`0x0c`](../protocols/0c-profile-reset.md).

The receiver's prepared readbacks keep the helper/WebSocket envelope length in
report byte 1, while writes use the canonical report declaration. The live
receiver probe confirmed:

| Report | Write byte 1 | FA60 readback byte 1 | HID report bytes |
|:-------|:-------------|:----------------------|:-----------------|
| `0x04` | `0x38` | `0x3a` | 56 |
| `0x05` | `0x0f` | `0x11` | 15 |
| `0x06` | `0x09` | `0x0b` | 9 |
| `0x08` | `0x3b` | `0x3d` | 59 |
| `0x0b` | `0x08` | `0x0c` | 10 |
| `0x0c` | `0x0a` | `0x0c` | 10 |

These values match the recovered WebDriver serializer and firmware prepared-
report builder. They are declarations inside the report, not extra HID bytes.
The Rust transport decodes the FA60 readback dialect but continues to emit the
canonical write declarations. \[live-confirmed + static-analysis]

The X3/M600 FA60 receiver battery report is `03 10 40 01 <level>` where level
is 1–10 (multiply by 10 for percentage). Confirmed by X3.exe disassembly
(×10 conversion at `0x41346d–0x413470`) and live capture (2026-07-24:
`03 10 40 01 0a` = 100%). The Rust decoder accepts both this X3 signature and
the legacy X11 signature (`03 55 40 01 <pct>`, 0–100 directly). The
`read_battery` API waits for the next autonomous push; reports arrive roughly
every 10–15 seconds while the mouse is active and discharging. The firmware
suppresses battery telemetry while charging (VBUS detected), so the wait will
time out in that state. See [`battery.md`](../protocols/battery.md) for full
details. \[disassembly + live-confirmed 2026-07-24]

### FA61 auxiliary DPI-button input

The wired X3/FA61 also emits a separate interrupt-input event when the
physical DPI button is pressed:

```text
endpoint 0x83 IN
03 00 10 <active-stage> 00
```

The fixed prefix is `03 00 10`; byte 3 is the resulting one-based active
DPI-stage index. Live profile-separated captures observed `03 00 10 02 00`
and `03 00 10 03 00`, and a targeted report-`0x04` read confirmed active
stage 3 after the latter event. This is not one of the five normal
mouse-button bits and is not a feature-report configuration command.

The Rust driver opens the auxiliary HID collection (usage_page `0x000a`,
interface 2, Col03) on a separate reader thread and exposes the raw report
plus decoded stage through `MouseHandle::subscribe_dpi_button_events()`.
This collection carries both DPI button events and battery reports (receiver
only). The input reader works on both wired (FA61) and receiver (FA60)
transports and is best-effort so configuration access remains usable when an
OS HID backend does not expose the auxiliary collection.

### One-shot readiness mailbox

**Live-confirmed on the wired M600, 2026-07-17:** the readiness flag does reset. Across
30 repeated DPI transactions, `GET_REPORT 0xa0` returned `a0 00 ...` before every
selector and `a0 01 ...` after every selector. Repeated status reads left it at `01`;
fetching the selected report changed it to `00`. A second report fetch without rearming
failed. The mailbox is therefore one-shot rather than a persistent report cache.

Readiness does not guarantee that every published field matches the selector. One of those
30 transactions requested profile 1 but returned a structurally DPI-like report beginning
`04 38 02 ...`. Earlier alternating `0x0c`/`0x04` stress runs also produced reports whose
first byte belonged to the preceding report. Firmware analysis shows that the selector
task and report producer communicate through shared SRAM and a single ready byte, while
the USB callback copies the prepared buffer. The combined evidence supports a publication
or selector-latching race rather than a stale ready flag.

A host must validate the returned report ID, fixed fields, target/subtype, complements,
and checksum as applicable. On failure, discard the entire observation and rearm the
selector; a second unarmed fetch is not available.

Back-to-back selector writes demonstrate that this is not a FIFO. In 30 M600 trials per
order with no intentional gap:

| Order | First report won | Second report won | Hybrid / failed |
|:------|-----------------:|------------------:|----------------:|
| `0x04` then `0x0c` | 10 | 17 | 3 |
| `0x0c` then `0x04` | 6 | 18 | 6 |

Hybrid prefixes included `0c 0a 02 00 00 3f ...` (metadata header with DPI body) and
`04 38 02 01 fe 01 fe ...` (DPI header with metadata body). Both selector writes returned
success at the USB API. Selection fields are therefore overwritten or consumed
independently rather than queued atomically. An expanded zero-gap matrix eventually left
Windows HID enumeration blocked until the mouse was replugged; do not use back-to-back
selectors in production or high-count probes.

An expanded zero-gap cross-report matrix (0x0b/0x0c, 0x0c/0x04, 0x04/0x08) eventually
left Windows HID enumeration blocked until the mouse was replugged. The stress test must
not be re-run on production hardware.

For reports `0x04`, `0x05`, and `0x08`, `PP` is not neutral metadata. Firmware stores
`PP - 1` as its working-profile alias and can immediately load that profile before
returning or processing the section. A profile-1 DPI read is:

```text
a0 04 38 00 01 00 00 00
```

The corresponding profile-2 read is:

```text
a0 04 38 00 02 00 00 00
```

Using `01` after persistent metadata has moved to profile 2 can load profile 1's working
image again while metadata continues to report profile 2. The same target rule applies
to preferences (`0x05`) and buttons (`0x08`). Reads must therefore be serialized with
profile changes and must always carry the intended target profile.

This sequence returned live FA61 reports `0x04` (56 bytes), `0x05` (15 bytes), `0x08`
(59 bytes), `0x0a` (128 bytes), and `0x0c` (10 bytes). Report `0x0a` is readable
but is a stub: its content is report ID, declared length, current profile number,
and 125 zero bytes. It does not carry DPI, preferences, or button data.
\[live-confirmed, 2026-07-24 write-verified diff probe] The target byte is established
for `0x04`, `0x05`, and `0x08`; do not generalize that meaning to unrelated reports.

## Browser limitations

WebHID can enumerate the tested device interfaces, but Chromium exposes no usable feature-report declarations for the configuration collections. WebUSB cannot claim the OS-owned HID interface. See [`browser.md`](browser.md) for the browser-specific evidence and native alternatives.
