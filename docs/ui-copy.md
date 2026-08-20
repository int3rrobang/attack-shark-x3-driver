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
| no write was attempted / no retry was attempted | (drop) or "Nothing was changed." |

## Blocklist

The following words/strings MUST NOT appear in normal user-facing copy
(status bars, dialogs, section text, CLI success/error prose). They are fine in
`docs/`, code comments, and the `--output json` envelope, and in technical
debug output (`x3ctl debug ...`), which is intentionally a raw surface:

`readback`, `persistence` (use "survives restart"), `baseline`, `observed`,
`desired`, `drift`, `evidence`, `preflight`, `submission`, `schema`,
`unverified`, `"no ... was attempted"`, `"definitive proof"`, `report 0x06`,
`ACK`, `packet`.

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

## Maintaining this guide

If you introduce a new user-facing string and it does not fit a table row, add
a row. If you need a term not covered, prefer a plain English result over
engineering vocabulary. Keep the confidence ladder in sync with
`ApplicationVerification` / `PersistenceVerification` in the manager.
