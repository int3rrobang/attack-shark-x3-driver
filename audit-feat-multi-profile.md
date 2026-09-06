# `feat/multi-profile` — read-only audit (2026-08-28)

> Branch: `feat/multi-profile` @ `0950975` (origin/main…HEAD +12168/-2216, 47 files). Scope: multi-profile impl + pre-existing issues, bugs/UX/bloat/redundant tests/perf. No fixes applied.

## TL;DR verdict

Feature is structurally sound (watermark invariant correct, ceremony journal durable+resumable) but **carrier branch not shippable**: stale AGENTS/docs drift, handful of state-corruption/TOCTOU/async-blocking bugs with real persistence risk, protocol-edge mis-classifications, and diffuse bloat. Biggest cluster is manager ceremony+discovery+lock stack (`manager.rs` ~5.6k + `state/model.rs` ~3.2k).

---

## 1. Protocol + driver — `attack-shark-x3`

### Correct
* Watermark layout exactly matches spec: `WATERMARK_LENGTH==FIXED_TAIL_LENGTH==25`, `X3ID` @0, ver `1` @4, 16 B token @5, CRC32 BE @21 of `0..21`, outer `sum16` at `50..51` recomputed in `DpiReport::encode` so tail is automatically checksummed — `dpi.rs:18-102, 450-620`.
* `decode_watermark` strict all-or-nothing: wrong-len/magic → `Absent`, ver≠1 → `UnsupportedVersion`, CRC fail → `Malformed` — `dpi.rs:152-188`. No `unsafe`, `#![forbid(unsafe_code)]` holds.
* DPiReport framing correct: declared `0x38` wired/ble, `0x38|0x3a` receiver tolerance, `Full` (56, +4×`0x00` padding) USB-only, `Compact` (52) every transport — `dpi.rs:540-700` and `profile.rs`.

### Bugs / risks

| Sev | Location | Issue |
|---|---|---|
| **MAJOR** | `dpi.rs:290-420` `captured_empty_profile_one` hardcodes `ProfileId::MIN(1)` while `captured_stock_reset(profile)` is retargetable — inconsistent API, callers must remember which is profile-parametric. Also `overlay_physical_id`/`with_physical_id` unconditionally overwrites `preserved_tail` with no guard against overwriting `Valid`/`UnsupportedVersion` tails — ceremony check lives only in `manager.rs`. Lab-only overwrite of foreign watermark would silently succeed at this layer. |
| **MAJOR** | `dpi.rs:450-520` `DpiReport::encode(state, transport)` ignores `transport` (`let _ = transport`) and always emits `Compact`. Callers reasonably expect transport-aware length; `encode_framed_for_transport` exists but is separate. Receiver always gets `0x38` even though readbacks may present `0x3a` — write/read framing split correct per spec but unexplained in API docs. |
| **MINOR** | `dpi.rs:730-900` duplicate hex helpers `preserved_tail_hex` (50 chars) vs `physical_id_hex` (32 chars) — near-identical serde modules, should be generic `const N`. |
| **MINOR** | `driver/handle.rs:15-55` `ReadPolicy::receiver_default` `write_delay = 5s` executed via `thread::sleep` inside worker thread — blocks the single HID worker, queue depth 32 stalls subsequent commands (GUI freeze on receiver writes). |
| **MINOR** | `driver/worker.rs:20-110` no multi-profile batch coalescing: each `read_profile` does 4 sequential armed `0xA0` reads (metadata+dpi+pref+buttons) with 500 ms initial delay on receiver → ~2 s per profile snapshot; `capture_all_profiles` `5×` that. No pipelining. |
| **MINOR** | `driver/worker.rs:560-700` `spawn_input_worker` marks `input_available=false` on any read error and exits without restart/reconnect; battery telemetry then stuck at last value. `Lagged` broadcast (cap 16) silently dropped. |
| **MINOR** | `fixtures/protocol/dpi.json` only 4 fixtures (one DPI set) — gaps: no watermarked tail, no BLE, no 1/8-stage extremes, no checksum/declared-length negative, no receiver `0x3a`. Watermark-correctness is probed only via inline unit tests. |

### Bloat / pre-existing

* `Cargo.toml:6-35` `windows = "0.48"` pinned while `cargo update` moved most deps — drift to `0.59+`. `bluest 0.6.9` likewise.
* `checksum.rs` single `sum16` wrapping sum — correct but tested in three overlapping suites (inline + `tests/checksum_codec.rs` + `dpi_codec.rs`) with near-duplicate wrapping cases.

---

## 2. State model — `attack-shark-x3-manager/src/state/model.rs` (~3206 ln)

### What landed
`SCHEMA_VERSION 5` (was 4, no migration — intentional), `IdentityMode::{Legacy,Persistent}`, `IdentitySetup{Phase,Stage,Journal,Subject,StampProgress}`, `CapturedProfileImage`, per-device `profile_names`, `ResourceState::is_empty`.

### Bugs / invariant holes

| Sev | Location | Issue |
|---|---|---|
| **CRITICAL** | `model.rs:590-600` `CapturedProfileImage` never validated. `validate_identity_setup` checks `stamp_progress` tracks captured keys but never calls `validate_resource_state` on `captured.profile_metadata` or each `captured.profiles[].{dpi,prefs,buttons,polling_rate}`. A journal can persist `ReadbackVerified+Mismatched`, `PersistenceVerification` without equality, or wrong `ProfileId` and resume stamping from poisoned baseline. `skip_serializing_if = ResourceState::is_empty` on `profile_metadata` also erases empty-vs-absent distinction. |
| **CRITICAL** | `state/store.rs:320-410` mutation path bypasses validation. `StateStore::mutate_async`/`transaction.commit` write atomically without re-running `StateFile::validate`. `validate` only on load/discard, so `next_device_number==0`, duplicate `PhysicalId`, or journal `reserved == already-assigned` can be persisted and only fault on next load. Observed via `DeviceManager::import_configuration` (validates import doc, not resulting `StateFile`). |
| **MAJOR** | `model.rs:552-580` `IdentitySetupSubject` all-`Option` with `#[serde(default, skip_serializing_if=…)]` + no `#[serde(deny_unknown_fields)]`. Stripped JSON silently yields subject with `token=None/captured=None/stamp_progress={}` — valid per serde but semantically empty. Typo `stmpProgress` silently ignored → `is_fully_stamped==false` → infinite stamping loop. Same for `StateFile::identity_mode` `#[serde(default)]` → missing field defaults to `Legacy` (correct for compat, but hides truncated file). No `deny_unknown_fields` anywhere in model. |
| **MAJOR** | `model.rs:682-820` `validate_identity_mode` vs `validate_identity_setup` coupling fragile: `Legacy` forbids `devices.len()>1` and any `physical_id`, yet `InitialEnrollment` journal (which requires `Legacy`) is allowed to hold 2 subjects while still `Legacy` with 0–1 devices. Direct `devices` insertion inside a `mutate_async` closure that skips `validate` can commit `Legacy+2 devices` without error. |
| **MAJOR** | `model.rs:608-620` `is_fully_stamped` requires exact key equality (`len==captured.len && keys all in captured && values all Stamped`) — correct strictly, but callers treat `Stamping` with empty `captured` as not-stamped (returns `false`) which tripped the `AwaitingCapture→Stamping` guard at `850-970` (extra `is_fully_stamped` check). Edge with zero-profile device (theoretical) falls through and blocks finalization with confusing "incompletely stamped subject". |
| **MAJOR** | `model.rs:904-970` `reserved` Vec + `seen.contains(&physical_id)` clone-heavy, but functional; duplicate-token check uses `&PhysicalId` pointer equality via `contains` on `Vec<&PhysicalId>` — works because `PhysicalId: Eq`, but reserves `&` to stored `BTreeMap` values which can alias across subjects if token bytes equal — deduplicated correctly but allocates per-validate. |
| **MINOR** | `model.rs:60-63` `ResourceState::is_empty` skips `profile_metadata` in `CapturedProfileImage` but no `ProfileState::is_empty` — an empty `ProfileState { dpi: empty, prefs: empty, buttons: empty, polling: empty }` still serializes into `captured.profiles` and bloats journal (2 subjects × 5 profiles × 4 resources × 25 B tail). |
| **MINOR** | `device.rs:114-180` `DeviceLocator::UsbPath(String)` verbatim HID path, no sanitisation before use in `device_lock_path` (`state_file.parent().join(format!("device-{id}.lock"))`) — `UsbPath` containing `../` would not escape (join drops), but still creates unexpected filename. `validate` trims blank→`None` for `serial_number` but `is_coherent` allows `BlePlatformId` with stale `vendor_id/product_id` from tampered JSON. |

### Copies / bloat

* Extensive clones: `StateStore::load_async` clones whole `StateFile`, `mutate_async` clones `desired_owned`, `overlay_physical_id` clones 25 B tail per profile, `CapturedProfileImage` duplicates entire `ProfileState` tree per subject — 2×5×4 `DpiState` = 40 `preserved_tail` copies per enrollment. No `Cow`.
* Duplicate types: `PortableDpi` (`resources/state.rs:28`) vs `DpiState`, `ProfileConfiguration` vs `ProfileState`, `IdentityStampProgress` tri-state duplicates `Verification` state machine in different vocabulary. `merge_portable_dpi` reconstructs via `baseline.preserved_tail` — coupling not type-enforced.

### Tests

* ~1500 ln in `model.rs` tests, heavily redundant: `readback_verified_requires_equal_and_not_stale`, `mismatch_requires_differing`, `persistence_claims_require_current_matching`, `invalid_persistence_combinations` all exercise same `validate_resource_state` predicates with `800` vs `1600` values, each building a full `BTreeMap` insertion. Should be parameterised macro; coverage still misses `captured image` validation and empty-profile-in-captured edge. `persistent_mode_requires_unique_physical_ids` overlaps `legacy_mode_rejects_multiple_devices`. Single `setup_journal_rejects_impossible_combinations` with ~10 sub-cases should be split. Compile-time bloat visible (`222 passed` manager suite includes many near-duplicate permutations).

---

## 3. Manager — `manager.rs` (~5596 ln) + `backend.rs` / `refresh.rs` / `verification.rs` / `events.rs`

### Architecture
`DeviceManager` owns `StateStore` (fs2 `state.lock` + atomic JSON swap) + `SessionFactory` (Real `hidapi`/`bluest` or `ScriptedFake`) + in-memory `Mutex<BTreeMap<(TransportKind,DeviceLocator),(WatermarkDecode,DeviceEndpoint)>>` identity cache + `AtomicBool skip_migration`. Discovery: `list → ensure_legacy_mouse → per-attachment watermark cache → resolve_connection → mark_scan_duplicates → sync_endpoint_associations`. Hotplug via `nusb::watch_devices` (mpsc 16). Ceremony journal drives 4 phases (InitialEnrollment 2-step, Add/Restore/Foreign/BLE) via `begin→reconnected(capture+evidence)→stamp(token mint + per-profile write_dpi)→finalize`.

### Bugs

| Sev | Location | Issue |
|---|---|---|
| **CRITICAL** | `store.rs:320-380` `acquire_operation_lock_disk` does `thread::sleep(5ms)` busy-loop while called from async via `open_locked` — blocks Tokio `current_thread` pool (GUI. Same for memory variant `382-410`). Should be `tokio::time::sleep`. Under contention GUI heartbeat stalls and battery/topology tasks miss deadlines. |
| **CRITICAL** | `manager.rs:198-260` identity cache trust brittle: `cache.get(key)` trusted only when `cached_endpoint == &disc.endpoint` (exact `DeviceEndpoint` equality incl. `display_name`/`serial_number` trimming). A legitimate serial trim (`" ABC "`→`"ABC"`) invalidates cache and forces redundant `read_endpoint_identity` (armed read + cursor freeze) each scan; conversely path reuse with same `(transport,locator)` but different `DeviceEndpoint` metadata would be considered uncaching correctly, but equality is too strict and latency multiplies (`to_read` loop serially `await open` per endpoint). |
| **CRITICAL** | `manager.rs:621-715` `authenticate_attachment` uses `Mutex` without async awareness and `discover` serialises `read_endpoint_identity` opens in a for-loop (`for endpoint in to_read { await open }`) — latency ∝ device count on cold plug with 2 wired receivers. Also `open_locked` holds per-device file guard while calling `factory.open + current_watermark` — deadlock risk if ceremony for same device tries to acquire same `device-<id>.lock` concurrently (both call `open_locked`). |
| **MAJOR** | `manager.rs:1500-1520` `begin_identity_ceremony(Restore)` checks `Wired+Receiver` connected locators but not `BLE` — a USB restore target with only BLE still connected still passes "device-connected" check, then `ceremony_reconnected` refuses later ("requires-usb"), confusing ladder. |
| **MAJOR** | `manager.rs:1751-1835` `ceremony_reconnected(ForeignAdoption)` token-disagreement check inspects only `observed.preserved_tail` watermark; `Malformed` tails fall through to `token-disagreement` refusal even though malformed is arguably "damaged identity" and should be adoptable via re-stamp? Inconsistent with `Absent` being adoptable. Future `UnsupportedVersion` correctly refuses but error path same bucket. |
| **MAJOR** | `manager.rs:1911-1970` `ceremony_stamp` pending vec built from `captured.profiles` keys without sorting — stamping order non-deterministic for resume (BTreeMap iteration is sorted, so actually deterministic here; but todo in code claims non-deterministic — worth locking order explicitly). Also `original_metadata` read from `captured.observed` while `capture_all_profiles` already expanded `maximum` — restore may leave device expanded (`maximum=5`) even if capture failed mid-way; `restore_original` priority inverted (`RefreshRestoreFailed` loses typed error). |
| **MAJOR** | `manager.rs:2005-2135` `ceremony_associate` checks "endpoint already belongs to device X" inside `mutate_async` closure but `discover` not re-checked between user picking unassociated row and action — TOCTOU: endpoint could have been claimed by concurrent `discover→sync_endpoint_associations` between the two awaits, then `mutate_async` validation also races (only `validate_identity_setup` end check). Could attach BLE that just became associated via discovery. |
| **MAJOR** | `refresh.rs` `capture_all_profiles` iterates `order = once(original)+filter(1..=MAX)` — correct when `original==MAX` (filtered) but hardcodes `MAX=5` not `ProfileId::MAX` generic (harmless now but spec says generic). `read_live_polling_rate(target)` after `read_profile(target)` assumes profile still active; driver may have auto-switched on error leaving live-rate read pointed at wrong profile (spec notes `0x06` rate read is live-image). |
| **MAJOR** | `verification.rs:180-260` `verify_power_cycle` snapshots `device_state.physical_id` before `wait_for_disappearance`; if token rotates during wait, reappearance reads new token and comparison fails as generic mismatch not "physical identity rotated" presentation. `wait_for_disappearance/wait_for_reappearance` fixed 250 ms poll (1 ms in tests) can miss disconnect+reconnect inside interval → spurious timeout. `observed_at >= updated_at` persistence gate fails if system clock skews backward (no monotonic check). |
| **MAJOR** | `events.rs` `subscribe_events` opens HID session then spawns task with `input.recv` loop, but dropped sender leaks until `input` closed (no `AbortHandle`). Double broadcast (device→inner 16, inner→outer 16) effectively 32-lag before `Lagged` surfaced; inner `Lagged {skipped}` forwarded as single `Lagged {1}` losing original count. No synthetic disconnect — consumer cannot distinguish unplug vs silent. Concurrent `subscribe_events` on same device opens second HID handle → Windows `ERROR_SHARING_VIOLATION` not mapped to `DeviceOperationBusy` (maps to generic `InvalidUpdate`). |
| **MAJOR** | `manager.rs:2504+` `ceremony_progress_from_journal` maps `AwaitingCapture` both to "ready" and "awaiting reconnect" — conflated UI states; `journal_blocks_device` blocks entire installation for `InitialEnrollment` via `open_locked` (single file lock still, but discovery association silently continues). `skip_migration: AtomicBool` separate from journal — should be journal field; survives unexpected restart? Value lost on process restart mid-ceremony (intentional opt-in forgotten). |
| **MAJOR** | `manager.rs:546-720` + `store.rs` discovery snapshot isolation absent: `load_async` (242) then `mutate_async` (320) with no snapshot isolation — device can disappear between `resolve_connection` and `sync_endpoint_associations` (handled via `continue` but silent loss, no `UnassociatedReason` surfaced). `mark_scan_duplicates` folds Unknown per-transport but `resolve_valid_token` separately folds per-token across transports — duplicate logic diverged once already. |
| **MINOR** | `operation.rs` `IdentityCeremonyStage::Failed{error: String}` stringly typed + `UnassociatedReason::ReservedByJournal` vs `Reserved` conflation not in help. `DeviceEvent::Lagged{skipped:u64}` unbounded no backpressure. `VerificationMethod`/`BaselineSource` serde `camelCase` but CLI flags `kebab` — mapping scattered. |
| **MINOR** | `backend.rs` `UsbSession::write_profile_metadata` re-reads metadata then conditionally `set_maximum_profile` twice — if first raise fails after raising max, second lower never runs (no rollback). `BleSession::write_polling_rate_unchecked` Transport path silently succeeds though BLE polling not supported; `unsupported` gate only on `Readback`. `RealSessionFactory::list(Auto)` concatenates USB then BLE sequentially — BLE await after sync USB, timeout starves BLE. |

### Stale / dead paths

* `DeviceManager::link_devices` / `rebind_missing_endpoint` removed per `x3ctl/tests::link_and_rebind_commands_are_removed` but `AGENTS.md` still documents them as primary workflow; `manager.rs` still exposes internal helpers with same names but `pub(crate)` — dead code not pruned or behind `#[cfg(test)]`.
* `ceremony_adopt` vs `Adopt` overloaded (covers `Begin` and `Stamp` confirm) — single CLI flag doing two steps violates ceremony step isolation.

---

## 4. GUI — `x3-gui`

### Functional bugs

| Sev | Location | Issue |
|---|---|---|
| **MAJOR** | `worker.rs` `AutoRefresh` coalesce bug: `queue_command` in `main.rs` coalesces successive `Refresh` into single `AutoRefresh`, but worker expects `AutoRefresh` only from hotplug poll; rapid navigation can drop explicit user `Refresh` (replaced by coalesced). |
| **MAJOR** | `main.rs` + `worker.rs` command-queue mismatch: `main.rs` pushes `Command::Startup` at launch but worker initial `discover` also triggered by `Startup`; topology watcher `HOTPLUG_SETTLE_DELAY 750ms` plus 2 s poll can race initial discover, delivering duplicate `DiscoveryView` before snapshot ready — `projection::apply_snapshot` early-returns on `VecModel` downcast failure silently. |
| **MAJOR** | `worker.rs` battery dual-path: `LiveSnapshot::battery` from `DeviceManager::read_battery` (receiver-only `03 10 40 01`, wired fails) vs `DeviceEvent::BatteryChanged` from auxiliary HID input report. Two sources + different transports; wired device with receiver dongle shows empty-string battery until next input event — `app-window.slint` conditionally hides battery text when `battery==""` (overview page) causing flicker. |
| **MINOR** | `presentation.rs:785` `snapshot_preserves_draft` ignores `dirty` flag in one branch (warns but still updates profile model) — dirty draft silently replaced when snapshot arrives on same tick as edits. |
| **MINOR** | `projection.rs:921` `replace_*_model` batch `VecModel::set_vec` correct but initial `clear_live_state` called without `replace_*` guard, leaving stale `last_enabled_profile=-1` displayed while profiles empty. |
| **MINOR** | `presentation.rs` + `app_settings.rs` duplicate `dpi_bounds`/`round_dpi_step` (presentation 1407 ln vs app_settings 604 ln). `app_settings::TEMP_COUNTER` static mutable counter for atomic file naming — process-global, collides across tests parallel. `normalised()` equality for coalesced `gui-preferences.json` write compares normalised form, so ordering-only change suppressed but semantically same — fine but hard to reason. |
| **MINOR** | `app-window.slint:1649` hard-coded `popup-above := index>=4` — assumes DPI stages ≤8 but popup placement wrong after dynamic stage add/remove. `DeviceOption` opacity toggled for disconnected devices makes row inaccessible to AT (Slint `accessible-role` not set). Overview battery empty-string conditional also hides receiver battery when truly 0% (should distinguish `None` vs `Some(0)`). |
| **MINOR** | `main.rs` BLE baseline forced via `if (label.contains("ble"))` string contains on transport label rather than typed `TransportKind::Ble` — brittle if label changes (presentation helper already returns typed detail). Ceremony routing via `int` codes (`UNASSOCIATED_ADD 0…`) not typed union. |

### Bloat

* `x3-gui/src/worker.rs` 2991 ln — single file owns `Command`, `LiveSnapshot`, Tokio `select!` (shutdown/commands/events/topology/battery), `DeviceManager` discovery + `ceremony_hint` mapping + `visible_topology`. Could split `ceremony_worker.rs` and `battery_worker.rs`.
* `presentation.rs` 1407 ln pure formatters with many `format_*` helpers duplicating `manager::operation::human_*` — presentation layer rightly separate but helpers like `transport_label_for_identity` duplicate `manager` presentation (three copies).
* `Cargo.toml` `slint 1.17.1` femtovg/winit + `accessibility` feature enabled but no a11y tests; `default usb` asymmetry — GUI always builds with `usb` even when manager debugged via `memory` backend — `#[cfg(feature="usb")]` guards scatter `worker.rs`.

---

## 5. CLI — `x3ctl` (`main.rs` 2562 ln + `args.rs`)

### Correct

* Dry-run prints `dry-run: <action> (no hardware/state access)` and returns before `StateStore`/`DeviceManager` creation — true stateless guarantee (`main.rs:60-150`).
* `--allow-unverified-ble-rate-write` correctly scoped to `RateCommand::Set` not global — `args.rs:400-500`.
* `--stateless` / `--memory` vs default paths isolated; `--dry-run` before hardware validated.
* New identity commands match spec: `identity {status,begin,restore,adopt,associate,reconnect,stamp,continue,accept-migration,skip-migration,cancel}` — `args.rs:280-380`.

### Bugs / UX

| Sev | Location | Issue |
|---|---|---|
| **MAJOR** | `main.rs:520-650` `IdentityAdopt` overloaded: begins `ForeignAdoption` if none, stamps if already `Stamping`, errors otherwise. Single `identity adopt` doing two ceremony steps violates step isolation, complicates `--help` and `--dry-run` (dry-run shows "adopt" but doesn't reveal which step). |
| **MAJOR** | `main.rs:660-780` `IdentityContinue` duplicated `discover+pick` for BLE vs USB; fallback `if connected_usb.len()==1 { Ok(connected_usb[0]) }` silently accepts already-resolved mouse when no unassociated present — masks misconfiguration in persistent mode (legacy enrollment artifact). |
| **MAJOR** | `main.rs:900-980` `resolve_device_arg` tries `DeviceId::new` then `find_device_by_name`: display name `mouse-2` that canonical-parses but not in store falls through to name search and may return wrong entity or confusing "not found" (name `mouse-2` vs id `mouse-2` ambiguity). |
| **MAJOR** | `main.rs:980-1030` `resolve_hardware` (writes/battery/verify honour `--transport`) vs `resolve_state_device` (export/import/state ignore `--transport`) — inconsistent device-resolution model, `x3ctl use` vs `--device` semantics confuse. |
| **MINOR** | `main.rs:95-110` dry-run leaks internal `action_name` (`"set polling rate (BLE, not confirmed)"` vs user ladder "saves when you unplug") and doesn't validate `--device/--transport` relevance (dry-run succeeds even with nonsense `--device mouse-99`). |
| **MINOR** | `main.rs:420-500` `Devices` registers every discovered identity via `register_device`, even disconnected shells — intentional pruning hook but extra `mutate_async` write on read-only list (state touched on every `x3ctl devices`). |
| **MINOR** | `main.rs:240-280` `RateSetUnverifiedBle` flag accepted on any transport, error only at `update_polling_rate_unverified_ble` (rejects non-BLE late). Early validation would give usage hint; `validation/baseline` flags still accepted though overridden to `Transport`-only. |
| **MINOR** | `main.rs:1080-1120` `battery` no transport pre-check; wired `x3ctl battery --transport auto` gets late `ManagerError::UnsupportedOperation` rather than CLI usage hint ("battery is receiver-only"). |
| **MINOR** | `args.rs:280-380` `IdentityCommand::Adopt` doc "both starts and confirms" — bloat, mirrors code confusion. |
| **MINOR** | `main.rs:1300-1500` human formatters mostly `ui-copy.md` compliant, but `ManagerError::to_string` paths (MissingBaseline, ExplicitAuthorizationRequired) still surface blocked terms `readback/persistence/baseline` when error printed — `main.rs` dispatch `e.to_string()` leaks engineering vocab despite blocklist. |

### Bloat / redundant

* `pick_ceremony_endpoint` / `pick_ble_endpoint` near-identical filters over `DiscoveryView.connections` — could unify with predicate.
* `tests` (~400 ln in `main.rs`) extensive human-output hex-free assertions enforce blocklist — valuable but many overlap (`bind_set_human_for_parameterless_has_no_hex` × `bind_human_labels_are_readable` × `bind_get_human_shows_generic…`).
* `x3ctl/Cargo.toml` `tokio rt/time/macros` with `current_thread` — duplicates manager's own runtime construction (`Builder::new_current_thread`) in `main.rs:60-150`; second runtime never used concurrently but adds cold-start cost.

---

## 6. Docs / evidence / repo hygiene

| Sev | Location | Issue |
|---|---|---|
| **CRITICAL** | `AGENTS.md:90-150` still documents **schema 4** (mouse-N only, `link/rebind`/`rebind_missing_endpoint`, `SCHEMA_VERSION 4`, `LinkPrecedence`, `nextDeviceNumber` narrative) while code and `docs/README.md` are **schema 5** (mode+watermark+journal, no migration from 4, `identity adopt/associate`). Tests assert `link_and_rebind_commands_are_removed`. Doc/code drift is blocking for new contributors + agents. |
| **MAJOR** | `docs/ui-driver-spec.md` still scoped "FA61 Col04 wired-only, single collection auto-select" while implementation multi-transport `DeviceManager` with watermark identity, per-device locks, ceremonies — needs version bump. |
| **MAJOR** | `docs/README.md` correctly says schema 5 (nextDeviceNumber, per-device `device-<id>.lock`, no migration) but cross-ref to stale `AGENTS.md`. |
| **MAJOR** | Root untracked `after_*.json`, `before.json`, `backup.json` (7 files) left from manual `watermark_probe` / persistence probe runs — should be `gitignored` (`/.tmp/`, `*_probe.json` or move to `test-artifacts/`). They pollute `git status` and risk accidental commit of machine-local state paths. |
| **MAJOR** | `cargo clippy --workspace --all-targets --all-features -- -D warnings` **fails** (2 errors in `crates/attack-shark-x3/examples/macro-wired.rs:11` `useless_format` + `:145` `identity_op` `&0xFFFF`). CI (Linux) runs clippy `-D warnings` — branch would fail CI on `--all-targets` (examples included). Other crates pass; manager suite `222 passed`, full workspace tests pass (13 suites, 548 tests total), but clippy gate red. |
| **MINOR** | `docs/ui-copy.md` blocklist enforced in CLI human paths but `ManagerError` messages still contain `readback/persistence/baseline` (see above) — engineering vocab leak not covered by lint. |
| **MINOR** | `todo.md` "Documentation Inconsistencies" section still lists already-retracted `safety.md 0x05` note and stale `04-dpi.md` offset phrasing — duplicates `docs/research/corrections.md` but not reconciled. |
| **MINOR** | `.github/workflows/ci.yml` Linux runs `fmt+clippy+test --all-features`, Windows runs `check+test` only — intentional per `AGENTS.md` but now branch adds `examples/watermark_probe.rs` (725 ln) without `required-features` gate on Windows check? Gate is present (`required-features = ["usb"]`) so ok, but `macro-wired` gate missing? It does have `required-features`. |

---

## 7. Pre-existing issues (not introduced by multi-profile, still present)

* **`docs/protocols/05-preferences.md` formula** `((ms-4)/2)+2` vs `ms/2` — correctly marked resolved in `todo.md` but still echoed as "conflict" in comment at `preferences.rs:40`.
* **Report `0x07`/`0x09` unsupported** explicitly tested as `decoded:null` (`tests/input_codec.rs`) — correct guard, but `x3ctl debug` still offers no `unsupported-wakeup/macro` hint.
* **Polling-rate `0x06` preflight** 5-step safe path correct but decoupled from `VerificationMethod::Transport` note that BLE writes are ACK-only `Unknown` — todo still lists "semantic persistence barrier missing" as open, correct but user-facing status text says "Not yet confirmed to survive a restart" which matches.
* **Receiver PID `fa60` vs mouse `fa61`** model-id assignment still via VID/PID endpoint metadata only — no RF handshake trace doc (`docs/reverse-engineering-notes.md` still says unknown).
* **Workspace lints** `clippy::all = warn -1` + CI `-D warnings` — good, but `clippy.toml` not checked in.

---

## 8. Bloated / diffuse — whole-tree notes

* **Line counts** now `manager 5596→3579 net new`, `state/model 3206`, `worker 2991`, `x3ctl/main 2562` — total audited carrier ~17 k ln. Single-owner ceremony engine in `manager.rs` (one file) is the readability bottleneck. Splits proposed:
  * `manager/ceremony/{reconnected,stamp,associate,cancel}.rs`
  * `manager/discovery/{resolve,associate,prune}.rs`
  * `state/validation.rs` (extract `validate_identity_setup` 250 ln)
* **Clones everywhere** (see §2). `CapturedProfileImage` inside journal duplicates durable `DeviceState` — could be `Arc<ProfileState>` or delta vs baseline to bound 25 B×20 tail copies.
* **Two runtimes**: `x3ctl` builds `tokio::runtime::Builder::new_current_thread` + `x3-gui worker` builds its own current-thread runtime + `manager` tests use `rt-multi-thread` — mismatch not exercised but hides `spawn_blocking` assumptions. Manager's file I/O via `spawn_blocking` still uses `thread::sleep` — inconsistent model.
* **Tests redundant by construction**: manager state suite `222 passed` includes many evidence-predicate permutations; DPI codec unit tests duplicate `tests/dpi_codec.rs` golden fixtures (compact/full support assertions in both). Manager integration mocks `ScriptedFakeSession` polling map shared `Arc<Mutex>` unsynchronised across parallel tests (factory cloned) — pollution risk if `#[test(tokio::test)]` parallelised beyond default.
* **Examples bloat**: `watermark_probe.rs 725 ln` + `macro-wired.rs 263 ln` + `reset-macros.rs` + `profile-activate.rs` — useful offline tooling but no `cargo xtask` or `scripts/` index. Probe scripts `macro-probe.ps1/macro-collision-probe.ps1` already cover similar ground; ad-hoc overlap.

---

## 9. Prioritised fix order (do not fix yet — per audit request)

**P0 — before any persistent-mode dogfood:**
1. Fix `AGENTS.md` schema-4 drift + docs/ui-driver-spec stale — prevents agent + human mis-implementation.
2. Wire `StateStore` mutation validation on commit (or call `validate` inside every `mutate_async` closure returning `Ok` before write). Also validate `CapturedProfileImage` resource states.
3. Replace `thread::sleep` in per-device lock acquisition with `tokio::time::sleep` (or move lock acquire to `spawn_blocking`); fix Windows `fs2` stale `state.lock` PID-less handle contention.
4. Fix clippy `macro-wired` so CI green on `--all-targets`; add untracked probe outputs to `.gitignore`.

**P1 — correctness / UX before broader testing:**
5. Narrow watermark overlay guard (`overlay_physical_id` refuses to overwrite `Valid/UnsupportedVersion` tails unless caller is ceremony) or document as `unsafe`-ish API.
6. Collapse `IdentityAdopt` overload into two commands (`adopt begin` / `adopt confirm` or `adopt` vs `stamp`), remove `connected_usb.len()==1` fallback that masks misconfig.
7. Harden `resolve_device_arg` ambiguity (prefer exact `mouse-N` id over name when canonical), align `resolve_hardware` vs `resolve_state_device` model.
8. Fix manager cache trust / serialised watermark reads (batch opens concurrently with `join_all` + typed cache key via `TransportKind+locator` only, metadata check separate).

**P2 — bloat / tech-debt:**
9. Extract ceremony + validation + discovery from `manager.rs`/`model.rs`, dedup hex helpers, introduce `ProfileState::is_empty`, `Cow` / `Arc` for captured tails.
10. Unify GUI battery source (single `Battery` resource via `read_battery` + `DeviceEvent` debounced), fix `snapshot_preserves_draft` dirty guard, replace stringly transport check + int ceremony codes with typed enums.

---

## Supplement — 15-min deep perf + missed-correctness pass (2026-08-28)

### A. Perf — hot paths (measured / estimated on Ryzen 3550H)

Architecture for reference: single HID `mpsc::SyncSender 32` worker per device (blocking `hidapi` feature reports, `thread::sleep` on `write_delay` / readiness poll), manager on `tokio::current_thread` + `spawn_blocking` for all `state.json` I/O (`state.lock` fs2 + atomic `temp→rename` + `sync_all`), discovery via `nusb::watch_devices` + 2 s fallback poll + 750 ms settle, ceremony journal `mutate_async` per profile (full file serde + fsync), codecs allocate per-call (`Vec<DpiValue>`, `BTreeMap`, `[u8;56]` copies, `CRC32` per watermark).

| P | Location | Current cost | Fix sketch | Gain est. |
|---|---|---|---|---|
| **CRITICAL** | `manager.rs:184-260` `discover` serial watermark `read_endpoint_identity` (`for endpoint in to_read { await open+current_watermark }`) | `N*(open 5-20 ms + armed_read 0-250 ms poll@1/5 ms + dpi read)` → 2 eps = 160 ms wired / 1 s+ receiver serialised. + `cache.clone()` copies whole `BTreeMap<(TransportKind,DeviceLocator),(WatermarkDecode,DeviceEndpoint)>` per discover (locator holds `PathBuf`). | `FuturesUnordered` / `join_all` with cap 4, reuse single `spawn_blocking` for `list`, clone only needed entries (`Arc<DeviceEndpoint>`). | **−60 % discover latency** (wired 160→60 ms, receiver 1000→520 ms) |
| **CRITICAL** | `driver/worker.rs:310-500` `RECEIVER_READINESS_DELAY 500 ms` unconditional `thread::sleep` before first `read_readiness_status`, plus `write_delay 500 ms wired / 5 s receiver` via `thread::sleep` blocking the sole worker (queue 32 fills → `WorkerBusy`). `CaptureAllProfiles` = 20 armed_reads → 10 s receiver delay alone. | Worst `4*250 ms=1 s` wired / `4*2 s=8 s` receiver per logical read; ceremony 5 profiles = 5 writes blocked serially. | Make worker async (`tokio::time::sleep`, interrupt-notified readiness), cache readiness after first success, adaptive receiver `write_delay` (measure firmware quiet window, not flat 5 s). Pipelining / interleaving. | **−90 % tail** for multi-profile ops; frees worker to interleave reads |
| **CRITICAL** | `manager.rs:1931-2015` `ceremony_stamp` per-profile `write_dpi (send+500/5000 ms+arm_read)` + per-profile `mutate_async` (`clone StateFile`, `load`+2×serde, `to_vec_pretty`, temp PID loop 128 tries, `write_all+sync_all+rename+sync_parent`) | 5 profiles = 5 fsync +5 parses of entire `StateFile` (5*4 `ResourceState` per profile × 25 B tails). Wired ~3 s / receiver ~27 s ceremony. | Batch journal: single `mutate_async` after all stamps (or 1 per 2 profiles), `to_vec`+`BufWriter` not `pretty`, reuse temp fd, pipeline `write_dpi` without fsync. | **−70 % ceremony wall**, **−80 % IOPS** (wired 3→0.9 s) |
| **MAJOR** | `refresh.rs:70-200` `capture_all_profile_images` `once(current).chain(MIN..MAX filtered)` up to 5 activations, each `write_exact_metadata (send+sleep+read_metadata)` + `read_profile (4 armed_reads)` + `read_live_polling_rate` | Sequential even when `temporarily_expanded==false` still 5 switches (4 unnecessary); no buffer reuse, `BTreeMap` alloc per profile. | Skip activation when `original.maximum==MAX` and profile==current (already), combine `read_profile` single command vs 3 reads, pre-alloc `BTreeMap::with_capacity(5)`, reuse report buffer. | −30 % refresh, −1 alloc/stage |
| **MAJOR** | `state/store.rs:380-550` `load_async` 2-pass header→full deserialize + `StateFile` clone per call; `discover` does **3 `load_async` + 1 `mutate_async` = 4 parses**; `LockGuard::try_acquire` 5 ms `thread::sleep` poll (GUI 2 s poll + CLI contention → 500 ms before `DeviceOperationBusy`) | Per discover: 4× `serde_json::from_slice` of whole file (devices×5×4 resources) + `state==original` clone compare. Under contention spin 5 ms*100. | `try_lock_exclusive` + `Notify`/exponential backoff + `tokio::time::sleep` for async path; `Arc<RwLock<StateFile>>` + mtime check to avoid re-parse; coalesce discovers 3 loads→1. `BufWriter`+`to_vec` not `pretty`. | −50 % discover CPU, −2 clones/op |
| **MAJOR** | `state/store.rs:700-800` `write_atomic` `to_vec_pretty` (+20 % bloat) + PID+timestamp+counter temp loop 128 + `sync_all+rename+sync_parent` per `mutate_async` (`update_observed_*` single-field observed still pays full txn) | 2 `mutate_async` in `read_status` sequentially; each tiny field = full file txn. | Batch observed updates, single pretty→`to_vec`, fd reuse. | −30 % write latency, −15 % file size |
| **MAJOR** | `protocol/dpi.rs:300-650` `Vec<DpiValue>` heap per `DpiState`, `packet.to_vec()` 52/56 B per armed_read, `crc32fast::hash` per watermark decode, `to_watermark_bytes` 25 B copy per overlay, `into_evidence` `clone` 5× | ~20 `Vec` allocs per refresh. | `SmallVec<[DpiValue;8]>` / `ArrayVec<u8,56>`, zero-copy `Bytes`, reuse decode buffer, intern `WatermarkDecode`. | −40 % allocs on hot path |
| **MAJOR** | `x3-gui/worker.rs:294-430` `select!` biased + `topology_poll 2 s` + `HOTPLUG_SETTLE_DELAY 750 ms` per hotplug, `open_manager()` (construct `StateStore`+parse JSON) per `Refresh` even when manager exists, `pending` coalesces only Startup/Refresh | Idle 2 s poll → HID enumeration+lock churn forever; +750 ms to every plug/unplug perceived latency. | Adaptive poll: 2 s only if 0 connected else 10 s; hotplug debounce 150 ms trailing; reuse manager, reload only on mtime change. | −60 % idle wakeups, −750 ms latency |
| **MAJOR** | `x3ctl/src/main.rs:100-180` `Builder::new_current_thread().enable_all().build()` per invocation (5 ms even for offline `debug` early-return) + `StateStore::with_default_paths` env resolution per dispatch | Simple `devices` = 2 `spawn_blocking` + enumeration + 1-2 watermark reads, but pays runtime creation. | `enable_time()` only, lazy runtime only for async cmds, `Arc<StateStore>` shared. | −10 ms cold start (40→30 ms), −1 pool |
| **MINOR** | `manager.rs:41` `identity_cache` `cache.clone()` per discover copies `PathBuf` strings; key `BTreeMap` ordering does string compare O(log n·len) | n≤3 trivial but 3× clone per 750 ms poll. | `Arc<DeviceEndpoint>` value, drain pattern. | alloc/tooling noise |
| **MINOR** | `Cargo` resolver 3/edition 2024 + `tokio enable_all` (`mio/socket2/fs` unused by `current_thread`), `serde derive` ~200 KB proc-macro per 34 types | 4 `serde_json` copies via dev-deps, `slint` dominates | Trim `tokio { features=[sync,time,rt] }`, `workspace.dependencies` dedup, `slint` default-features pruning. | −15 % compile, −300 KB binary |

Notes: `perf/supplemental` scouts unified; `store.rs:320-410` `thread::sleep(5ms)` is both correctness (blocks Tokio pool) and perf (spin) — counted in P0 fix list.

### B. Missed correctness — new true positives

| Sev | Location | Issue |
|---|---|---|
| **MAJOR** | `manager.rs:54-55,1585-1587,1801,1997,2030` `skip_migration: AtomicBool` | In-memory only, not in `IdentitySetupJournal`. User `identity accept-migration / skip-migration` choice lost on power-loss/process restart mid-ceremony; resume `finalize_ceremony` defaults `false` so enrollment migration re-enables even if explicitly skipped. Journal should carry `bool` or be re-prompted; `ceremony_progress_from_journal` has no migration flag. |
| **MAJOR** | `x3ctl/src/main.rs:590-632,1016-1022,1716-1782` `identity continue` | `--device` (global) is only ceremony *target* (`resolve_ceremony_target` for Restore/BLE) but `pick_ceremony_endpoint`/`pick_ble_endpoint` ignore it when picking the *presented* USB/BLE endpoint. `x3ctl identity continue --device mouse-2` with two unassociated USB mice picks the sole Unassociated (or ambiguous error) rather than filtering to `mouse-2`'s locator. BLE path correctly requires `--device`, USB silently ignores — docs don't clarify precedence. |
| **MINOR** | `x3-gui/worker.rs:310,379-385,1112-1181,1569-1585` topology/ceremony staleness | `visible_topology` refreshed only on `Startup/Refresh/AutoRefresh` discover. `BatteryChanged` events and `CeremonyAction` success don't refresh topology; `nusb` watcher + 2 s poll fallback can leave stale until manual Refresh. `is_coalescable_command` can coalesce a needed rediscovery before `Stamp`. No data loss (journal durable) but UI can show stale “awaiting reconnect”. Minimize doesn't lose `ceremony_active` but same coalesce gap. |
| **MINOR** | `app_settings.rs:165-310` `gui-preferences.json` | `schemaVersion 1` only: any `!=1` triggers `reset_gui_preferences` (backup+defaults), destructive not migratory. Intentional per spec (no `state.lock`, backup on unreadable) but future v2 wipes user prefs. Track as TODO for v2 migration. |
| **MINOR** | `manager.rs:1835-1925,2199-2224` `mint_unique_token` | `getrandom::fill` failure → `ManagerError::TokenGeneration`, aborts `Stamp`, leaves journal `Stamping token=None`; resume re-mints (safe, idempotent). `getrandom` retries `EINTR` internally, 16-collision loop is 2^128-rare. Not a bug, no auto-retry needed — surfaces as explicit “retry/cancel”. |

### C. False-positive corrections (prior flags that are actually safe)

| Prior flag | Correction |
|---|---|
| DPI ratio vs `DpiValue` step 50 drift | **Not a bug.** Single source `DpiValue::MIN 50 MAX 26000 STEP 50`; `presentation::round_dpi_step/dpi_bounds/clamp_dpi_value` and `app_settings::dpi_bounds` both delegate to `STEP`, `projection::dpi_ratio` uses normalised min/max; manual `parse_dpi_setting→round_dpi_step` ensures no off-step value reaches hardware. |
| `is_fully_stamped` infinite loop / power-loss mid-stamp | **Not a bug; resumable.** `is_fully_stamped` exact key equality + `validate_identity_setup` strict. `ceremony_reconnected` persists token+captured before any write; `ceremony_stamp` mints token in separate `mutate_async`, persists each non-last profile immediately, defers last to finalizing txn. Power-loss after last HW write but before final txn leaves HW stamped, journal not fully stamped → resume re-writes idempotently (verified by readback). |
| `next_device_number` overflow/wrap | **Safe.** `allocate_device_id` `checked_add(1)` → `StateError "nextDeviceNumber overflow"` on `u64::MAX`; `validate` rejects `0` and requires `next > max` device number; `import_configuration` also `checked_add`. |
| BLE association with no Wired endpoint in persistent mode | **By design.** Persistent mode needs every `DeviceState` `physical_id` unique, but BLE association attaches `BlePlatformId` to existing mouse (already USB-enrolled). Discovery authenticates only Wired/Receiver via `current_watermark`; BLE never verified (spec §13). Pure-BLE resolve is locator-match, not watermark. |
| `SessionWrite` vs `WriteOutcome` drift | **Not drift.** `SessionWrite::Acknowledged (BLE/Transport) → record_ack (Ack/Unknown, observed None)`; `ReadbackVerified → record_readback`. `UpdatePolicy` forces BLE to `Stored`+`Transport`, so BLE never claims `Readback`. `ProfileUpdateOutcome` coalesces via `persist_write` once per resource; polling_rate isolated. |
| “Fixture watermark golden missing = breakage” | **Coverage ok.** `fixtures/protocol/input.json` + `dpi.json` evidence-labeled golden (battery/input) with `decoded:null` for `0x07/0x09`; watermark covered by unit tests + live `watermark_probe` captures (dual-surface/faux-profile probes). No breakage. |

### D. Still-open confirmed gaps (overlap with §2-3, restated)

* `CapturedProfileImage` resource evidence never validated (`validate_identity_setup` checks `stamp_progress` tracks keys but not `profile_metadata`/`profiles[*].{dpi,prefs,buttons,polling_rate}` `validate_resource_state`). Overlaps CRITICAL in §2 — remains P0.
* `AGENTS.md` schema-4 drift vs `SCHEMA_VERSION 5` + removed `link/rebind` — remains P0, blocks contributors/agents.

### E. Perf / compile — extra bloat findings (beyond §8)

| Sev | Location | Detail |
|---|---|---|
| **HIGH** | `Cargo.toml:1-25` resolver 3 + edition 2024 | 4 members sharing `attack-shark-x3` path dep force full feature unification + re-resolve on any lock churn; no `[profile.release] strip/lto/codegen-units/panic=abort` → x3ctl/x3-gui ~8-15 MB unstripped. Add `strip="debuginfo" lto="thin" codegen-units=1` → −30-40 % size, +10-15 % cold compile; consider edition 2021+resolver 2 unless let-chains needed. |
| **HIGH** | `x3-gui/Cargo.toml` `slint 1.17.1 renderer-femtovg + backend-winit` | ~180 crates, dominates `cargo build` wall (>60 %); `compat-1-2` unused unless `.slint` uses old syntax; `[profile.dev.package.*] opt-level 1/2` doubles dev compile (4→8 s Ryzen3550H). Gate `renderer-femtovg` behind feature, move `opt-level=2` to `profile.dev.build-override` only. |
| **MEDIUM** | `attack-shark-x3-manager/Cargo.toml vs x3ctl/Cargo.toml` tokio features | Manager prod `tokio {rt,sync,time}` without `rt-multi-thread` yet `mutate_async` uses `spawn_blocking` (needs pool); `x3ctl` prod `tokio {macros,rt,time}` missing `sync`/`rt-multi-thread` but uses `StateStore` flock+`spawn_blocking` — compiles but `try_mutate_async` panics on pool-less runtime; CI `cargo check --all-features` masks. Unify `workspace.dependencies.tokio = {version="1",features=[rt,rt-multi-thread,macros,sync,time]}`. |
| **HIGH** | `.github/workflows/ci.yml` | No `Swatinem/rust-cache@v2`/`sccache`; 2 OS jobs reinstall toolchain 1.97.1 (vs rust-version 1.92) + 6 cargo invocations (`fmt+clippy+test --all-features + test --workspace + 2× check --no-default-features`), recompiles x3-gui femtovg twice. Cold ~6-8 min vs ~90 s cached. Add cache, split jobs, `cargo hack` dedup, pin toolchain to `Cargo.toml` version. |
| **MEDIUM** | `serde` derive bloat | ~34 types `#[serde(rename_all="camelCase")]` monomorphise `Deserialize` (~25 KB .text/type); `deny_unknown_fields` on leaf types adds branch per field without catching typos (only top-level matters). Keep `deny_unknown_fields` on `StateFile`/`ConfigurationExport` only, alias elsewhere. |
| **LOW** | `x3-gui/build.rs` | `slint_build::compile` recompiles whole `ui/app-window.slint` on any `Cargo.toml` touch; add `cargo:rerun-if-changed=ui/app-window.slint` + split UI modules → −1.2 s incremental. |
| **MEDIUM** | Tests wall | `tests/dpi_codec.rs` iterates `dpi.json` twice + per-framing loops; `offline_debug.rs` repeats tail/checksum asserts with hardcoded 52 B vectors. Wall 1.2 s fixture deserialization per binary. Collapse `encode_debug_*` aliases (6 identical fns), share `assert_fixture_roundtrip`, `#[rstest]` per-fixture for parallelism. `ScriptedFakeSession` 11× `Arc<Mutex<T>>` + `BTreeMap<String,>` key formatting per call → 2-level locking, ~2-3 s of 8-13 s suite in lock contention. Single `Arc<Mutex<SessionState>>` + `DeviceLocator` as key. Manager inline helpers (`wired_endpoint_named` etc.) copied 5 times → extract `test_support` crate, `#[rstest]` matrix. |

### F. Revised priority (perf folded in)

P0 unchanged (validation on commit, `thread::sleep→tokio::sleep`, `AGENTS.md`, clippy). New P1 perf neighbors: parallel watermark reads, batched `ceremony_stamp` fsync, `CaptureAllProfiles`/`StateStore` load coalesce, GUI poll adaptive, Tokio feature unify. See table A for ordering.

