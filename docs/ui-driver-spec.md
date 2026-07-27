# Rust FA61 driver UI specification

Status: prototype integration contract for a user-facing UI.

This document describes the current Rust USB driver as a UI-facing capability
surface. It separates behavior that is implemented today from UI policy that an
adapter should enforce. It targets X3/FA61 wired devices; the Rust implementation
does not implement BLE or older X11 transports, but Section 10 records the
transport-specific capabilities a future adapter must honor.

Canonical packet layouts remain in [`protocols/`](protocols/README.md). Device and
transport constraints are in [`devices/x3-fa61.md`](devices/x3-fa61.md) and
[`transports/usb-hid.md`](transports/usb-hid.md).

## 1. Scope and support boundary

### 1.1 Supported connection

The Rust hardware driver supports the FA61 USB configuration collection:

- USB vendor ID: `0x1d57`
- USB product ID: `0xfa61`
- interface: `2`
- Windows collection: `Col04`
- transport: wired USB HID only

The driver must not silently select another HID collection. Automatic selection
is valid only when exactly one matching configuration collection exists. A UI that
supports multiple mice must display the discovered device path and pass the exact
path when opening a device.

BLE, firmware update operations, custom macro editing, battery over wired USB,
and X11 `0xfa55`/`0xfa60` behavior are outside this contract.

### 1.2 Design principles

1. Every profile section read and write has an explicit one-based profile target.
2. Every profile-section write is read-modify-write and must preserve fields the user did not edit; global polling-rate writes replace one explicit global value.
3. A write is not successful until the driver validates a fresh readback.
4. Immediate readback proves the device returned the requested state; it does not
   by itself prove persistence across power loss.
5. Profile metadata and target-section data are separate observations. The UI must
   not infer the live working profile from metadata alone.
6. Unknown bytes are preserved, not relabeled or discarded.
7. Operations on one device are serialized. The UI must not issue concurrent
   target reads or writes to try to improve latency.

## 2. Device session model

### 2.1 Discovery

```text
listDevices() -> DeviceInfo[]
```

`DeviceInfo` contains:

```text
{
  path: string,
  vendorId: 0x1d57,
  productId: 0xfa61,
  interfaceNumber: 2,
  product?: string,
  serialNumber?: string
}
```

The UI should show:

- no device: `No compatible FA61 configuration collection found`;
- one device: allow open by default;
- multiple devices: require an explicit device selection.

### 2.2 Open session

```text
open(devicePath?: string) -> MouseSession
close(session)
```

A session owns one exclusive HID worker. Cloning a Rust `MouseHandle` is allowed,
but all clones still share the same serialized worker. A UI adapter should expose
one session object per selected device and should close it when the device is
removed or the application shuts down.

The driver has no event stream. The UI must refresh state after opening, after a
write, after profile activation, and after reconnecting a device.

## 3. State model

The following is a language-neutral representation. Rust uses strongly typed
`ProfileId`, `StageIndex`, `DpiValue`, and enum types rather than these primitive
ranges.

### 3.1 Profile metadata

```text
ProfileMetadata {
  currentProfile: 1..=5,
  maximumProfile: 1..=5
}
```

Invariants:

```text
1 <= currentProfile <= maximumProfile <= 5
```

`maximumProfile` is the highest enabled profile slot. It is not a profile reset
and does not create a new profile record. Live-confirmed FA61 behavior shows that
reducing the maximum hides higher slots while their stored DPI, preferences, and
button records remain available when the maximum is raised again.

### 3.2 Complete profile snapshot

```text
ProfileSnapshot {
  persistentMetadata: ProfileMetadata,
  targetProfile: 1..=5,
  dpi: DpiState,
  preferences: PreferencesState,
  buttons: ButtonsState
}
```

`persistentMetadata` reports the persistent `0x0c` image. `targetProfile` reports
the profile explicitly requested for the section reads. These values can differ
from the profile currently loaded into the device's live working buffers because
targeted reads can load a working profile without changing persistent metadata.

### 3.3 DPI state

```text
DpiState {
  profile: 1..=5,
  stages: DpiValue[1..=8],
  activeStage: 1..=stages.length,
  sensor: {
    liftOffDistance: oneMillimeter | twoMillimeters,
    rippleControl: boolean,
    angleSnap: boolean,
    motionSync: boolean
  },
  preservedTail: opaque byte array
}
```

Validation:

- each DPI value is `50..=26000` in steps of `50`;
- one to eight stages are allowed;
- `activeStage` must refer to a configured stage;
- `preservedTail` is not user-editable and must survive read-modify-write.

The preserved tail has unresolved semantics. The UI may display it in a diagnostic
view, but must not offer arbitrary editing.

### 3.4 Preferences state

```text
PreferencesState {
  profile: 1..=5,
  lightModeRaw: u8,
  configurationRaw: u8,
  deepSleepRaw: u8,
  hostColorRaw: [u8; 3],
  sleepTimerRaw: u8,
  debounceRaw: u8
}
```

The UI may expose decoded views while retaining the raw values in its model:

| UI field | Valid user values | Wire representation |
|:---------|:------------------|:---------------------|
| Light mode | known modes or an unknown/raw value | known values include `0x00`, `0x10` … `0x60`; raw `0x00..=0xff` is accepted by the CLI |
| LED speed | `1..=5` | low nibble of `configurationRaw` is `6 - speed` |
| Deep sleep | `1..=60` minutes | `deepSleepRaw` plus the high configuration bucket |
| Normal sleep | `0.5..=30` minutes in `0.5` steps | `sleepTimerRaw = minutes * 2` |
| Debounce | even `4..=50` ms | `debounceRaw = ((ms - 4) / 2) + 2` |
| Host color | raw bytes only on X3 | preserve `[r, g, b]` exactly |

The host-labeled color bytes have no confirmed X3 hardware effect. They should be
shown as diagnostic/raw data or kept hidden, not presented as confirmed RGB
control. Unknown light-mode values must be displayed as `Unknown (0xNN)` and
preserved unless the user explicitly selects a replacement.

Known CLI light-mode labels are `off`, `static`, `breathing`, `neon`,
`color-breathing`, `static-dpi`, and `breathing-dpi`. The X3 BLE warning about
light mode `0x00` does not apply to this wired USB transport, but a UI should still
avoid presenting unverified hardware effects as fact.

### 3.5 Button state

```text
ButtonsState {
  profile: 1..=5,
  slots: ButtonAssignment[18]
}

ButtonAssignment {
  action: u8,
  modifier: u8,
  keyCode: u8
}
```

The raw action, modifier, and key-code fields are intentionally preserved because
not every firmware value has a complete semantic decode.

The current high-level CLI exposes these physical controls:

| UI control | Raw table slot | Supported actions |
|:-----------|:---------------|:------------------|
| Left | 1 | disable, left click, right click, middle click, forward, backward, double click, profile cycle/plus/minus |
| Right | 2 | same action set |
| Middle | 3 | same action set |
| Forward | 7 | same action set |
| Backward | 8 | same action set |

The remaining slots must be displayed as raw/unknown or read-only until their
semantics are established. A UI must not offer arbitrary raw remaps, macro
actions, or experimental repeating actions.

### 3.6 Global polling rate

```text
PollingRate {
  hz: 125 | 250 | 500 | 1000,
  code: 0x08 | 0x04 | 0x02 | 0x01
}
```

Polling rate is global, not profile-specific. It must not be displayed as a field
inside a profile editor.

## 4. Operation contract

The following operations map directly to the current Rust `MouseHandle` API. A
CLI/IPC adapter may expose different names, but must preserve these semantics.

| Operation | Scope | Result | Targeted read side effect |
|:----------|:------|:-------|:---------------------------|
| `readProfileMetadata()` | global | metadata | no section target |
| `readProfile(profile)` | profile | metadata + DPI + preferences + buttons | may load `profile` working buffers |
| `readDpi(profile)` | profile | validated `DpiState` | may load `profile` |
| `readPreferences(profile)` | profile | validated `PreferencesState` | may load `profile` |
| `readButtons(profile)` | profile | validated `ButtonsState` | may load `profile` |
| `readPollingRate()` | global | validated rate | no profile target |
| `writeDpi(state)` | profile | fresh verified `DpiState` | readback targets `state.profile` |
| `writePreferences(state)` | profile | fresh verified `PreferencesState` | readback targets `state.profile` |
| `writeButtons(state)` | profile | fresh verified `ButtonsState` | readback targets `state.profile` |
| `writePollingRate(rate)` | global | fresh verified rate | no profile target |
| `activateProfile(profile)` | persistent metadata | fresh verified metadata | deliberately avoids target-section traffic during its quiet period |
| `setMaximumProfile(maximum)` | persistent metadata | fresh verified metadata | preserves current profile |

### 4.1 Read-modify-write requirement

A UI editor must load the complete current section before constructing a write.
It must patch only fields changed by the user and pass the full preserved state to
the driver. The driver already follows this pattern for DPI, preferences, and
button writes.

The UI must not construct a complete profile from factory/default constants when
editing an existing profile. In particular, it must preserve:

- DPI `preservedTail`;
- preference `lightModeRaw`, `configurationRaw`, `deepSleepRaw`, host-color bytes,
  sleep timer, and debounce fields not edited;
- every button slot not edited;
- unknown action/modifier/key-code values.

### 4.2 Profile activation

```text
activateProfile(target)
```

Preconditions:

- `target` is `1..=maximumProfile`;
- `target` is not already `currentProfile`.

The driver preserves the current maximum, writes the edge-triggered compact
`0x0c` control, waits 500 ms without targeted section traffic, and verifies
metadata. The UI should avoid calling this operation when the target is already
current; it should treat the current profile as selected without sending a
redundant activation.

The returned metadata is an immediate verification. The UI should offer a
separate reconnect/persistence verification status rather than claiming that
activation alone proves persistence.

### 4.3 Maximum-profile update

```text
setMaximumProfile(maximum)
```

Preconditions:

- `maximum` is `1..=5`;
- `maximum >= currentProfile`.

A request below the current profile is rejected. An unchanged maximum is a
read-only no-op. A lower maximum that still contains the current profile is a
bounded metadata operation, not a reset. Because it hides higher profile slots,
the UI should require explicit confirmation and explain that those slots will not
be selectable until re-enabled.

The current implementation serializes the write, waits 500 ms, and verifies
metadata. The UI must not send a raw standalone `0x0c` packet or expose a generic
profile-metadata editor.

### 4.4 Button writes

The Rust library can write a complete validated 18-slot table. The current CLI
provides a safer single-button operation: it reads the complete target table,
changes one recognized physical button assignment, writes the complete table, and
requires a matching fresh readback.

The UI should use the single-button abstraction for ordinary editing. It should
show a confirmation for changes to physical buttons and keep raw table editing
behind a diagnostics/developer capability, if exposed at all.

### 4.5 Proposed UI adapter interface

An IPC, FFI, or process adapter may expose the following shape to the UI. The
adapter must implement the transaction rules in this document rather than
forwarding arbitrary HID packets:

```ts
type Immediate<T> = {
  value: T;
  verification: "immediate-readback";
  persistence: "unknown";
};

interface Fa61Session {
  readMetadata(): Promise<ProfileMetadata>;
  readPollingRate(): Promise<PollingRate>;
  readProfile(profile: ProfileId): Promise<ProfileSnapshot>;
  readDpi(profile: ProfileId): Promise<DpiState>;
  readPreferences(profile: ProfileId): Promise<PreferencesState>;
  readButtons(profile: ProfileId): Promise<ButtonsState>;

  writeDpi(state: DpiState): Promise<Immediate<DpiState>>;
  writePreferences(state: PreferencesState): Promise<Immediate<PreferencesState>>;
  writeButtons(state: ButtonsState): Promise<Immediate<ButtonsState>>;
  writePollingRate(rate: PollingRate): Promise<Immediate<PollingRate>>;

  activateProfile(profile: ProfileId): Promise<Immediate<ProfileMetadata>>;
  setMaximumProfile(maximum: ProfileId): Promise<Immediate<ProfileMetadata>>;
  close(): Promise<void>;
}
```

For ordinary UI editing, add patch helpers such as
`updateDpi(profile, patch)` and `updatePreferences(profile, patch)`. These helpers
must read the current section, apply the patch, write the complete state, and
return the verified readback. They must not synthesize missing fields from
hard-coded defaults.

## 5. Transaction and readback rules

### 5.1 Serialization

One worker thread exclusively owns the HID handle. The worker serializes every
read, write, activation, maximum update, and polling-rate operation. A UI must
queue operations through the session rather than issuing direct HID requests.

Do not use `Promise.all` or equivalent parallelism for reads on the same device.
Serialization is required for correctness, not only for rate limiting.

### 5.2 FA61 read transaction

For a targeted section read, the driver:

1. sends a command-specific `0xa0` selector with the explicit target profile;
2. polls the readiness mailbox for at most 250 ms by default;
3. requires a valid ready status;
4. fetches the selected feature report once;
5. validates report ID, length, declared fields, target profile, fixed bytes,
   complements, and checksum;
6. rearms and retries malformed observations up to four total attempts by default.

A transport I/O error is returned directly. A malformed response is discarded as
an invalid observation; it must not be shown as valid profile data.

### 5.3 Write transaction

For profile-scoped writes, the driver:

1. sends the model-correct report;
2. waits 500 ms by default;
3. performs one fresh targeted readback;
4. compares the decoded state to the requested state;
5. returns success only when they match.

Polling-rate writes use the same serialized write/readback pattern but are global.

### 5.4 Targeted-read warning

Reports `0x04`, `0x05`, and `0x08` carry an explicit profile target. Reading one of
these sections can load that profile into live working buffers without changing
persistent `0x0c` metadata.

The UI should:

- show the target profile in every section-read result;
- never label a target read as proof that the persistent current profile changed;
- avoid rapid polling of targeted sections after activation;
- allow the driver's 500 ms quiet period to complete before the first target read;
- invalidate cached working-section data after activation, reconnect, or another
  targeted read to a different profile.

A complete `readProfile(profile)` is the preferred refresh operation when the UI
needs a coherent profile view. It remains one serialized worker command, but the
result still reports persistent metadata separately from the target profile.

## 6. UI verification and persistence states

The UI should distinguish these states:

```text
unloaded
reading
loaded
dirty
writing
verified-immediate
persistence-unknown
persistence-verified
error
```

Recommended write flow:

1. load and display the current validated state;
2. mark edited fields dirty;
3. on Save, send one read-modify-write operation;
4. replace the optimistic UI state with the driver's verified readback;
5. show `Applied; persistence not independently verified`;
6. after a deliberate reconnect/power-cycle check, change the status to
   `persistence-verified` only if the expected state is read back again.

The UI must not claim persistence solely from a USB API success, an ACK, or an
immediate metadata readback.

## 7. Error mapping

The adapter should preserve structured errors and provide an actionable message:

| Driver condition | UI behavior |
|:-----------------|:-------------|
| `DeviceNotFound` | show disconnected state; offer refresh |
| `AmbiguousDevice` | require explicit device selection |
| HID transport error | show connection/permission failure; stop writes |
| readiness timeout or exhausted read attempts | show read failure; offer a fresh read, not stale data |
| malformed report/checksum/profile mismatch | discard observation and report validation failure |
| `ProfileAlreadyActive` | avoid by checking metadata; treat as a non-changing selection |
| `ProfileNotEnabled` | disable the profile in the selector and refresh metadata |
| `MaximumProfileBelowCurrent` | require selecting a lower current profile first |
| `WriteVerificationMismatch` | mark section as uncertain and require a fresh full read |
| `GlobalWriteVerificationMismatch` | mark the global setting uncertain and require a fresh global read |
| protocol range error | reject locally before any HID write |
| worker unavailable | close the session and require reopen |

After a write verification mismatch, the UI must not silently retry the write.
It should perform a fresh read if the transport remains available and show the
observed state to the user.

## 8. Recommended UI workflows

### 8.1 Startup

1. Enumerate FA61 configuration collections.
2. Let the user select a device if more than one is found.
3. Open one session.
4. Read profile metadata and global polling rate.
5. Read the current enabled profile with `readProfile(currentProfile)`.
6. Populate profile tabs `1..=maximumProfile`; show higher slots as disabled.

### 8.2 Profile selection

1. If the requested profile is already current, do not call activation.
2. Otherwise call `activateProfile(target)`.
3. Wait for the returned metadata verification.
4. Read the target profile once with `readProfile(target)`.
5. Display the target snapshot and keep persistent metadata separate in the model.

### 8.3 Editing a profile

1. Use the selected profile's cached snapshot only while it is still valid.
2. If stale, refresh the complete profile.
3. Apply a patch to one section in memory.
4. Send the complete read-modify-write state for that section.
5. Replace the section with the verified readback.
6. Keep unrelated sections and opaque bytes unchanged.

### 8.4 Changing the maximum profile

1. Read metadata.
2. If reducing the maximum, require confirmation that higher slots will be hidden.
3. If the current profile would become disabled, first activate a profile at or
   below the requested maximum.
4. Call `setMaximumProfile(maximum)`.
5. Refresh metadata and rebuild the profile selector.
6. Do not delete cached higher-profile data solely because it is currently hidden.

### 8.5 Reconnect/persistence verification

1. Close the HID session.
2. Reopen the selected exact device path after it reconnects.
3. Read metadata and the expected profile sections.
4. Compare against the last verified state.
5. Set `persistence-verified` only for fields that match.

## 9. Capability matrix

| Capability | Current Rust USB driver | UI exposure recommendation |
|:-----------|:-------------------------|:----------------------------|
| Discover FA61 collections | supported | required |
| Profile metadata read | supported | required |
| Profile activation | supported | required |
| Maximum-profile update | supported and bounded | required with confirmation |
| DPI read/write | supported per profile | required |
| Preferences read/write | supported per profile | required; preserve raw fields |
| Button table read | supported per profile | required for display/backup |
| Recognized button assignment write | supported by CLI | expose recognized physical controls |
| Arbitrary raw button editing | library-level only | diagnostics only, preferably hidden |
| Polling-rate read/write | supported globally | required |
| Battery telemetry on FA61 wired USB | unavailable; the wired path is silent | show `Unavailable on wired USB`; never poll and never display `0%` |
| Custom macros | not exposed | disabled |
| BLE transport | not production-supported here | disabled |
| Factory reset | not exposed as a standalone UI operation | disabled |
| Firmware update | unsupported and unsafe | never expose |

## 10. Cross-transport capability matrix

The current Rust UI contract is USB-only. A future BLE adapter must expose a
different capability set instead of reusing USB readback and persistence claims.
The canonical BLE and battery details are in
[`transports/ble-gatt.md`](transports/ble-gatt.md) and
[`protocols/battery.md`](protocols/battery.md).

| Capability | FA61 wired USB | X3/M600 BLE GATT | UI contract |
|:-----------|:---------------|:-----------------|:------------|
| Configuration writes | `SET_REPORT`; no transport ACK | FEE3 write plus FEE4 ACK notification | USB success requires validated readback; BLE success means ACK acceptance only |
| Configuration readback | Validated `0xa0` selector plus report read | Not available; FEE1 is opaque and is not configuration state | Never offer BLE `readDpi`, `readPreferences`, `readButtons`, or `readProfile` as if they were implemented |
| DPI/preferences/buttons writes | Read-modify-write with fresh immediate readback | Reports may receive ACK `status=0x00`, but there is no configuration readback | BLE UI must show `Accepted by device; application/persistence unverified` |
| Profile metadata (`0x0c`) | Bounded read/write with immediate metadata verification | ACK `status=0x00` is parser acceptance; profile-save persistence is unconfirmed | Do not label BLE profile saves as persisted without a reconnect test |
| Polling rate (`0x06`) | Read/write supported globally | Exact nine-byte packet accepted in the same-hardware probe and changed the rate observed after USB reconnect; stock app skips BLE | Keep the BLE control disabled until a production BLE writer adopts the validated nine-byte contract |
| Battery | Unavailable while wired; no battery polling stream | Standard Battery Service `0x180f` / `0x2a19`, read and notify | Show battery only when the active transport advertises battery capability |
| Light mode `0x00` | Not subject to the BLE crash finding | Accepted in a corrected same-hardware probe; historical crash report remains unresolved | Keep the conservative explicit BLE block until the packet/test contract is independently resolved |
| Custom macros (`0x09`) | Not exposed by the Rust UI contract | One packet has been ACKed; full multi-page behavior is untested | Keep disabled unless a separate capability explicitly enables it |
| FFC1/FFC2 | Not used | Never accessed during normal probing; purpose is speculative | Never expose as a configuration or update control |
| Device name / RF slot | USB product/path identity | `M600-5.2` / `M600-5.4` names identify RF/host slots, not profile slots | Do not display BLE RF names as firmware versions or profile personas |
| Version query (`0x0b`) | USB read confirmed (wired + FA60 receiver); byte [4] mirrors `0x05` light mode, not firmware identity | No useful BLE response; firmware dispatcher does not populate the readback buffer | Expose as read-only composite state over USB; do not use as a model discriminator; unavailable over BLE |

The adapter should expose these transport capabilities explicitly, not infer them
only from a transport name:

```text
configReadback: validated | unavailable
writeFeedback: none | ack-acceptance
battery: unavailable | read-notify
pollingRate: supported | unsupported
profilePersistence: reconnect-verified | unknown
```


BLE ACK format:

```text
10 50 <status> <report_id>
```

`status=0x00` means that the firmware accepted and parsed the packet. It does not
mean that the setting was applied to the live working image or persisted to
nonvolatile storage. `status=0x01` means rejected/unsupported. No ACK is also a
distinct outcome and must not be treated as success.

The USB wired path has the opposite limitation: it has validated configuration
readback through the Rust worker but no per-report firmware ACK. The UI must not
invent an ACK status for USB, and must not treat a successful host HID write
without readback as proof of application.

Battery is a separate capability from configuration. On FA61 wired USB the
device is powered but does not provide battery telemetry through this path. The
UI should use an explicit `battery: unavailable` state rather than zero,
stale-last-value, or a fabricated charging percentage. BLE battery read/notify
must be scoped to a connected BLE session.

## 11. Non-goals and unresolved semantics

The prototype UI must not imply that the following are fully characterized:

- X3 host-color bytes have a confirmed RGB effect;
- every raw button action has a stable semantic label across firmware revisions;
- preserved DPI tail bytes are user-editable settings;
- sleep/deep-sleep fields have fully confirmed physical timing behavior;
- immediate USB readback proves persistence;
- a profile metadata change alone proves that its live working image has loaded.

These fields should remain raw, explicitly qualified, or hidden until stronger
model-specific evidence exists.
