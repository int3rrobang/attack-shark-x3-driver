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

The production Rust driver uses native HID feature-report writes via `hidapi`. USB writes are effectively fire-and-forget from the firmware perspective: unlike X3 BLE, the transport provides no per-report firmware ACK. The manager's default `Transport` validation therefore completes on `SET_REPORT` submission success — evidence that the report was accepted by the transport, not that the firmware applied or persisted it. Readback is opt-in: `--validation readback` performs an immediate armed readback of the affected fields, and even that proves current working state only. A successful host write never proves that the firmware persisted the setting to EEPROM.

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
Increasing the receiver deadline to five seconds (about 22 seconds across four
attempts including initial delays) did not wake a sleeping mouse: the entire
expanded window exhausted, while the same command succeeded immediately after
physical movement. The production deadline therefore remains bounded at two
seconds; callers must wake the mouse and retry rather than turn sleep into a
long blocking operation. \[live-confirmed, 2026-08-04]

`read_profile(profile)` keeps metadata, DPI, preferences, and buttons inside one
queue item. Its result exposes persistent metadata separately from the explicit
working-profile target. DPI, preferences, and complete button-table writes are
also single queue commands. Under the default `Transport` validation the worker
sends the model-correct packet and completes on submission. Under
`--validation readback` it additionally holds a 500 ms wired or five-second
receiver quiet period, rearms a fresh targeted read, and requires the decoded
state to match before reporting success. This proves immediate readback only,
not persistence.

Polling-rate reads and writes use the same serialized worker. The low-level
read is `MouseHandle::read_live_polling_rate(alias)` (USB-only) — alias is a
wire side effect and content is the current live rate — and the unchecked
primitives are `MouseHandle::write_polling_rate_unchecked(profile, rate)`
(with `send_polling_rate_unchecked(profile, rate)` for the transport-only
path). Report `0x06` skips the profile loader, so a bare live-rate read
reflects the currently live profile and byte 2 is a save alias: the deferred
writer persists the complete live image into that slot. Profile-scoped meaning
therefore requires a preceding complete profile load in the same session; the
manager's safe `DeviceManager::read_polling_rate(target)` does exactly that —
`read_profile(target)` followed immediately by `read_live_polling_rate(alias)`
in one guarded session, validated, and persisted under the target profile — and
safe writes go through the manager's preflight (complete desired non-rate
image, metadata `current == target`, no-op when the rate already matches) — see
[`06-polling-rate.md`](../protocols/06-polling-rate.md). The manager's
`--validation transport` (default) submits the report without post-write
readback; `--validation readback` performs the fresh armed read and requires
the returned rate to match. The exact nine-byte report `0x06` was also accepted
over BLE in the same-hardware probe and changed the value observed after USB
reconnect. Over BLE the safe rate path fails with
`ExplicitAuthorizationRequired`; the only BLE write is the explicit
`--allow-unverified-ble-rate-write` override, whose parser ACK is the strongest
evidence BLE offers — there is no byte-for-byte readback path, so readback
validation is rejected over BLE and `read_live_polling_rate` remains USB-only.
Profile activation preserves the reported maximum, rejects idempotent or
disabled targets, writes the model-correct `0x0c` edge, holds the same
transport-specific targeted-traffic quiet period, and verifies metadata without
using a target-section selector. The same serialized worker exposes a bounded
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
`read_battery` API waits up to 20 seconds for the next autonomous push; reports
arrive roughly every 10–15 seconds while the mouse is active and discharging.
The firmware suppresses battery telemetry while charging (VBUS detected), so
the wait will time out in that state. See
[`battery.md`](../protocols/battery.md) for full details.

### Auxiliary report-`0x03` input events

The X3/FA61 wired device and FA60 receiver expose a separate interrupt-input
collection. The stock host accepts only five-byte reports with ID `0x03`:

```text
03 <event-type low> <event-type high> <event-data low> <event-data high>
```

The Rust driver decodes the little-endian event pairs once and publishes a
single `MouseHandle::subscribe_input_events()` stream. Currently recognized
events are:

| Type | Payload | Rust event |
|:-----|:--------|:-----------|
| `0x1000` | one-based stage/profile value, trailing byte `00` | active DPI stage |
| `0x2000` | profile `1..=5`, trailing byte `00` | secondary profile |
| `0x4010` | charging marker `01`, level `1..=10` | X3 battery (`level × 10`) |
| `0x4055` | legacy level `0..=100` | X11 battery |
| `0x5010` | state `0`/`1`, trailing byte `00` | connected/disconnected |
| `0x6000` | DPI index `1..=10`, trailing byte `00` | DPI index |
| `0x7000` | LED mode `0..=7`, trailing byte `00` | LED mode |
| `0x8000` | new active profile `0..=4` (zero-indexed), trailing byte `00` | profile sync |

Unknown types and malformed/out-of-range payloads are ignored. The driver
preserves all five report bytes in each decoded event. This is an
**implementation** of the static-analysis event layout; the existing DPI and
battery interpretations remain **live-confirmed** as described below. The
profile-sync (`0x8000`) payload was live-confirmed on the FA60 receiver on
2026-08-04 as the **zero-indexed new active profile**. All three physical
profile-navigation actions emit the identical format, one report per switch on
the auxiliary endpoint `0x83`:

| Binding | Presses observed | Reports (`03 00 80 <new-0-indexed> 00`) |
|:--------|:-----------------|:----------------------------------------|
| profile-cycle (`0x34`) | 2→3, 3→4, 4→5, wrap 5→1 | `02, 03, 04, 00` |
| profile-plus (`0x35`) | 1→2, 2→3, 3→4, 4→5 | `01, 02, 03, 04` |
| profile-minus (`0x36`) | 5→4, 4→3, 3→2 | `03, 02, 01` |

A full-range profile-plus capture independently observed 1→2, 2→3, 3→4, and
4→5 as `03 00 80 01 00`, `03 00 80 02 00`, `03 00 80 03 00`, and
`03 00 80 04 00`, with matching metadata after every transition. The press at
profile 5 emitted no event and left metadata at profile 5. The temporary
multi-profile binding setup used for that capture later exposed the persistent
state hazard documented under
[`0x0c`](../protocols/0c-profile-reset.md#stale-working-button-table-after-interleaved-multi-profile-writes);
the retained `profile-navigation-probe` example is consequently read-only.
\[live-confirmed, 2026-08-04]

A press that does **not** change the profile (clamp at maximum for
profile-plus, clamp at minimum for profile-minus) emits **no report** and no
LED flash. The press itself produces no button-bit change on the movement
reports. **Host-initiated** profile switches (a report-`0x0c` metadata write
via `x3ctl profile set`) change the working profile but do **not** emit the
profile-sync report — the `0x8000` report is button-event-driven, not
switch-driven; the write is acknowledged by the separate
`03 10 50 00 <report_id>` ACK family.
A controlled host-initiated profile 1→2 activation on 2026-08-04 verified
metadata `current=2` but produced no decoded `0x8000` profile-sync event during
the following two seconds; the only decoded input was the autonomous battery
report `03 10 40 01 09`. This confirms that host activation does not synthesize
the physical-button profile-sync event. Because the decoded event API rejects
the ACK-shaped `0x5010` family, this probe does not independently recapture the
raw ACK bytes.

### Profile-switch transport freeze probe

The guarded probe runner
[`scripts/profile-switch-transport-probe.ps1`](../../scripts/profile-switch-transport-probe.ps1)
compares FA60 receiver and FA61 wired behavior while the operator moves the
tested mouse continuously. It separates an idle baseline, repeated report-`0x0c`
metadata reads, repeated report-`0x0a` polling reads, an idempotent profile-1
write, raw profile 1→2→1 edges, and fully verified profile 1→2→1 edges. The
probe never reads or writes the mutable configuration sections `0x04`, `0x05`,
or `0x08`.

The script is plan-only unless `-Execute` is present. Start with its read-only
suite on each transport:

```powershell
pwsh -NoProfile -File scripts/profile-switch-transport-probe.ps1 -Transport receiver -Suite read-only
pwsh -NoProfile -File scripts/profile-switch-transport-probe.ps1 -Transport receiver -Suite read-only -Execute -Capture
pwsh -NoProfile -File scripts/profile-switch-transport-probe.ps1 -Transport wired -Suite read-only -Execute -Capture
```

Switch cases require a second, conspicuous
`-AllowProfileWrites` guard. Before capture starts, the runner verifies
`max=5` and normalizes the working profile to profile 1 if necessary. Each
measured case still independently requires `current=1, max=5`, each successful
edge case returns to profile 1, and write repetitions are capped at three
because each 1→2→1 pair may consume two metadata-sector erase cycles:

```powershell
pwsh -NoProfile -File scripts/profile-switch-transport-probe.ps1 -Transport receiver -Suite switches
pwsh -NoProfile -File scripts/profile-switch-transport-probe.ps1 -Transport receiver -Suite switches -Execute -Capture -AllowProfileWrites
```

`-Capture` uses the sibling `tshark_mouse` batch runner to capture all USBPcap
traffic, including `GET_REPORT` transfers, in one elevated session with a
separate pcap per case. It stops after a failed probe and skips automatic JSON
packet decoding because decoding an all-traffic capture is expensive; inspect
the saved pcaps directly afterward. The Rust probe emits timestamped
`PROBE_EVENT` lines live during capture. Compare movement-report timing on the
primary input endpoint against those markers and the configuration traffic.

The controlled captures under `test-artifacts/profile-switch-transport/`
(`20260806-031203`, `032221`, `033203`, and `033520`) isolate the stall to
**FA60 prepared reads**, not report-`0x0c` writes. On the receiver, all 49
analyzable readiness completions across metadata, polling-rate, idempotent,
raw-edge, and verified-edge cases coincide with a primary endpoint-`0x82`
interrupt-report gap: range 502.022–508.003 ms, median 502.972 ms. The
read-only cases reproduce it 20/20 times for metadata and 20/20 times for
polling rate. Each gap ends approximately two milliseconds after the
`a0 01 00 00 00 00 00 00` ready response, immediately before the requested
report arrives (`0c 0c ...` metadata or `06 0b ...` polling rate).

The matching FA61 wired cases do not have this half-second signature. Wired
polling-rate reads remain within 0.994–1.023 ms interrupt spacing, and the
idempotent/raw-edge reads remain within 0.997–1.003 ms. Nineteen of twenty
wired metadata reads remain at or below 9.002 ms; one 478.047 ms
operator-motion dropout spans far beyond that operation's 9.010 ms readiness
cycle and was not perceived as a device freeze.

The raw-edge timing distinguishes reads from writes. Receiver report-`0x0c`
writes return their `03 10 50 00 0c` ACK without a half-second interruption;
the two 502.98 ms gaps occur approximately ten seconds later, at the explicit
metadata verification reads. The receiver idempotent case similarly has two
gaps for its preflight and verification reads, while the verified-edge case
has five gaps for its initial, pre-write, and post-write metadata reads.
This is the validation-mode latency difference: prepared reads (any
`--validation readback` verification, including the `0x06` polling-rate
readback) temporarily suppress pointer traffic on the FA60 receiver, while
send-only transport writes do not — each readback adds the roughly 502 ms
gap, so per-write readback costs are cumulative.
\[capture-confirmed, capture-directory timestamps 2026-08-06]

### Inference and causal chain

Armed prepared reads on the FA60 receiver coincide with a ~502 ms primary
endpoint-`0x82` pointer gap: range 502.022–508.003 ms, median 502.972 ms.
The signature is absent on FA61 wired reads and absent on send-only writes,
and it reproduces 20/20 times for metadata reads and 20/20 times for
polling-rate reads.
\[capture-confirmed]

The receiver suppresses pointer forwarding while servicing the prepared-read
RF round-trip.
\[inference]

The explicit causal chain: (a) the driver sends the `0xa0` selector to arm
the read → (b) a ~502 ms window opens in which pointer reports stop,
overlapping the driver's `RECEIVER_READINESS_DELAY` (500 ms, `worker.rs`),
which covers the RF round-trip → (c) the `a0 01 00 00 00 00 00 00` ready
response arrives → (d) the requested report follows ~2 ms later and pointer
traffic resumes. The gap STARTS at arming and ENDS ~2 ms after the ready
response: arming is the onset, readiness-complete is the release; the
pointer stream does not unfreeze on arming.

`RECEIVER_READINESS_DELAY` (500 ms, read path) and
`ReadPolicy::write_delay` (5 s receiver quiet period) are driver code; the
~502 ms wire gap is hardware. The code buffers are sized to the hardware
latency, not its cause. A send-only write
(`VerificationMethod::Transport`) hits neither buffer nor the wire gap.

<a id="profile-minus-2-to-1-observation"></a>
The profile-minus (`0x36`) path from profile 2 to profile 1 is defective on the
tested X3 through its FA60 receiver (live-confirmed 2026-08-04). An initial
session produced one abnormal switch in six attempts: it emitted
`03 00 10 02 00` (event type `0x1000`, data `0x02`) with a DPI-stage-style LED
instead of the `0x8000` profile-sync and profile-style N-flash LED; the other
five presses produced no switch, report, or LED.

A later controlled probe removed the stale-working-table confounder. Profile 2
was activated, the Backward slot's persisted readback was verified as `0x36`,
and one press produced no LED, no profile event in a 30-second capture, and no
metadata change (`current=2`). After a mouse power cycle, metadata still
reported profile 2 and the same press again produced no LED, no profile event,
and no metadata change. As a same-slot control, replacing only `0x36` with
profile-cycle (`0x34`) made the next press switch 2→3, flash three times, emit
`03 00 80 02 00`, and update metadata to `current=3`. This rules out the
physical button, slot, stale working map, and receiver event path for the
reproduced failure. Earlier FA61 wired evidence confirms `0x36` for 5→4 and
its clamp at profile 1, but does not cover 2→1.

The LED tracks the event family: `0x8000` → profile N-flash, `0x1000` →
DPI-style, none → no LED.

The profile-sync payload is zero-indexed. The Rust decoder adds one before
constructing the one-based [`ProfileId`], including `00` → profile 1.

The physical DPI button is observed as:

```text
03 00 10 <active-stage> 00
```

Live profile-separated captures observed `03 00 10 02 00` and
`03 00 10 03 00`, and a targeted report-`0x04` read confirmed active stage 3
after the latter event. This is not one of the five normal mouse-button bits
and is not a feature-report configuration command.

The auxiliary collection uses usage page `0x000a`, interface 2, Col03. The
input reader is best-effort so configuration access remains usable when an OS
HID backend does not expose the collection.

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
