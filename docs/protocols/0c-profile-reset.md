# Profile load and reset (report `0x0c`)

Report `0x0c` carries persistent current/maximum profile metadata on the X3/M600 family. A current-profile change is edge-triggered through stale complement bytes; resending the already stored current profile can ACK without reloading it.

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Load/reset supported | implementation |
| X3/M600 via FA60 receiver | USB HID | Ten-byte profile control and `0xa0` readback | binary report + implementation |
| X3/FA61 wired | USB HID | Edge-triggered metadata update live-confirmed; targeted section reads corrected | live-confirmed + static-analysis |
| X3/M600 BLE | BLE FEE3 | Same shared metadata and targeted-section dispatcher | live-confirmed + static-analysis |

## X3/M600 profile framing

FA61 wired sends this six-byte feature report on interface 2:

```text
0c 0a PP ~PP AA ~AA
```

`PP` is the requested current profile and `AA` is the maximum enabled profile. Both are
one-based and both complement pairs must sum to `0xff`. Firmware analysis establishes the
meaningful range `1 <= PP <= AA <= 5`.

Examples:

| Packet | Meaning | Evidence |
|:-------|:--------|:---------|
| `0c 0a 01 fe 01 fe` | Select profile 1 with one profile enabled | implementation + static-analysis |
| `0c 0a 01 fe 05 fa` | Select profile 1 with five profiles enabled | live-confirmed metadata |
| `0c 0a 02 fd 05 fa` | Select profile 2 with five profiles enabled | live-confirmed metadata |

The FA61 `0xa0` readback sequence returns a ten-byte normalized metadata image:

```text
0c 0a 01 PP ~PP AA ~AA 00 00 00
```

The tested device returned `0c 0a 01 02 fd 05 fa 00 00 00` after the six-byte profile-2
write. This proves the metadata update, not completion of a live profile load.

The FA60 receiver returns the same ten-byte metadata image with byte 1 set to
`0x0c`:

```text
0c 0c 01 PP ~PP AA ~AA 00 00 00
```

The canonical ten-byte receiver write still uses `0x0a`. A serialized live
probe decoded `current=1, maximum=5` through the FA60 form before and after
reversible rate/DPI/preferences writes, with exact final-state equality.
\[live-confirmed]

### Profile-switch behavior

**Live-observed on the X3 via the FA60 receiver, 2026-08-04:** on a physical
profile switch (profile-cycle, profile-plus, or profile-minus button), the LED
normally used for DPI-stage indication flashes **N times in white**, where N is
the one-based profile being switched to. A press that does not change the
profile (clamp at maximum for profile-plus, clamp at minimum for
profile-minus) produces **no flash**. The switch is accompanied by the
auxiliary profile-sync input report (`03 00 80 <new-0-indexed> 00`, see
[`usb-hid.md`](../transports/usb-hid.md)); no report is emitted when the
profile does not change.
The tested FA60 firmware has one confirmed exception: profile-minus `0x36`
does not reliably execute the 2→1 transition. A controlled post-power-cycle
probe produced no LED, profile event, or metadata change, while profile-cycle
`0x34` in the same slot immediately switched 2→3 and emitted
`03 00 80 02 00`. See the
[transport evidence](../transports/usb-hid.md#profile-minus-2-to-1-observation).

### Stale working button table after interleaved multi-profile writes

**Observed on the X3 via the FA60 receiver, 2026-08-04:** after a session of
interleaved per-profile button writes (each `0x08` write and armed read
targets one profile and can replace the working button map, see above), the
mouse's *working* table diverged from every persisted readback: persisted
tables read back the written actions on all profiles, but presses of the two
recently rewritten buttons stopped having any effect while the working profile
was 2, and later switches into profile 1 reported the metadata profile 2.
Other buttons and host-initiated profile switches kept working. The failure
was not cleared by power-cycling the mouse or the dongle alone, but a full
rewrite of all five profiles' button tables (to the original actions) followed
by a mouse power cycle restored every button. \[live-confirmed] The lesson
matches the existing serialization warnings: write to a profile while it is
active when possible, and after heavy interleaved multi-profile traffic,
power-cycle the mouse before relying on button behavior.

A follow-up full-range profile-plus probe reproduced a more severe form of this
hazard. Its setup repeatedly alternated host activation, a targeted button read,
a temporary button write, a physical profile transition, and another activation
and write to restore the source table. Before that sequence, profile 1 read
active DPI stage 2 and preferences `02 01 00 00 00 ff 05 00`; afterward it
consistently read active stage 4 and preferences `70 03 a8 00 ff 00 01 04`,
despite the probe writing only report `0x08`. The altered DPI/preferences image
survived a mouse power cycle. Two attempts to restore the recorded DPI through
the typed report-`0x04` path did not verify, and subsequent reads remained
unchanged, so further writes were stopped. \[live-confirmed, 2026-08-04]

This establishes that immediate per-report readback is not sufficient recovery
proof after interleaved multi-profile traffic. Do not automate temporary
per-profile remaps by switching, writing, and switching back. Capture complete
profile backups before any such experiment when the existing user configuration
must be preserved. If replacing it is acceptable, the capture-confirmed stock
reset image is also a known-good recovery source. Use a read-only event
validator for range tests; the Rust `profile-navigation-probe` example follows
that rule.

A perceived momentary pointer freeze during switching was not reproduced in
the receiver's device→host USB stream: a 2026-08-04 capture of a switch while
the mouse was moved continuously showed no pause in the interrupt input
reports (1 ms cadence preserved) and no zero-delta reports around the switch.
\[corrected, live-confirmed] The earlier impression is attributed to the
mechanical pause required to press the bottom-mounted DPI button, or to
host-side cursor processing; it is not visible on the USB input path.

### Rust codec status

The reusable Rust crate implements the pure packet boundary for profile work:

- `ProfileMetadata::new` enforces `1 <= current <= maximum <= 5`;
- `ProfileControlReport` emits the six-byte compact write or an explicitly requested
  ten-byte padded image;
- `ProfileMetadataReport` validates the normalized wired `0x0a` or FA60
  readback `0x0c` declaration, subtype, complement pairs, profile relationship,
  and reserved bytes;
- `ReadSelector` requires an explicit `ProfileId` for DPI, preferences, and button reads;
- `ReadinessStatus` validates the one-shot `0xa0` mailbox image.

The ten-byte framing is required by the X3/M600 FA60 helper path; the six-byte
form is the wired X3 form. This is supported by the recovered helper read
lengths and the X3 dongle writer; current Rust transport selection emits the
model-correct form. The Rust driver serializes reads, profile activation, and
bounded maximum-profile updates through one worker. A maximum update reads the
persistent current profile, preserves it, rejects a lower maximum, sends the
model-correct edge, holds the transport-specific quiet period, and verifies
metadata. An unchanged maximum is a read-only no-op. Neither operation is a
general metadata write or standalone reset.

The BLE CLI also exposes `activate-profile` and `set-max-profile`. Because BLE
cannot read persistent metadata first, activation uses maximum profile `5` by
default, while maximum-profile updates use current profile `1` by default.
These commands report FEE4 parser acceptance only; they do not verify profile
application or persistence.

### Maximum-profile reduction and re-enablement — 2026-07-20

**Live-confirmed on an X3/FA61 wired USB device:** `set-max-profile --maximum N`
changes the enabled-slot limit without reinitializing the profile records above that
limit. The test began at `current_profile: 1`, `maximum_profile: 5`, and captured a
complete readback of profiles 1 through 5 (DPI, preferences, and all 18 button slots).

The bounded maximum update to one sent the compact profile-control image:

```text
0c 0a 01 fe 01 fe
```

The CLI verified `current_profile: 1`, `maximum_profile: 1`. After a physical
unplug/replug, metadata still read `current_profile: 1`, `maximum_profile: 1`.
Profile 1's settings also survived the power cycle.

Before reducing the maximum, the test wrote one reversible preference marker to each
profile, then independently read it back:

| Profile | Preference marker |
|:-------:|:------------------|
| 1 | LED speed `1` |
| 2 | Deep-sleep timeout `11` minutes |
| 3 | Normal sleep timeout `1.0` minute |
| 4 | Debounce `10` ms |
| 5 | LED speed `5` |

After the power cycle, the maximum was raised one slot at a time from 1 through 5.
Each newly enabled profile returned its marker, its original DPI state, and its
original button table. Profile 2 was additionally given a unique active-stage marker
before the reduction; that marker survived the reduction and power cycle, ruling out
copying profile 1 or reinitializing profile 2 to a default image.

Therefore, for the tested X3/FA61 wired device, `maximum_profile` acts as an
enablement limit over five persistent profile records. Lowering it hides higher
profiles but does not erase or regenerate them. The test restored the original
settings and ended at `current_profile: 1`, `maximum_profile: 5`.

The test read and preserved the opaque light-mode value (`0x70`) but did not write it,
because the current CLI enum cannot reproduce that value exactly. Button mappings were
read and verified but not written; unsafe remaps remain outside this test.

### Edge-triggered load semantics

The immediate handler writes new current/maximum values but deliberately leaves their old
complements in RAM. Deferred processing uses a now-invalid current complement as its change
edge:

1. a changed current value repairs the complement and changes pending command `0x0c` to
   `0x23`;
2. command `0x23` waits for all four persistence flags;
3. it loads the persistent current profile only when that value differs from the
   working-profile alias;
4. an idempotent resend of the same current value produces no `0x23` reload.

Reports `0x04`, `0x05`, and `0x08` provide a second load path. Their byte 2 is a one-based
target profile; armed reads carry the same target at selector byte 4. Firmware updates its
working alias and can load that target before handling the section. Therefore, a read
targeted at profile 1 can replace live buffers with profile 1 while persistent `0x0c`
metadata still reports profile 2.

Firmware analysis establishes five `0x80`-byte profile records. Targeted reports `0x04`,
`0x05`, and `0x08` operate on the selected working profile and schedule a deferred
complete-record write. Profile metadata changes, targeted section traffic, and persistence
must be serialized; an immediate ACK or metadata readback is not a completion barrier.

### Stock-host exposure

Static tracing of the deployed web bundle shows that its X3 route cannot
construct this interleaving: `deviceNo=77` uses `/dpiX3`,
`/parameterXThree`, and `/factoryIndex`; none imports the shared `0x0c`
profile builder or exposes a profile selector. The only deployed caller of
that builder is the MV6 profile UI. The legacy `X3.exe` configuration path is
write-only and has no callsites for its loaded feature-read functions, so it
also does not issue the target-changing read selectors.

FA60 stock-application captures corroborate the static route. A profile-1 DPI
change sent one compact report `0x04`, and a profile-1 button change sent one
report `0x08`; neither interval contained report `0x0c` or an `0xa0` targeted
read. The stock X3 path is therefore protected by absent cross-profile
functionality, not by a proven host-side transaction barrier.

The deployed web store's apparent ACK/retry serializer is inactive: a later
duplicate `sendMessage` property replaces it with direct `socket.send`, while
`isSending` is never set true. `WebDriver.exe` supplies fixed transport sleeps
(about 200 ms wired and 500 ms FA60), but those are not firmware persistence
barriers. Models that do expose profile controls, custom clients, and the Rust
API must provide their own serialization.

The Rust receiver policy now holds five seconds without targeted traffic after
configuration and profile-control writes before issuing readback or returning
to the caller. This matches the successful guarded-probe persistence window;
wired traffic retains the established 500 ms delay. It reduces the reproduced
race surface but remains a timing guard rather than explicit flash-completion
evidence.

The `M600-5.2` and `M600-5.4` BLE names are unrelated to these profile indices; they are
RF/host slots. See the [correction ledger](../research/corrections.md).

## Reset safety

A current-profile activation that preserves the existing maximum is distinct
from reset. The Rust API exposes only that constrained operation and holds a
500 ms wired or five-second receiver quiet period before metadata verification.
It does not claim that metadata readback proves persistence.

The production reset flow sends `0c 0a 01 fe 01 fe` before reapplying every configuration
section with target byte `01`. Preserve that complete sequence for reset operations.
Because reports `0x04`, `0x05`, and `0x08` are targeted working-profile operations, even
their read selectors can change live buffers.

For X3/FA61, a stock reset capture sends:

1. `0x0c` — profile load/reset
2. `0x04` — DPI and sensor settings
3. `0x05` — preferences
4. `0x06` — polling rate
5. `0x08` — button mapping

The capture uses approximately 500 ms between reports and sends no `0x09` pages when custom macro content is not dirty. The driver therefore skips its legacy empty custom-macro reset in `x3-wired` mode; that legacy sequence was observed breaking the FA61 back button.

The raw capture is preserved at [`../evidence/x3-fa61/reset-packets.json`](../evidence/x3-fa61/reset-packets.json).

## Corrected interpretation of the 2026 wired probes

The initial FA61 probe and subsequent X3/M600 A/B run used:

```text
a0 04 38 00 01 00 00 00
```

for every DPI observation. That final `01` explicitly targets profile 1. After a genuine
profile-2 metadata edge or physical Cycle event, the next profile-1 DPI read could
immediately load profile 1 through the targeted-report branch while metadata continued to
report profile 2. The observations were real, but the earlier “metadata-only; loader did
not run” conclusion was incorrect.

This correction explains both sessions:

- Both X3 and M600 persisted the profile-1 800 DPI fingerprint across power-cycle.
- `0c 0a 02 fd 05 fa` created profile-2 metadata.
- The probe's repeated profile-1 DPI selectors then kept or restored the profile-1 working
  image, so identical profile-1 readback did not test profile-2 loading.
- The initial physical Cycle transition from profile 1 to profile 2 was genuine; the
  following profile-1 `0x04` read moved the working image back and contaminated subsequent
  button-action observations.
- Both sessions restored their original reports and metadata successfully.

The version and baseline comparisons remain valid: both devices returned primary version
field `01 10`, while their trailing version bytes and starting configurations differed.
The profile application conclusion does not remain valid and must not be used as evidence
of a missing finalization command or shared X3/M600 loader failure.

### Correctly targeted X3/M600 rerun — 2026-07-17

**Live-confirmed:** Follow-up wired runs supplied the intended one-based target on every
`0x04`, `0x05`, and `0x08` operation. Both the X3 and M600 then:

1. loaded distinct profile-1 and profile-2 DPI images under software selection;
2. preserved each image across a power-cycle;
3. changed from profile 1 to profile 2 after one physical Forward-button press bound to
   `34 00 00`;
4. read back the expected profile-2 metadata and DPI image after that physical transition.

This confirms five-profile metadata, software profile application, and the physical Cycle
action on both tested wired mice. Only profiles 1 and 2 were assigned distinctive images;
the contents and physical cycling behavior of profiles 3 through 5 were not tested.

One M600 run aborted on a transient malformed read, then an identical rerun succeeded. A
successful rerun also captured one DPI response beginning `0c 38 02 ...` before the next
read returned the correct `04 38 02 ...`. Treat a mismatched report header or malformed
metadata complement/subtype as a failed observation and repeat the complete armed-read
transaction rather than interpreting it as profile data.

### Readback can perturb an in-progress load

**Live-confirmed on both wired mice, 2026-07-17:** low-latency polling is not a passive
way to time profile application. After a real `0x0c` edge, metadata became readable in
approximately 51–111 ms. Issuing a target-`01` DPI selector as soon as metadata reported
profile 1 could leave the previous profile-2 DPI image live indefinitely. The targeted
dispatcher updates the working alias, so it can race the deferred loader and make the
loader's metadata-versus-alias comparison report an apparent match before the intended
profile image has been loaded.

The first uncontaminated profile-1-to-profile-2 transitions produced profile-2 metadata
and DPI by approximately 97/134 ms on M600 and 98/110 ms on X3, respectively. These are
single observations, not stable latency bounds. Subsequent alternating samples were
invalidated by the observer effect and must not be used as a speed comparison.

Do not poll profile application with reports `0x04`, `0x05`, or `0x08`. After a `0x0c`
edge, allow at least the established 500 ms serialization delay without targeted section
traffic, then perform one correctly targeted, fully validated read. Restoration writes
need a longer quiet persistence window before switching away; the guarded probe now uses
five seconds and verifies recovery after power-cycle.

### Safe serialized profile sequence

To recover from “metadata says profile 2, working buffers hold profile 1” and then observe
profile 2:

1. select profile 1 to create a real metadata edge;
2. allow at least 500 ms for deferred work without issuing targeted section reads;
3. select profile 2 and again leave at least 500 ms without targeted section traffic;
4. read DPI with target `02`: `a0 04 38 00 02 00 00 00`;
5. use target `02` for subsequent reports `0x05` and `0x08` too;
6. do not return to target `01` unless intentionally loading profile 1's working image.

No separate apply/finalization command was found. Correct target selection and serialized
edge-triggered metadata changes are the required controls.

### Five-profile stock-image recovery — 2026-08-04

**Live-confirmed on X3 via the FA60 receiver:** profile 1's DPI and preferences
were recovered after the corrupted full-range probe by retargeting the
capture-confirmed X3.exe profile-1 reset images to profiles 1 through 5 and
recomputing each target packet. The same recovery also rewrote the
capture-confirmed default button table and the global 1000 Hz polling-rate
packet.

The recovery did not require a pre-probe backup because replacing every user
setting was explicitly acceptable. A backup remains necessary only when the
previous custom configuration must be preserved. Arbitrary complete,
codec-valid profile images should also work in principle, but the stock capture
minimizes uncertainty about unresolved bytes. \[inference]

The applied sequence was:

1. keep `maximum_profile` at 5;
2. create a real profile edge into profile 1, with no targeted section traffic
   during the five-second receiver quiet period;
3. for each profile 1 through 5, activate it, then write and read back DPI,
   preferences, and buttons serially, retaining the five-second quiet period
   after every write;
4. write and verify the global 1000 Hz polling rate;
5. return to profile 1;
6. power-cycle both mouse and receiver, then activate and read all five
   profiles.

Every immediate readback matched, and all five complete images plus polling
rate matched after the power cycle. The guarded
`factory-profile-recovery` example defaults to an offline packet dry-run;
hardware writes require `--apply-all-profiles`, and the final verification
(profile activation plus section readback) requires
`--verify-after-power-cycle`. \[live-confirmed]

The wakeup-state read path comes from static factory-tool analysis. Its response and model
coverage remain unconfirmed; the related write packet is documented under [`0x07`](07-wakeup-mode.md).
