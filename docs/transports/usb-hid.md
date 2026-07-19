# USB HID feature-report transport

USB HID is the production transport for X11 wired (`0xfa55`), X11 2.4 GHz adapter (`0xfa60`), and X3/FA61 wired (`0xfa61`). Packet bytes are model-specific and are defined under [`../protocols/`](../protocols/README.md).

## Device modes

| Driver mode | PID | Model | Status |
|:------------|:----|:------|:-------|
| `wired` | `0xfa55` | X11 wired | Supported |
| `adapter` | `0xfa60` | X11 2.4 GHz adapter | Supported |
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
(59 bytes), `0x0a` (128 bytes), and `0x0c` (10 bytes). The target byte is established
for `0x04`, `0x05`, and `0x08`; do not generalize that meaning to unrelated reports.

## Browser limitations

WebHID can enumerate the tested device interfaces, but Chromium exposes no usable feature-report declarations for the configuration collections. WebUSB cannot claim the OS-owned HID interface. See [`browser.md`](browser.md) for the browser-specific evidence and native alternatives.
