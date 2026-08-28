# UI Copy Guide

The authoritative voice for **every user-facing string** in this repository:
`x3-gui` (`ui/app-window.slint`, `src/main.rs`), `x3ctl` human output, and any
`ManagerError` message that can surface to a user. **Read this file and apply
it whenever you add or edit user-facing text.** The rest of the codebase
(comments, `docs/`, internal types) keeps the precise engineering vocabulary;
this guide governs only what a user reads on screen or in a terminal.

The goal is an app a casual user can use without a manual, while keeping every
control, option, and honest constraint. We rewrite the **words**, never the
**features**. Nothing is removed or hidden; nothing is claimed that is not true.

## Voice principles

1. **State the outcome, not the absence of effort.**
   Prefer "Applied." / "Saved." / "Nothing was changed." Never "no write was
   attempted" or "no retry was attempted". If nothing happened, say so plainly,
   or drop the clause entirely — it is implied by the failure.

2. **Turn negated caveats into a confidence status.**
   Instead of "readback is not definitive proof of persistence", show a short
   status (see the ladder below) and move the explanation to a tooltip or the
   Advanced page. Don't wallpaper every screen in disclaimers.

3. **One idea per line.** No run-on em-dash caveats in the status bar or in a
   dialog. A long explanation belongs in a tooltip or the Advanced page.

4. **Don't name the mechanism; name the result.**
   "readback" is plumbing. "confirmed by the mouse" is a result. Prefer verb
   phrases over nouns for internal operations.

5. **Write like the app, not like a log.** Match the voice already used in
   good strings: "Choose what each physical button does." Keep case consistent
   (labels are lowercase; instructions are full sentences).

6. **Keep all honest constraints.** Persistence really is unverified until the
   power-cycle check runs. We don't claim otherwise — we present the truth as a
   calm status instead of a lecture.

## Confidence ladder

Use these tiers for anything about whether a setting stuck. They map exactly to
the real evidence levels, in increasing strength:

| Tier | User-facing | Engineering meaning |
|:-----|:------------|:--------------------|
| Applied | **applied** / **applied (device acknowledged)** | `ApplicationVerification::Acknowledged` |
| Confirmed | **confirmed by the mouse** | `ApplicationVerification::ReadbackVerified` |
| Survives switch | **survives switching profiles** | `PersistenceVerification::ProfileReloadVerified` |
| Survives power-off | **survives a full power-off** | `PersistenceVerification::PowerCycleVerified` |

When a stronger tier is not yet proven, say **"not yet confirmed"** — never
"unverified", "no evidence", or "is not definitive proof of ...".

## Vocabulary table

| Internal / jargon (do NOT use) | User-facing replacement |
|:-------------------------------|:------------------------|
| readback, readback-verified | confirmed by the mouse |
| live readback loaded | values read from the mouse |
| persistence | survives restart / power-off |
| persistence unverified | not yet confirmed after restart |
| power-cycle persistence verified | confirmed to survive a full power-off |
| profile-reload persistence verified | confirmed to survive switching profiles |
| writes submitted | applied |
| transport submission / transport-verified | applied (device acknowledged) |
| no readback (BLE) | the mouse won't confirm this |
| baseline | stored copy of your settings |
| desired values | your settings / the values you chose |
| observed / observations | read from the device / readings |
| drift | differences between your saved settings and the device |
| evidence / no evidence | confirmation / not yet confirmed |
| preflight | safety check |
| verification workflow | the check / **Verify** (button verb) |
| invalidate persistence evidence / state | reset saved confirmation / clear saved status |
| schema version / state file | local data |
| packet / byte / ACK / report `0x06` | (drop; describe the effect) |
| protocol error / device-operation lock timeout | that change isn't valid / the mouse is busy; try again |
| watermark / physical token / physical id | this mouse's identity tag |
| unassociated connection | this mouse isn't recognized |
| ceremony / enrollment / journal | setup |
| adopt / adoption | add as a different mouse |
| restore (rotate to a fresh token) | set this mouse up again (as `<name>`) |
| BLE association | connect over Bluetooth |

## Blocklist

The following words/strings MUST NOT appear in normal user-facing copy
(status bars, dialogs, section text, CLI success/error prose). They are fine in
`docs/`, code comments, and the `--output json` envelope, and in technical
debug output (`x3ctl debug ...`), which is intentionally a raw surface:

`readback`, `persistence` (use "survives restart"), `baseline`, `observed`,
`desired`, `drift`, `evidence`, `preflight`, `submission`, `schema`,
`unverified`, `"no ... was attempted"`, `"definitive proof"`, `report 0x06`,
`ACK`, `packet`, `byte`.

Copy tests apply this blocklist only to strings shown in normal UI or human
output. Do not run it over `Debug` values, raw diagnostic fields, JSON, code
comments, or test fixture labels; those surfaces intentionally retain exact
engineering terms.

The buttons **Verify profile reload** / **Verify power cycle** and the card
title **save verification** are borderline: keep the "Verify" verb, but prefer
plain descriptions around it (e.g. "Reload the profile to check saved
settings", "Disconnect and reconnect the mouse to check it survives a power
off").

## Tooltips

When a fact is true but noisy (e.g. "confirmed now, but not yet proven to
survive a power-off"), show a compact status inline and put the explanation in
a `Tooltip { text: "..." }` on that element. Users who care get the detail;
nobody gets lectured in 9px text.

## Physical identity ceremonies

Ceremonies establish *which physical mouse is which* — the one place identity
is allowed to change. The reconnect gesture is the authentication: copy says
what to do with the mouse, never asks the user to reason about logical state.
Never ask "Is this your usual mouse?" or "Is this Mouse 1?" — those require
internal knowledge or fuzzy judgment.

### Entry copy

| Ceremony | User-facing entry | When it appears |
|:---------|:------------------|:----------------|
| First-time enrollment | **Add another mouse** → "You'll need both mice nearby." | No physical identity yet |
| Add another mouse | **Add another mouse** → "Reconnect the mouse you want to add." | Persistent identity already set up |
| Restore | **Restore a saved mouse** | "This mouse isn't recognized." and the user picks a saved mouse |
| Adopt | **Add as a different mouse** | A valid identity tag unknown to this installation |
| BLE associate | **Connect over Bluetooth** | Attaching a Bluetooth connection to a logical mouse |

### Ceremony flow copy

- **"You'll need both mice nearby."** — only for the first-time transition.
  Say exactly this; do not ask which mouse is which.
- **"Reconnect the mouse you're adding."** then **"Now reconnect your other
  mouse."** — the two reconnect gestures are what identify the mice.
- **"Reconnect the mouse you want to add."** — for every later mouse; earlier
  mice do not need to be present.
- **"This mouse is already added."** — a known identity tag reappeared.
- **"This mouse isn't recognized."** — an unassociated connection. Present it
  as a distinct object without a name or number yet, and offer
  **Restore a saved mouse** / **Add as a different mouse**.
- **Restore** is a user assertion — no reading can prove which former unit
  this is. Ask for confirmation and a reconnect: "If this is <name>, reconnect
  it and we'll set it up again." The name is kept; nothing else is claimed.
- **Adopt** always requires explicit confirmation, never a one-click default:
  "This mouse has an identity tag from another computer. Add it here?" It
  becomes a fresh mouse with a fresh name; the tag is never replaced.
- **BLE** cannot confirm identity. "Connect over Bluetooth" only associates
  the connection; to verify which physical unit it is, say plainly: "To
  confirm which mouse this is, connect it with a cable or receiver."

### Progress and the confidence ladder

Ceremony progress is a step count, not a spinner ("Mouse 1 of 2"). Map stages
to results and keep the ladder language:

| Stage | User-facing |
|:------|:------------|
| AwaitingReconnect | "Reconnect the mouse." |
| Capturing | "Reading the mouse." |
| Stamping | "Setting up this mouse." |
| Verified | "Confirmed by the mouse." |
| Complete | "Done." |
| Cancelled | "Cancelled." |
| Failed | "Something went wrong." (detail in a tooltip or diagnostic surface) |

A stamped identity is confirmed by readback — the "confirmed by the mouse"
tier, not "survives a full power-off". Until that confirmation has run, say
"not yet confirmed", never "unverified".

## Maintaining this guide

If you introduce a new user-facing string and it does not fit a table row, add
a row. If you need a term not covered, prefer a plain English result over
engineering vocabulary. Keep the confidence ladder in sync with
`ApplicationVerification` / `PersistenceVerification` in the manager.
