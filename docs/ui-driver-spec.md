# Rust FA61 driver UI specification

Status: prototype integration contract for a user-facing UI.

This document describes the current Rust driver and GUI as a UI-facing capability
surface. It separates behavior that is implemented today from UI policy that an
adapter should enforce. It targets X3/FA61 wired devices and the constrained
X3/M600 BLE write path; BLE configuration reads and readback are unavailable, and
the GUI does not offer polling-rate writes over BLE.

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

Constrained BLE non-rate writes are covered below. BLE configuration
reads/readback, safe polling-rate writes, and the dangerous unverified BLE
polling-rate override are outside the GUI contract. Firmware update operations,
custom macro editing, battery over wired USB, and X11 `0xfa55`/`0xfa60` behavior
remain outside this contract.

### 1.2 Design principles

1. Every profile section read and write has an explicit one-based profile target,
   including polling rate.
2. Every profile-section write is read-modify-write and must preserve fields the user did not edit; polling-rate writes target the profile selected by `--profile`/the active profile editor context and replace one explicit per-profile value.
3. Every write carries a manager-owned validation choice: `Transport`
   (default) or `Readback`. `Transport` completes on transport-level
   submission — USB feature-report submission succeeded, BLE parser ACK —
   with no post-write readback. `Readback` re-reads the section after the
   write and succeeds only when the fresh state matches; it is unsupported
   over BLE.
4. A fresh readback proves the device returned the requested state; it does
   not by itself prove persistence across power loss. Neither transport
   submission nor readback is a persistence claim.
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
| Deep sleep | `1..=60` whole minutes | `deepSleepRaw` plus the high configuration bucket |
| Normal sleep | `0.5..=30` minutes in `0.5` steps | `sleepTimerRaw = minutes * 2` |
| Debounce | even `4..=50` ms | `debounceRaw = ((ms - 4) / 2) + 2` |
| Host color | raw bytes only on X3 | preserve `[r, g, b]` exactly |

The leaf crate exposes checked, allocation-free helpers for the three timing
controls — `DebounceMs` (even `4..=50` ms), `SleepTimer` (`0.5..=30` minutes in
`0.5` steps), and `DeepSleepMinutes` (`1..=60` minutes split across
`deepSleepRaw` and the high configuration bucket). The UI must convert through
these helpers rather than embedding the packet formulas: constructors and wire
decoders return `None` for invalid user values and noncanonical wire bytes, and
deep-sleep edits must preserve the low configuration nibble via
`configuration_with`. Exact conversions and canonical-state rules live in
[`protocols/05-preferences.md`](protocols/05-preferences.md#typed-conversion-helpers-rust).

The preference packet offsets and checksums are live/capture-confirmed, but the
actual sleep/deep-sleep **effects** are not fully live-characterized on X3.
Controlled wired FA61 stock-app captures confirm that the configuration bucket
advances at 16, 32, and 48 minutes while the deep-sleep byte becomes `0x08`;
all whole-minute values from 1 through 60 are therefore representable. The UI
must not present host color
as a confirmed RGB control — it should be shown as diagnostic/raw data or kept
hidden — and must not claim live-confirmed sleep behavior for fields whose
effect on this hardware is unverified. Unknown light-mode values must be
displayed as `Unknown (0xNN)` and preserved unless the user explicitly selects
a replacement.

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

### 3.6 Polling rate

```text
PollingRate {
  hz: 125 | 250 | 500 | 1000,
  code: 0x08 | 0x04 | 0x02 | 0x01
}
```

Polling rate is **per-profile**, stored and edited inside the profile editor
like DPI, preferences, and buttons. Report `0x06` byte 2 is the one-based
target profile; the firmware skips the profile loader for `0x06`, so byte 2
is a **save alias** — the deferred writer persists the complete *live* image
into that slot. Consequences:

- a read returns the rate of the profile currently live on the device (the
  UI must not claim a non-live profile's rate was read);
- a safe write is a manager preflight, not a bare packet: the complete
  desired non-rate image (DPI, preferences, buttons) for the target must
  exist and equal the fresh same-session read, persistent metadata must have
  `current == target`, and an already-equal rate is a no-op with no hardware
  write; the rate is written last, with the same validation choice as other
  section writes;
- transport submission or rate readback proves the immediate rate field of
  the live alias only, never the non-rate save image and never persistence;
- every actual write schedules a deferred flash save (sector erase), so
  repeated unchanged writes still wear flash; the safe path skips them as
  no-ops, and a UI must not retry blindly;
- over BLE the safe path is unavailable (no fresh complete-profile read);
  BLE writes are an explicit advanced override with ACK-only evidence and
  unknown persistence (see [Section 4.6](#46-polling-rate-writes-safe-preflight-and-ble-advanced-override)).

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
| `readPollingRate(profile)` | profile | validated rate | live rate; `0x06` never loads a profile |
| `writeDpi(state, validation?)` | profile | `Transport`: acknowledged; `Readback`: fresh verified `DpiState` | readback (when selected) targets `state.profile` |
| `writePreferences(state, validation?)` | profile | `Transport`: acknowledged; `Readback`: fresh verified `PreferencesState` | readback (when selected) targets `state.profile` |
| `writeButtons(state, validation?)` | profile | `Transport`: acknowledged; `Readback`: fresh verified `ButtonsState` | readback (when selected) targets `state.profile` |
| `writePollingRate(profile, rate, validation?)` | profile | `Transport`: acknowledged; `Readback`: fresh verified rate | `0x06` byte 2 names the target profile; safe preflight requires complete desired non-rate image and `current == target`; already-equal rate is a no-op (see [4.6](#46-polling-rate-writes-safe-preflight-and-ble-advanced-override)) |
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

The driver preserves the current maximum, writes the edge-triggered
model-correct `0x0c` control, holds a 500 ms wired or five-second FA60 receiver
quiet period without targeted section traffic, and verifies metadata. The UI
should avoid calling this operation when the target is already
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

The current implementation serializes the write, holds the same
transport-specific quiet period, and verifies metadata. The UI must not send a
raw standalone `0x0c` packet or expose a generic
profile-metadata editor.

### 4.4 Button writes

The Rust library can write a complete validated 18-slot table. The current CLI
provides a safer single-button operation: it reads the complete target table,
changes one recognized physical button assignment, and writes the complete table
under the chosen validation — `Transport` completes on submission, `Readback`
requires a matching fresh readback.

The UI should use the single-button abstraction for ordinary editing. It should
show a confirmation for changes to physical buttons and keep raw table editing
behind a diagnostics/developer capability, if exposed at all.

### 4.5 Proposed UI adapter interface

An IPC, FFI, or process adapter may expose the following shape to the UI. The
adapter must implement the transaction rules in this document rather than
forwarding arbitrary HID packets:

```ts
type ValidationMethod = "transport" | "readback";

type Immediate<T> = {
  value: T;
  verification: "transport-accepted" | "readback-verified";
  persistence: "unknown";
};

interface Fa61Session {
  readMetadata(): Promise<ProfileMetadata>;
  readPollingRate(profile: ProfileId): Promise<PollingRate>;
  readProfile(profile: ProfileId): Promise<ProfileSnapshot>;
  readDpi(profile: ProfileId): Promise<DpiState>;
  readPreferences(profile: ProfileId): Promise<PreferencesState>;
  readButtons(profile: ProfileId): Promise<ButtonsState>;

  writeDpi(state: DpiState, validation?: ValidationMethod): Promise<Immediate<DpiState>>;
  writePreferences(state: PreferencesState, validation?: ValidationMethod): Promise<Immediate<PreferencesState>>;
  writeButtons(state: ButtonsState, validation?: ValidationMethod): Promise<Immediate<ButtonsState>>;
  writePollingRate(profile: ProfileId, rate: PollingRate, validation?: ValidationMethod): Promise<Immediate<PollingRate>>;

  activateProfile(profile: ProfileId): Promise<Immediate<ProfileMetadata>>;
  setMaximumProfile(maximum: ProfileId): Promise<Immediate<ProfileMetadata>>;
  close(): Promise<void>;
}
```

For ordinary UI editing, add patch helpers such as
`updateDpi(profile, patch)` and `updatePreferences(profile, patch)`. These
helpers must read the current section, apply the patch, and write the complete
state under the chosen validation. They must not synthesize missing fields
from hard-coded defaults; when no stored section exists to merge against they
return `MissingBaseline` rather than inventing one.

`validation` defaults to `transport`. With `transport`, `value` echoes the
requested state and `verification` is `transport-accepted`; with `readback`,
`value` is the fresh readback and `verification` is `readback-verified`.
`persistence` is `unknown` in both cases. A `readback` request over BLE is
rejected up front with `UnsupportedOperation`. Activation and maximum-profile
updates always include their intrinsic metadata readback; they do not take
the validation choice.

`writePollingRate` implements the safe preflight of Section 4.6; the adapter
must not forward a bare `0x06` packet to the hardware.

### 4.6 Polling-rate writes: safe preflight and BLE advanced override

**Safe write preflight (manager-enforced).** `writePollingRate` must fail
**before any hardware write** unless:

1. complete desired DPI, preferences, and buttons exist for the target
   profile (`MissingBaseline` otherwise);
2. persistent metadata reports `current == target` (`MissingBaseline`
   otherwise — writing to a non-current slot would persist the live image
   under an alias the user is not editing);
3. the complete profile freshly read in the same session equals the desired
   non-rate image in every section (`MissingBaseline` naming the mismatched
   section otherwise);
4. the current rate read differs from the requested rate — an already-equal
   rate is a no-op returning the current state with **no hardware write**;
5. the write then goes out with the requested validation (`Transport` or
   `Readback`).

**BLE has no safe path.** BLE cannot freshly read the complete profile, so the
same operation fails with `ExplicitAuthorizationRequired`. The only BLE rate
write is an explicitly dangerous override:

```ts
writePollingRateUnverifiedBle(profile: ProfileId, rate: PollingRate): Promise<Immediate<PollingRate>>
```

It performs the direct packet write and returns **ACK-only evidence**
(`verification: "transport-accepted"`, `persistence: "unknown"`); `Readback`
is rejected up front.
Lower-level adapters that choose to expose it must gate it behind a visible
warning, never as a hidden fallback of the normal Apply flow, and must show its
result as `acknowledged (ACK only)` with `persistence-unknown`; the current GUI
does not expose this override.

**Current GUI contract.** `x3-gui` now uses `DeviceManager` on a dedicated
serialized worker and exposes the safe USB path plus a constrained BLE non-rate
configuration path:

- startup discovers one exact wired, receiver, or already-connected BLE device,
  reads status where the transport supports it, and shows errors instead of
  preview values;
- BLE DPI, preferences, button, and other non-rate edits are offered only when
  their complete baseline comes from stored/imported state or explicitly
  captured defaults authorized for that operation; the GUI never invents
  missing fields;
- BLE non-rate writes use transport/ACK evidence only
  (`acknowledged (ACK only)`); BLE has no configuration readback, so the GUI
  makes no readback or persistence claim for these writes;
- for USB, rate choices remain disabled until manager state contains complete
  desired DPI, preferences, and buttons for the current target; a missing
  baseline is surfaced before any draft section is written;
- USB Save calls the manager's safe polling-rate operation, never a bare `0x06`
  writer, and reports transport/readback evidence with persistence unknown;
- BLE safe polling-rate writes and the dangerous unverified polling-rate
  override are unavailable in the GUI and are never hidden fallbacks of an
  ordinary Save path;
- the shipped GUI exposes explicit USB-only `DeviceManager` actions for
  profile-reload and power-cycle verification. Profile reload performs the
  manager's deliberate profile switch-away/switch-back check; power-cycle
  verification closes the session and waits for the exact USB device to
  disappear and return before reopening it and comparing the complete profile
  readback;
- before starting power-cycle verification, instruct the user to unplug the
  mouse, power the mouse off, wait for the exact device to disappear, then power
  it on and reconnect it. Only a complete matching readback from one of these
  explicit actions may mark persistence verified; transport submission,
  immediate readback, and other acknowledgements remain persistence-unknown.
  These workflows are not available over BLE, and the GUI must not claim BLE
  readback or persistence evidence.

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

Profile-scoped writes take the validation choice described in Section 6;
`Transport` is the default. The choice is serialized with the write, never
decided after the fact.

`Transport` validation:

1. sends the model-correct report;
2. returns success on transport-level submission — USB feature-report
   submission succeeded, BLE parser ACK;
3. performs no post-write readback, so the result carries no readback
   evidence.

`Readback` validation (USB wired and FA60 receiver only; BLE has no readback
path and rejects the request up front):

1. sends the model-correct report;
2. holds the transport-specific quiet period (500 ms wired, five seconds
   through the FA60 receiver);
3. performs one fresh targeted readback;
4. compares the decoded state to the requested state;
5. returns success only when they match.

Polling-rate writes use the same serialized pattern with the explicit profile
carried by the `0x06` byte-2 slot, behind the safe preflight of
[Section 4.6](#46-polling-rate-writes-safe-preflight-and-ble-advanced-override):
the complete desired non-rate image and `current == target` are required, and
an already-equal rate is a no-op with no hardware write. A `0x06` readback's
byte 2 mirrors the armed selector and is a wire-shape check, not a live-content
proof.

### 5.4 Targeted-read warning

Reports `0x04`, `0x05`, and `0x08` carry an explicit profile target. Reading one of
these sections can load that profile into live working buffers without changing
persistent `0x0c` metadata.

The UI should:

- show the target profile in every section-read result;
- never label a target read as proof that the persistent current profile changed;
- avoid rapid polling of targeted sections after activation;
- allow the driver's transport-specific quiet period to complete before the
  first target read;
- invalidate cached working-section data after activation, reconnect, or
  another targeted read to a different profile.

A complete `readProfile(profile)` is the preferred refresh operation when the UI
needs a coherent profile view. It remains one serialized worker command, but the
result still reports persistent metadata separately from the target profile.

## 6. UI verification and persistence states

Writes carry a manager-owned validation choice. The choice belongs to the
manager (for example the global `--validation transport|readback` CLI flag)
and rides along with each write request; it is not an after-the-fact probe.

| Validation | Semantics | Recommended UX label |
|:-----------|:----------|:----------------------|
| `Transport` (default) | completes on transport-level submission — USB feature-report submission succeeded, BLE parser ACK — with no post-write readback | **Fast** |
| `Readback` | re-reads the section after the write and succeeds only when the fresh state matches | **Verify** |

Recommendations:

- **Fast is the default** for ordinary editing. It performs no post-write
  readback, so the write stays fast and quiet.
- **Verify is an explicit choice**, not an automatic Apply step. Each
  readback is a full read transaction: on an FA60 receiver a prepared read
  interrupts the primary pointer-report stream for about half a second
  (502–508 ms gap, median 502.972 ms; capture-confirmed 2026-08-06), so
  users should choose Verify deliberately when immediate-application evidence
  matters.
- Readback is **unsupported over BLE**: BLE has no configuration readback, so
  the choice is effectively fixed to `Transport`; the UI must say readback is
  unavailable rather than degrade silently. BLE rate writes additionally
  require the explicit advanced override of [Section 4.6](#46-polling-rate-writes-safe-preflight-and-ble-advanced-override);
  their result state is `acknowledged (ACK only)` with
  `persistence-unknown` — the UI must never display a BLE ACK as
  `verified-readback` or `persistence-verified`.

The UI should distinguish these states:

```text
unloaded
reading
loaded
dirty
writing
applied-transport
verified-readback
persistence-unknown
persistence-verified
error
```

Recommended write flow:

1. load and display the current validated state;
2. mark edited fields dirty;
3. on Save, send one read-modify-write operation with the selected validation;
4. replace the optimistic UI state with the result:
   - `Transport` — keep the sent state, mark it `applied-transport`, and show
     `Submitted (Fast); no readback performed`;
   - `Readback` — replace the section with the driver's verified readback and
     show `Verified; persistence not independently checked`;
5. keep `persistence-unknown` until a deliberate diagnostic
   `verify --method profile-reload|power-cycle` check re-reads the expected
   state.

Profile-reload and power-cycle verification remain **explicit diagnostic
actions**, stronger than either write-time option and never part of automatic
Apply behavior. They are the only paths that may set `persistence-verified`.

The UI must not claim persistence solely from transport submission, a BLE ACK,
a USB readback, or an immediate metadata readback.

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
| readback request over BLE | `UnsupportedOperation`: BLE has no configuration readback; tell the user readback is unavailable and fall back to `Transport` |
| `MissingBaseline` | a patch/delta update has no stored section to merge against; read the section first so the helper can read-modify-write |
| `ExplicitAuthorizationRequired` | a safe polling-rate write was requested over BLE (no fresh complete-profile read exists there); tell the user the operation needs explicit authorization, offer the wired transport, or surface the explicit advanced BLE override with its ACK-only evidence and unknown persistence |
| rate-safe preflight `MissingBaseline` | the target profile's complete desired non-rate image is missing or differs from a fresh same-session read, or persistent metadata `current != target`; load the target profile and reconcile the full image before offering the rate write — the write aborts before any hardware write |
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
4. Read profile metadata.
5. Read the current enabled profile with `readProfile(currentProfile)` for a
   coherent snapshot.
6. Populate profile tabs `1..=maximumProfile`; show higher slots as disabled.
   The current profile's polling rate is available from the current live read;
   other profiles show their last stored/read values.

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
4. Send the complete read-modify-write state for that section with the chosen
   validation (Transport/Fast by default).
5. Replace the section with the result: for `Readback`, use the verified
   readback; for `Transport`, keep the sent state marked `applied-transport`
   with no readback evidence.
6. Keep unrelated sections and opaque bytes unchanged.

### 8.4 Changing the maximum profile

1. Read metadata.
2. If reducing the maximum, require confirmation that higher slots will be hidden.
3. If the current profile would become disabled, first activate a profile at or
   below the requested maximum.
4. Call `setMaximumProfile(maximum)`.
5. Refresh metadata and rebuild the profile selector.
6. Do not delete cached higher-profile data solely because it is currently hidden.

### 8.4.1 Refreshing all profile observations

Use the manager-owned `refreshAllProfiles` workflow only from an explicit
advanced action. It is USB-only and writes profile metadata while temporarily
enabling all five slots and activating each one so its live polling rate can be
read honestly. The manager captures DPI, preferences, buttons, and rate in one
session, restores the exact original current/maximum metadata, and only then
commits all observations together.

Frontends must not reproduce this loop themselves or attach it to ordinary
startup/Refresh behavior. Preserve desired values, display the manager's drift
result, and state that persistence remains unverified.

The GUI may show a non-modal first-run recommendation when any of the five
profile slots lacks a complete observed DPI/preferences/buttons/rate image. It
must require an explicit confirmation before starting, keep “not now”
session-local, block the action while a draft is dirty, and keep the permanent
advanced action available. BLE views should explain that configuration
readback is unavailable rather than offering this workflow.


### 8.5 Reconnect/persistence verification

This is the explicit diagnostic workflow (`verify --method profile-reload` or
`verify --method power-cycle` in the CLI); it is never part of the normal
Apply flow.

1. Close the HID session.
2. Reopen the selected exact device path after it reconnects.
3. Read metadata and the expected profile sections.
4. Compare against the last verified state.
5. Set `persistence-verified` only for fields that match.

### 8.6 Changing the polling rate

1. Select the target profile and ensure it is current (activate if needed).
2. Ensure a complete validated snapshot exists for it (the safe preflight
   requires the full desired non-rate image and `current == target`).
3. Read the current rate; if it already equals the selection, save is a no-op
   that sends nothing — surface the equality rather than a fake write.
4. Apply the rate write with the chosen validation (`Transport`/Fast by
   default); `Readback`/Verify proves the immediate rate field only.
5. Show the result as `applied-transport` or `verified-readback` with
   `persistence-unknown`.
6. Persistence is only ever claimed after the explicit reconnect/power-cycle
   diagnostics of Section 8.5.
7. The GUI does not offer BLE polling-rate writes: both the safe preflight and
   the dangerous unverified override remain unavailable, rather than becoming a
   hidden fallback of the ordinary Save flow.

## 9. Capability matrix
| Capability | Current Rust implementation | UI exposure recommendation |
|:-----------|:----------------------------|:----------------------------|
| Discover FA61 collections | supported | required |
| Profile metadata read | supported | required |
| Profile activation | supported | required |
| Maximum-profile update | supported and bounded | required with confirmation |
| DPI read/write | supported per profile | required |
| Preferences read/write | supported per profile | required; preserve raw fields |
| Button table read | supported per profile | required for display/backup |
| Recognized button assignment write | supported by CLI | expose recognized physical controls |
| Arbitrary raw button editing | library-level only | diagnostics only, preferably hidden |
| Polling-rate read/write | supported per profile; readback USB-only | required inside the USB profile editor; unavailable over BLE |
| Battery telemetry on FA61 wired USB | unavailable; the wired path is silent | show `Unavailable on wired USB`; never poll and never display `0%` |
| Custom macros | not exposed | disabled |
| BLE non-rate configuration | constrained writes supported from complete stored/imported baselines or explicitly captured defaults; ACK only, no configuration readback | expose only this limited path; make no readback or persistence claim |
| BLE polling-rate writes | safe preflight unavailable; the dangerous unverified override is a lower-level operation | not exposed in the GUI; keep both safe and dangerous paths unavailable |
| Factory reset | not exposed as a standalone UI operation | disabled |
| Firmware update | unsupported and unsafe | never expose |

## 10. Cross-transport capability matrix

The current Rust UI contract includes full USB operations and a constrained BLE
non-rate write path. The BLE path must expose a different capability set instead
of reusing USB readback and persistence claims. The canonical BLE and battery
details are in [`transports/ble-gatt.md`](transports/ble-gatt.md) and
[`protocols/battery.md`](protocols/battery.md).

| Capability | FA61 wired USB | X3/M600 BLE GATT | UI contract |
|:-----------|:---------------|:-----------------|:------------|
| Configuration writes | `SET_REPORT`; no transport ACK | FEE3 write plus FEE4 ACK notification | USB success means transport submission (default) or validated readback, per the validation choice; BLE non-rate writes are offered only with a stored/imported baseline or explicitly captured defaults and succeed only on ACK acceptance |
| Configuration readback | Validated `0xa0` selector plus report read | Not available; FEE1 is opaque and is not configuration state | Never offer BLE `readDpi`, `readPreferences`, `readButtons`, or `readProfile` as if they were implemented |
| DPI/preferences/buttons writes | Read-modify-write with the chosen validation (transport submission by default, fresh readback on request) | Reports may receive ACK `status=0x00`, but there is no configuration readback | BLE UI may offer these non-rate writes only from stored/imported or explicitly captured complete baselines; show `Accepted by device; application/persistence unverified` and never claim readback or persistence |
| Profile metadata (`0x0c`) | Bounded read/write with immediate metadata verification | ACK `status=0x00` is parser acceptance; profile-save persistence is unconfirmed | Do not label BLE profile saves as persisted without a reconnect test; the GUI does not offer that verification |
| Polling rate (`0x06`) | Read/write supported per profile; safe preflight per [4.6](#46-polling-rate-writes-safe-preflight-and-ble-advanced-override) | Exact nine-byte packet accepted in the same-hardware probe and changed the rate observed after USB reconnect; stock app skips BLE; no safe path (no fresh complete-profile read) | The GUI exposes neither the safe BLE write nor the dangerous unverified override; no BLE polling-rate write or persistence evidence is offered |
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
validation: transport | readback (readback only where configReadback is validated)
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
without readback — the `Transport` path — as proof of application; that is why
`Readback` exists as an explicit choice.

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
- a standalone `0x06` write changes only the rate (it can persist the
  complete live image under the target alias, and a BLE ACK or rate readback
  does not prove the non-rate save image);
- a profile metadata change alone proves that its live working image has loaded.

These fields should remain raw, explicitly qualified, or hidden until stronger
model-specific evidence exists.
