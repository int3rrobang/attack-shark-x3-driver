# Packet protocol reference

These documents are organized by report ID rather than by product name. Each report page identifies the supported model dialects, transports, evidence, and known implementation gaps.

| Report | Purpose | X11 USB | X3 USB | X3 BLE |
|:-------|:--------|:--------|:-------|:-------|
| [`0x04`](04-dpi.md) | DPI and sensor settings (1–8 configurable stages; physical DPI button is a separate auxiliary input `03 00 10 <stage> 00` with six positions observed in captures) | Supported | Live/capture-confirmed; Rust wired and FA60 receiver paths; USB readback via armed `0xa0` selector | Live-confirmed |
| [`0x05`](05-preferences.md) | Preferences, sleep, debounce, lighting fields | Supported | Rust wired and FA60 receiver paths; USB readback via armed `0xa0` selector | Live-confirmed with restrictions |
| [`0x06`](06-polling-rate.md) | Polling rate | Supported | Live-confirmed; Rust wired/FA60 receiver read/write paths; exact BLE packet cross-transport effect confirmed | Stock app skips; shorter BLE packet forms rejected |
| [`0x07`](07-wakeup-mode.md) | Wakeup mode | Unknown | Format known; behavior untested — explicitly unsupported, not exposed by Rust driver/CLI/manager; no fixture — tested as ignored/rejected (`fixtures/protocol/input.json` `unsupported-wakeup-0708` `decoded:null` + `tests/input_codec.rs::unknown_and_unsupported_reports_remain_unsupported`) | Untested — not exposed; same `decoded:null` rejection |
| [`0x08`](08-button-mapping.md) | Button mapping | Supported | Rust wired and FA60 receiver paths; USB readback via armed `0xa0` selector | Checksum live-confirmed |
| [`0x09`](09-custom-macros.md) | Custom macro pages | Supported | Live-confirmed; Rust driver/manager/CLI not exposed — parser acceptance only; explicitly unsupported, no fixture — tested as ignored/rejected (`fixtures/protocol/input.json` `unsupported-macro-0900` `decoded:null` + `tests/input_codec.rs::unknown_and_unsupported_reports_remain_unsupported`) | Parser acceptance only; not exposed |
| [`0x0b`](0b-version.md) | Version and profile state composite | — | Live-confirmed; wired and FA60 receiver paths; byte [4] mirrors `0x05` light mode | No useful response |
| [`0x0c`](0c-profile-reset.md) | Profile load/reset actions | Supported | Capture/live-confirmed; Rust wired and FA60 receiver paths | Parser acceptance; persistence incomplete |
| [Battery](battery.md) | Battery status | Legacy `03 55 40 01 <pct>` 0–100 | X3/M600 FA60 `03 10 40 01 <level>` level 1–10 ×10 = percentage; confirmed by X3.exe disassembly + capture 2026-07-24 | Standard BLE Battery Service `0x180f`/`0x2a19` |

Targeted reads for `0x04`/`0x05`/`0x08` carry the one-based profile in byte 2 (and selector byte 4 of the `0xa0` read) and load that target's working buffers, which can change live mouse behavior without necessarily changing persistent `0x0c` current metadata. Report `0x06` skips that loader: byte 2 is a save alias, the deferred writer serializes the complete *live* image into that slot, and a `0x06` read is a live-rate read (byte 2 is a wire-shape check, not a content proof).

“Parser acceptance” means the firmware acknowledged a packet; it does not by itself prove application or persistence. BLE ACK `10 50 00 <report>` is parser acceptance only, never a readback or persistence proof.

### Coverage and fixture gaps

Golden fixtures live in `fixtures/protocol/` and cover `dpi.json`, `preferences.json`, `buttons.json`, `profile.json`, and `input.json` (evidence-labeled, with `decoded:null` for `0x07`/`0x09` unsupported). Checksums have boundary/wrapping integration via `tests/checksum_codec.rs`. Additional implementation coverage exists outside golden fixtures; unsupported scope (`0x07`/`0x09`) remains explicitly unsupported and tested as ignored/rejected, not implemented.

| Area | Golden fixture (`fixtures/protocol/*.json`) | Unit / integration coverage | Notes |
|:-----|:--------------------------------------------|:----------------------------|:------|
| `0x04` DPI, `0x05` prefs, `0x08` buttons, `0x0c` profile | `dpi.json`, `preferences.json`, `buttons.json`, `profile.json` — decode/encode round-trip | Codec unit tests plus manager resource tests | — |
| `0x06` polling rate | No fixture | Driver `read_live_polling_rate` and manager `apply_profile_update` tests; live-alias side-effect verified via `ScriptedFakeSession::last_polling_alias` (`live_polling_alias_is_side_effect_not_content`); no golden byte vector | `0x06` byte 2 is save alias, not load target; live-rate is profile-scoped only after `read_profile(target)` |
| Input events `0x03` family | `input.json` — evidence-labeled golden vectors (`disassembly+live-confirmed`/`live-confirmed`/`implementation`) | `tests/input_codec.rs` golden decode (`golden_fixtures_decode_as_labeled`) plus battery/malformed/unsupported tests (`x3_battery_*`, `malformed_*`, `unknown_and_unsupported_reports_remain_unsupported` proves `0x07`/`0x09` ignored) | Includes `x3-battery` 1–10 contiguous, DPI-button, profile-sync, malformed lengths, `unsupported-wakeup/macro` rejected |
| 16-bit checksums (`0x05`/`0x08`) | No golden JSON fixture — boundary/wrapping integration via `tests/checksum_codec.rs` | Wrapping `sum16` boundary tests: empty/zero, single-byte, carry to high byte, `0xffff` max, `0x10000` wrap, `0x01fe`/`0x0101` carry, known-DPI-slice mimic; deterministic/order-independent | Live ACK tables prove correct vs legacy; no golden byte vector by design |
| Checked `serde` / state schema 5 | No protocol fixture | `state::model` + `device` tests reject non-canonical `mouse-N`, incoherent `DeviceEndpoint`, unknown `schemaVersion`; `app_settings` schema 1 tested with backup on unreadable | Schema 5 has no migration from schema 4; schema-4 documents are rejected |
| `0x07` wakeup mode, `0x09` macros | No fixture — **explicitly unsupported** | Format known from X3.exe static analysis/captures; no exposed Rust driver/manager/`x3ctl`/GUI API, no claim of device effect | Explicitly tested as ignored/rejected: `fixtures/protocol/input.json` `unsupported-wakeup-0708`/`unsupported-macro-0900` (`decoded:null`) and `tests/input_codec.rs::unknown_and_unsupported_reports_remain_unsupported`; not implemented |
| Battery / `0x0b` version | No fixture | Battery `1–10 ×10%` live-confirmed; `0x0b` byte [4] mirrors light mode | — |

`0x07` and `0x09` remain explicitly unsupported in the Rust driver/manager/`x3ctl`/GUI surface (see their report pages) — format known, no API, tested as ignored/rejected (`decoded:null`), not implemented. Human-friendly labels are used in normal CLI output; raw protocol bytes remain in `--output json`, `debug` commands, and `x3ctl --dry-run` hex.
