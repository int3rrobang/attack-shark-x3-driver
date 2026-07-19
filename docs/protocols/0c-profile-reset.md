# Profile load and reset (report `0x0c`)

Report `0x0c` carries persistent current/maximum profile metadata on the X3/M600 family. A current-profile change is edge-triggered through stale complement bytes; resending the already stored current profile can ACK without reloading it.

## Compatibility

| Variant | Transport | Status | Evidence |
|:--------|:----------|:-------|:---------|
| X11 wired / adapter | USB HID | Load/reset supported | implementation |
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

### Rust codec status

The reusable Rust crate implements the pure packet boundary for profile work:

- `ProfileMetadata::new` enforces `1 <= current <= maximum <= 5`;
- `ProfileControlReport` emits the six-byte compact write or an explicitly requested
  ten-byte padded image;
- `ProfileMetadataReport` validates the normalized readback header, subtype, complement
  pairs, profile relationship, and reserved bytes;
- `ReadSelector` requires an explicit `ProfileId` for DPI, preferences, and button reads;
- `ReadinessStatus` validates the one-shot `0xa0` mailbox image.

The padded framing option is a wire-shape codec, not a claim of live-confirmed FA60
behavior. Device opening, selector/read serialization, bounded retries, and edge-triggered
switch sequencing remain transport-layer work; the codec does not expose an unsafe
standalone reset operation.

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

The `M600-5.2` and `M600-5.4` BLE names are unrelated to these profile indices; they are
RF/host slots. See the [correction ledger](../research/corrections.md).

## Reset safety

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

The wakeup-state read path comes from static factory-tool analysis. Its response and model
coverage remain unconfirmed; the related write packet is documented under [`0x07`](07-wakeup-mode.md).
