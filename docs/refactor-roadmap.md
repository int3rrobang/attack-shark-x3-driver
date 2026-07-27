# Rust-only architecture and refactor roadmap

Status: proposed implementation plan
Recorded: 2026-07-27
Scope: X3/FA61, X3/M600 through the FA60 receiver, and qualified X3/M600 BLE behavior

This document records the intended post-refactor architecture, the migration sequence, durable-state semantics, GUI integration boundary, and verification rules. It is a plan rather than a description of the current source tree. Current protocol behavior remains defined by the Rust implementation, focused tests, protocol fixtures, and evidence documents.

## 1. Goals

The refactor should produce one Rust implementation of the supported X3/M600 protocol family and one production CLI. There is no public compatibility obligation, so obsolete implementations and interfaces should be removed rather than deprecated.

The result should:

- make the Rust codecs and drivers the only production implementation;
- retain exact packet framing, profile targeting, checksums, unknown-byte preservation, and safety restrictions;
- expose reusable, presentation-independent device operations for both `x3ctl` and a possible future GUI;
- distinguish desired configuration from state actually observed on hardware;
- distinguish parser acceptance, immediate readback, profile reload, and power-cycle persistence evidence;
- serialize stateful hardware operations within and across processes;
- preserve raw evidence without carrying obsolete runtime support;
- remain small enough to understand without a framework or daemon.

Correctness and truthful verification claims take priority over API uniformity. The refactor must not invent semantics for unknown bytes or generalize behavior across models or transports without evidence.

## 2. Supported product boundary

### 2.1 Supported implementation targets

- X3/FA61 wired USB, VID/PID `1d57:fa61`;
- X3/M600-family operation through the shared FA60 receiver, VID/PID `1d57:fa60`, with model qualification preserved;
- qualified BLE configuration through service FEE0 and characteristics FEE3/FEE4;
- BLE battery through the standard Battery Service where advertised.

### 2.2 Explicit non-targets

- X11 wired runtime support, including PID `0xfa55`;
- assuming that every mouse paired through an FA60 receiver is an X3;
- firmware update, OAD, FFC1, or FFC2 access;
- arbitrary raw button writes through production-facing commands;
- experimental scroll remaps known to risk repeated input;
- report `0x09` custom macros until a separately reviewed Rust implementation exists;
- unsupported protocol behavior retained only for historical compatibility.

Historical X11 evidence remains useful for provenance and dialect comparison. Removing X11 runtime support does not authorize rewriting or deleting raw X11 evidence.

## 3. Current baseline and reasons for change

The current branch contains three overlapping generations:

1. the original TypeScript package and Bun CLI;
2. an obsolete Rust CLI source file no longer registered as a Cargo binary;
3. the newer Rust library and `x3ctl` implementation.

The Rust protocol layer is already the strongest part of the repository. It provides typed constrained values, explicit profile targeting, full framing validation, checksums, complete button-table preservation, and fake-transport tests. The USB driver serializes selector/readiness/fetch transactions and verifies writes through fresh readback. BLE serializes writes and waits for matching ACK notifications while correctly avoiding readback claims.

The largest remaining structural costs are:

- roughly 7,000 lines of TypeScript product, tests, and one-off scripts;
- two CLIs plus an ignored obsolete Rust CLI source;
- a broker daemon and NDJSON wire schema for a local command-line application;
- repeated wire, stored, and protocol representations of the same state;
- application policy, persistent state, transport orchestration, output DTOs, and daemon lifecycle combined in a very large executor;
- Bun-only CI and repository instructions despite Rust being the intended production path;
- documentation that alternates between historical TypeScript status and current Rust behavior.

The refactor should remove parallel paths before reorganizing the remaining Rust code.

## 4. Target architecture

Use three workspace crates with one-way dependencies:

```text
attack-shark-x3
    protocol codecs and hardware transports
             |
             v
attack-shark-x3-manager
    device operations, state, safety policy, provenance
             |
       +-----+-----+
       |           |
       v           v
     x3ctl      future GUI
```

A future GUI changes the architecture only by making the headless manager boundary worth preserving. It does not justify retaining a daemon or designing an IPC protocol now.

### 4.1 Proposed workspace layout

```text
crates/
|-- attack-shark-x3/
|   |-- Cargo.toml
|   |-- src/
|   |   |-- lib.rs
|   |   |-- error.rs
|   |   |-- model.rs
|   |   |-- protocol/
|   |   `-- driver/
|   |-- tests/
|   `-- examples/
|
|-- attack-shark-x3-manager/
|   |-- Cargo.toml
|   `-- src/
|       |-- lib.rs
|       |-- manager.rs
|       |-- device.rs
|       |-- operation.rs
|       |-- defaults.rs
|       |-- error.rs
|       `-- state/
|           |-- mod.rs
|           |-- model.rs
|           |-- store.rs
|           `-- merge.rs
|
`-- x3ctl/
    |-- Cargo.toml
    `-- src/
        |-- main.rs
        |-- args.rs
        |-- output.rs
        `-- commands/
```

The existing root workspace should remain. Flattening the library into the repository root would create churn without improving a real boundary.

### 4.2 `attack-shark-x3` responsibilities

The low-level crate owns:

- packet layouts, lengths, fixed bytes, and checksums;
- constrained protocol values and decoded state types;
- target-profile validation;
- USB compact/full and receiver framing;
- BLE FEE3 writes and FEE4 ACK parsing;
- USB HID and BLE GATT access;
- selector/readiness/fetch transactions;
- bounded retries and write/readback verification;
- battery and DPI-button event decoding;
- hardware handles and transport errors.

It must not know about:

- CLI arguments or output;
- selected-device preferences;
- durable-state paths;
- import/export documents;
- GUI widgets or frontend events;
- captured defaults as universal protocol truth;
- daemon or IPC message formats.

The existing protocol modules are already appropriately explicit. They should not be replaced by generic report, checksum-strategy, or packet-framework traits.

### 4.3 `attack-shark-x3-manager` responsibilities

The headless manager owns reusable application behavior:

- discovery and exact device selection;
- safe device opening and session ownership;
- typed read and update operations;
- USB versus BLE capability policy;
- read-modify-write baseline resolution;
- safe captured defaults and authorization policy;
- durable desired and observed state;
- state provenance and persistence evidence;
- import/export;
- cross-process locking;
- mapping driver failures into frontend-neutral manager errors;
- event subscriptions for battery, DPI-stage changes, and disconnection.

It returns typed values and errors. It must not return human-formatted strings, CLI JSON envelopes, exit codes, or GUI-framework types.

A representative API shape is:

```rust
pub struct DeviceManager { /* private state */ }

impl DeviceManager {
    pub async fn list_devices(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredDevice>, ManagerError>;

    pub async fn read_status(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<DeviceStatus, ManagerError>;

    pub async fn read_dpi(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<DpiState>, ManagerError>;

    pub async fn update_dpi(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        update: DpiUpdate,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<DpiState>, ManagerError>;
}
```

Methods should be typed by operation rather than routed through a generic serialized `Request` enum. A future GUI can call the manager in-process. If process isolation becomes a demonstrated requirement later, a daemon can be another thin adapter over the manager.

### 4.4 `x3ctl` responsibilities

The CLI owns only:

- Clap definitions and validation specific to CLI syntax;
- mapping subcommands to manager methods;
- human output;
- JSON output intended for scripts;
- exit codes.

The CLI must not build packets, read `state.json` directly, duplicate BLE baseline logic, or infer verification claims from transport names.

## 5. Process and session model

### 5.1 Remove the broker

Remove the current:

- daemon lifecycle;
- named-pipe and Unix-socket server;
- auto-start behavior;
- NDJSON messages and protocol version;
- `RequestHandler` and `SessionRelease` interfaces;
- broker-specific request and response DTOs;
- `daemon`, `disconnect`, and `--direct` CLI surfaces.

Direct in-process execution becomes the only mode:

```text
parse command
  -> load state as needed
  -> acquire cross-process lock
  -> open exact device
  -> execute manager operation
  -> update state
  -> atomically save state
  -> release device and lock
```

Retain one stateless escape hatch. `--stateless` means no durable-state read or write. It does not weaken BLE baseline requirements: a partial BLE update without a supplied full baseline must still fail unless explicit defaults are authorized.

### 5.2 Preserve in-process worker serialization

The existing driver worker remains. Stateful X3 selector/readiness/fetch transactions must not run concurrently on one device. Clones of a handle must share the same serialized worker.

Split the driver at its natural boundary:

```text
driver/mod.rs      exports and shared errors/types
driver/handle.rs   public handle and command messages
driver/worker.rs   transaction engine and fake-transport tests
driver/usb.rs      HID mechanics
driver/ble.rs      GATT mechanics
```

The existing `FeatureTransport` abstraction earns its place because it isolates transaction logic and enables deterministic hardware-free tests. Do not expand it into a hypothetical generic device framework.

### 5.3 Cross-process lock

Replacing the broker must not allow two frontends to race hardware or state. Use a short-lived OS-backed lock stored alongside the state file.

The first implementation should use one global product lock:

```text
attack-shark-x3/state.lock
```

It serializes:

- CLI versus CLI;
- CLI versus a future GUI;
- two GUI instances;
- a hardware operation and its corresponding state mutation.

The lock must be released automatically when the owning process exits. Do not build heartbeat, lease, stale-PID, or lock-server protocols. Per-device locks should be considered only after real concurrent multi-device use demonstrates a need.

Offline debug commands need no hardware lock. Pure state reads may avoid it when they cannot race a write, but mutations must load the latest state only after acquiring the lock.

## 6. Durable-state design

### 6.1 State is evidence, not device truth

The state file is a cache of desired configuration and hardware observations with explicit confidence. Loading a cached value must never upgrade it to a live observation.

The manager must not auto-apply configuration at startup, device discovery, or GUI launch. Applying desired state is an explicit user operation.

### 6.2 Product-neutral location

The CLI and future GUI share one manager-owned state location.

Windows:

```text
%LOCALAPPDATA%\attack-shark-x3\state.json
```

Linux:

```text
$XDG_STATE_HOME/attack-shark-x3/state.json
```

Fallback:

```text
$HOME/.local/state/attack-shark-x3/state.json
```

Optional override:

```text
ATTACK_SHARK_X3_STATE_PATH
```

Do not store shared application state under an `x3ctl`-specific directory.

### 6.3 Desired and observed state

Store desired and observed resource values separately:

```rust
pub struct ResourceState<T> {
    pub desired: Option<DesiredState<T>>,
    pub observed: Option<ObservedState<T>>,
}
```

Desired state records what the user or import wants:

```rust
pub struct DesiredState<T> {
    pub value: T,
    pub source: DesiredSource,
    pub verification: Verification,
    pub updated_at: Timestamp,
}
```

Observed state records what a supported readback actually returned:

```rust
pub struct ObservedState<T> {
    pub value: T,
    pub source: ObservationSource,
    pub observed_at: Timestamp,
}
```

The initial source vocabulary should remain small:

```rust
pub enum DesiredSource {
    UserWrite,
    Imported,
    ExplicitDefaults,
}

pub enum ObservationSource {
    UsbReadback,
}
```

BLE configuration ACKs do not create observed state because BLE has no configuration readback.

### 6.4 Application and persistence verification

Keep immediate application evidence separate from persistence evidence:

```rust
pub struct Verification {
    pub application: ApplicationVerification,
    pub persistence: PersistenceVerification,
}

pub enum ApplicationVerification {
    NotSent,
    Acknowledged,
    ReadbackVerified,
    Mismatch,
}

pub enum PersistenceVerification {
    Unknown,
    ProfileReloadVerified { verified_at: Timestamp },
    PowerCycleVerified { verified_at: Timestamp },
}
```

Do not add speculative states such as `LikelyPersistent`, `EEPROMQueued`, or `AssumedPersistent`. A generic connection reopen while the device remains powered does not establish nonvolatile persistence and should not be represented as such.

Any change to a resource invalidates its prior persistence evidence. Importing or merging a new desired value sets application verification to `NotSent` and persistence to `Unknown`.

If verification returns a different value, keep both values:

- desired remains what the user requested;
- observed becomes what the device returned;
- application becomes `Mismatch`;
- persistence becomes `Unknown`.

### 6.5 State transitions

#### USB read

A successful fully validated USB read updates only observed state. It does not silently replace a different desired value.

#### USB write and immediate readback

A successful write followed by matching fresh readback updates:

- desired value and source `UserWrite`;
- desired application verification `ReadbackVerified`;
- observed value from the actual readback;
- persistence `Unknown` unless a stronger check was performed.

#### BLE write and ACK

A matching FEE4 ACK updates:

- desired value and source `UserWrite`;
- application verification `Acknowledged`;
- persistence `Unknown`;
- no observed configuration value.

#### Import

Import creates desired state with source `Imported`, application `NotSent`, and persistence `Unknown`. Importing a file never claims a hardware write or observation.

#### Explicit defaults

Explicit defaults create desired state with source `ExplicitDefaults`, application `NotSent`, and persistence `Unknown`. The user must separately authorize applying them.

### 6.6 Persist complete resource images

DPI state must retain the unresolved 25-byte tail required for safe read-modify-write. Serialize it as an exactly 50-character lowercase hexadecimal string rather than a JSON array.

Button state must retain all 18 assignments because report `0x08` replaces the complete table. A user-facing update to one safe button must preserve the other 17 slots.

Preferences must preserve raw unresolved fields rather than replacing them with host-UI names that lack confirmed X3 semantics.

### 6.7 Device identity

Do not key durable state primarily by an advertised name or reconstruct transport from the HID interface number.

Preferred wired USB identity:

```text
usb:<vid>:<pid>:serial:<serial>
```

When no serial exists:

```text
usb:<vid>:<pid>:path:<normalized-platform-instance>
```

Keep the current openable HID path separately because it may change.

Preferred BLE identity:

```text
ble:<stable-platform-device-id>
```

The advertised name is display metadata, not identity.

FA60 state must be receiver-qualified:

```text
receiver:<vid>:<pid>:<receiver-instance>
```

An FA60 receiver does not prove the identity of the currently paired mouse. Whenever readback is available, refresh the baseline from the currently paired device rather than automatically applying saved state. A pairing change can invalidate assumptions attached to the receiver entry.

Do not automatically merge wired, receiver, and BLE entries into one physical mouse. No shared identifier has been established. A future frontend may let a user assign a common display alias, but aliases must not cause automatic cross-transport writes.

### 6.8 State transaction

Every mutation follows one transaction:

```text
acquire cross-process lock
  -> reload latest state
  -> validate schema and resource invariants
  -> perform hardware operation
  -> update desired and/or observed records
  -> serialize to a sibling temporary file
  -> flush and sync as appropriate
  -> atomically replace state.json
  -> release lock
```

A long-lived GUI may cache state for display, but it must reload after obtaining the mutation lock. It must not overwrite changes made by another frontend from a stale startup snapshot.

The atomic-replace implementation must account for Windows replacement semantics rather than assuming Unix `rename` behavior.

### 6.9 Internal state versus portable exports

`state.json` is internal and machine-specific. It contains selectors, stable platform IDs, selected-device state, observations, timestamps, and verification evidence.

Portable export is a separate versioned document containing user configuration only. It must exclude:

- HID paths and BLE platform IDs;
- selected-device preference;
- observation claims;
- readback or persistence verification claims;
- machine-local timestamps.

Importing a portable document creates unsent desired state.

### 6.10 Schema evolution before public release

The project has no public state compatibility obligation. During this refactor, bump the schema version and reject old state clearly rather than retaining migration chains. Do not silently guess how to migrate baselines whose provenance cannot be established.

## 7. Stronger verification workflows

Persistence enumeration earns its place only when an operation can produce each state.

### 7.1 Default setter behavior

- USB setter: write, wait, perform fresh targeted readback, compare complete normalized state.
- BLE setter: write, wait for the matching ACK, report parser acceptance only.

A requested verification mode must never be silently downgraded. For example, asking BLE for profile-reload verification must fail as unsupported rather than returning an ACK as success.

### 7.2 Profile-reload verification

For profile-scoped USB resources, an explicit stronger check may perform:

```text
record original active profile and complete expected target state
  -> switch away to another already-enabled valid profile
  -> wait for deferred work
  -> switch back to the target profile
  -> wait for deferred work
  -> read target DPI, preferences, and buttons
  -> compare complete normalized state
  -> restore original active profile
  -> verify restoration
```

This is disruptive and must never run automatically on every write. Preconditions include:

- USB configuration readback is supported;
- a distinct enabled profile already exists;
- metadata and target resources validate;
- the original profile is known;
- recovery/restoration is prepared.

If only one profile is enabled, fail rather than changing the maximum-profile setting merely to run a test.

The strongest practical CLI shape is:

```text
x3ctl verify --profile 2 --method profile-reload
```

This can verify the complete profile with one switch cycle. Individual resource selection may be offered when useful.

Setters may also accept an explicit convenience option:

```text
x3ctl dpi set ... --verify profile-reload
```

The manager implements the workflow; the CLI only parses the option.

### 7.3 Power-cycle verification

An interactive power-cycle workflow provides the strongest host-visible evidence:

```text
record expected state
  -> release the hardware handle
  -> instruct the user to physically power-cycle
  -> wait for the exact device to disappear and return
  -> reopen it
  -> read complete expected resources
  -> compare
  -> store PowerCycleVerified only for matching values
```

Initial CLI form:

```text
x3ctl verify --profile 2 --method power-cycle
```

Do not initially build prepare/complete tokens, pending-verification daemons, or background watchers. Add a two-step non-interactive workflow only if an actual use case requires it.

### 7.4 Capability matrix

```text
                         ACK   immediate readback   profile reload   power cycle
FA61 wired               n/a          yes                yes             yes
FA60 receiver            n/a          yes                yes             yes*
BLE                      yes          no                 no              no
```

`*` Receiver results remain qualified by the identity of the mouse currently paired through that receiver.

A BLE-originated setting may later be observed through USB on the same physical mouse, but software must not infer that BLE and USB identities are the same device without an explicit, trustworthy association.

## 8. GUI integration boundary

A future GUI depends on `attack-shark-x3-manager` directly. It must not invoke `x3ctl`, parse CLI JSON, construct raw packets, or read `state.json` itself.

Representative flow:

```text
UI action
  -> manager typed update
  -> baseline resolution and safety validation
  -> driver write
  -> transport-appropriate verification
  -> durable state update
  -> typed outcome
  -> UI rendering
```

The manager may expose a small event stream for facts the hardware actually emits:

```rust
pub enum DeviceEvent {
    BatteryChanged(BatteryEvent),
    ActiveDpiStageChanged(DpiButtonEvent),
    Disconnected,
}
```

Do not create a general event bus, UI-state store, frontend plugin interface, or GUI framework abstraction. A future GUI chooses its own toolkit and maps manager results into presentation state.

Useful UI labels follow verification evidence:

- `Accepted by device; application and persistence cannot be read back` for BLE ACK;
- `Applied and immediately verified; persistence unknown` for USB readback;
- `Profile reload verified at ...`;
- `Power-cycle verified at ...`;
- an explicit desired-versus-observed mismatch when values differ.

## 9. Migration sequence

Every phase must leave the Rust workspace buildable and tested. Do not create compatibility aliases between phases.

### Phase 0: baseline correctness

1. Format the current Rust source.
2. Fix USB transport classification to use product identity rather than interface number.
3. Record a focused regression test for wired and receiver collections that both use interface 2.
4. Run the current full Rust test matrix and CLI smoke tests.
5. Commit the baseline before destructive refactoring.

### Phase 1: remove obsolete product surfaces

Delete:

- top-level TypeScript `src/`;
- `__tests__/`;
- `package.json`, Bun lockfile, TypeScript and ESLint configuration;
- Prettier configuration used only for the removed product;
- Husky hooks;
- the obsolete `attack-shark-x3.rs` Rust CLI source;
- TypeScript CLI documentation and examples.

Do not port the old class/builder hierarchy. Do not create an empty npm compatibility package.

Review each TypeScript probe separately. If its outcome is already preserved in evidence and documentation, delete it. Port only a probe that will genuinely be rerun, and port it when that experiment is needed rather than preemptively.

### Phase 2: create crate boundaries

1. Keep `attack-shark-x3` as the low-level library.
2. Create `attack-shark-x3-manager` and move application policy/state into it.
3. Create a dedicated `x3ctl` package and move the binary frontend into it.
4. Move Clap and CLI-only JSON dependencies out of the low-level package.
5. Make the library `serde` feature real and optional; let the manager enable it where direct domain serialization is appropriate.

Dependency direction must remain:

```text
x3ctl -> attack-shark-x3-manager -> attack-shark-x3
```

### Phase 3: remove daemon and wire schema

1. Change `x3ctl` to invoke manager methods in-process.
2. Add the short-lived cross-process lock.
3. Remove broker startup, IPC, request/response wire types, daemon lifecycle, and redundant direct mode.
4. Retain human and JSON rendering in the CLI.
5. Retain `--stateless`, `--dry-run`, explicit-default authorization, transport selection, exact device selection, and profile selection.

### Phase 4: consolidate domain and state types

1. Serialize protocol domain values directly where their representation is appropriate.
2. Remove structurally duplicate wire/stored/payload types that existed only for broker IPC.
3. Keep CLI-only output DTOs only where output differs materially from domain state.
4. Split `Auto` selection policy from the actual `TransportKind` enum.
5. Introduce desired/observed resource records and precise verification evidence.
6. Move captured defaults into manager policy with qualified names.

### Phase 5: split modules by cohesion

1. Dismantle the giant executor into manager operations grouped by resource.
2. Split driver handle and worker transaction logic.
3. Split state schema, I/O, and merge logic at their existing natural seams.
4. Leave packet-specific protocol modules explicit.

Do not split files merely because their test modules are long. Do not create one module for every tiny subcommand.

### Phase 6: documentation and automation cutover

1. Rewrite the root README as Rust-only.
2. Rewrite `AGENTS.md` around Cargo commands and Rust sources of truth.
3. Remove stale TypeScript implementation comparisons from canonical protocol pages.
4. Keep raw X11 evidence as explicitly historical and unsupported.
5. Replace Bun CI with Rust formatting, Clippy, tests, and Windows all-feature compilation.
6. Remove TypeScript Codecov and hook-manager configuration without immediately replacing them with new infrastructure.

### Phase 7: final cleanup and qualification

1. Review untracked evidence, session output, probe output, and local artifacts deliberately.
2. Preserve canonical evidence and fixtures; ignore or remove machine-local output.
3. Run the complete offline verification matrix.
4. Run explicitly authorized conservative hardware smoke tests.
5. Confirm documentation links and support claims.

## 10. Verification gates

### 10.1 Continuous offline gate

After every implementation phase:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Also check meaningful feature combinations:

```text
cargo check -p attack-shark-x3 --no-default-features
cargo check -p attack-shark-x3 --features usb
cargo check -p attack-shark-x3 --features ble
cargo check -p attack-shark-x3-manager --all-features
cargo check -p x3ctl --all-features
```

### 10.2 Protocol tests

Retain fixture-driven tests for:

- exact packet bytes and declared lengths;
- target profile;
- compact and full receiver framing;
- checksums;
- invalid fixed bytes;
- invalid profile and DPI values;
- complete button-table preservation;
- unknown DPI tail preservation;
- malformed readback rejection.

The checked-in JSON fixtures become the sole cross-implementation packet oracle after TypeScript deletion.

### 10.3 Driver tests

Retain fake-transport coverage for:

- selector/readiness/fetch ordering;
- retry rearming and bounded exhaustion;
- malformed target and checksum mismatch;
- write followed by fresh readback;
- profile activation quiet periods;
- receiver framing;
- worker shutdown;
- battery and DPI-button input decoding.

### 10.4 Manager and state tests

Cover observable contracts:

- schema rejection;
- atomic save behavior;
- lock serialization;
- stable device identity rules;
- desired/observed separation;
- BLE ACK without observed-state promotion;
- missing-baseline rejection;
- explicit-default authorization;
- partial merges preserving omitted fields;
- invalidation of persistence evidence after mutation;
- profile-reload and power-cycle verification transitions;
- portable import/export excluding machine-local claims.

### 10.5 CLI smoke tests

At minimum:

```text
x3ctl --help
x3ctl devices --help
x3ctl debug dpi ...
x3ctl debug prefs ...
x3ctl debug buttons ...
```

Test argument parsing, JSON output, exit status, ambiguous selection, unsupported verification requests, and useful error messages. Do not test source text or internal module layout.

### 10.6 Hardware qualification

Hardware operations require explicit authorization and the safety checklist.

FA61 wired:

- discover exact configuration collection;
- read metadata and complete profile state;
- perform one known-good write with immediate readback;
- restore original state;
- optionally perform one explicit profile-reload verification.

FA60 receiver:

- report receiver transport by product identity;
- verify receiver readback framing;
- observe the X3/M600 battery signature;
- perform one known-good readback-verified write and restore;
- retain paired-device qualification.

BLE:

- discover and connect to the exact bonded device;
- perform one typed safe write;
- verify matching ACK handling;
- confirm state records acceptance without observed/persistence claims;
- do not access firmware-update characteristics.

## 11. CI target

Use one small Rust CI workflow.

Linux:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Windows:

```text
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
```

Windows coverage matters because USB collection selection and BLE WinRT compilation are platform-sensitive. Do not add macOS CI until a supported macOS path exists. Do not replace TypeScript coverage with LLVM coverage or a threshold during the structural refactor.

## 12. Documentation and evidence rules

- Protocol documents remain canonical for stable packet facts.
- Raw captures and dated probe results remain byte-for-byte evidence.
- The sibling `attack-shark-x3-binary` repository remains the heavy reverse-engineering workspace for firmware, native binaries, web bundles, DLLs, and Ghidra projects.
- Do not copy those bulky artifacts into this repository.
- Summaries must retain model, transport, and evidence qualification.
- A BLE ACK remains parser acceptance only.
- Immediate USB readback remains working-state evidence only.
- Persistence is claimed only after the corresponding explicit workflow succeeds.

## 13. Deliberate non-goals

The refactor must not grow into any of the following:

- a GUI implementation;
- an HTTP or WebSocket service;
- a replacement daemon;
- a plugin system;
- a generic mouse-driver framework;
- separate crates for every packet or transport;
- a database for state;
- a state migration framework before public release;
- automatic matching of USB, receiver, and BLE identities;
- automatic application of cached state;
- raw firmware-update support;
- arbitrary raw button actions;
- custom macro implementation folded into structural work;
- a generalized packet/checksum trait hierarchy;
- a new task runner or hook manager;
- repository flattening or renaming for aesthetics;
- rewrites of already clear packet codecs merely for uniformity.

## 14. Completion criteria

The refactor is complete when:

- no production TypeScript or Bun tooling remains;
- only `x3ctl` is exposed as a CLI;
- the low-level crate, manager, and CLI have one-way dependencies;
- neither CLI nor future frontend must construct packets or access state files directly;
- the daemon and wire protocol are gone;
- hardware and state are serialized across processes without a service;
- desired and observed state are distinct;
- BLE ACKs cannot appear as readback or persistence verification;
- stronger verification is explicit, transport-capability-checked, and safely restorative;
- X11 is historical evidence rather than runtime support;
- Rust formatting, Clippy, tests, and CLI smoke checks pass;
- documentation describes the implemented Rust architecture without stale TypeScript status.

The intended simplification is not to remove protocol caution. It is to remove alternate implementations and application plumbing so there is one understandable path:

```text
frontend input
  -> typed manager operation
  -> durable-state and safety policy
  -> Rust driver
  -> packet codec
  -> hardware
```
