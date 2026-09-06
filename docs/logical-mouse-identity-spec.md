# Mouse identity / multi-mouse model

## 1. Core concepts

There are four different things and they must remain separate:

```text
Physical mouse
    ↕ persistent watermark, once identity mode exists
Logical mouse
    ↕ one or more current transport endpoints
Endpoint
Unassociated connection
```

### Physical mouse

An actual piece of hardware.

Once persistent identity has been initialized, it is identified by a **driver-owned watermark stored in the DPI `0x04` opaque tail**.

The watermark is an opaque random token. It is not `mouse-1`, the display name, USB path, PID, etc.

Example:

```text
physical token = 8f7c2a...
```

### Logical mouse

The durable application-side object representing one physical mouse.

Conceptually:

```rust
LogicalMouse {
    id: DeviceId,                    // mouse-1
    name: Option<String>,            // "Desk mouse"
    physical_id: Option<PhysicalId>,  // absent before physical identity setup
    endpoints: ...,
    profile/config state: ...,
}
```

The user-facing name belongs here.

`mouse-1` is an internal stable logical identifier; the GUI should generally show the logical mouse's name instead.

### Endpoint

A currently usable transport/address:

```text
FA61 wired HID path
FA60 receiver HID path
BLE platform ID
```

An endpoint is **never physical identity**.

One physical mouse may expose multiple endpoints simultaneously, e.g.:

```text
same physical mouse
├── FA61 wired
└── FA60 receiver
```

Therefore:

> Number of endpoints != number of physical mice.

Do not infer physical mouse count from connected endpoint count.

### Unassociated connection

A discovered compatible endpoint that cannot yet be safely attached to a logical mouse:

```text
valid but locally unknown watermark
missing or malformed watermark, in persistent mode
BLE endpoint awaiting explicit association
duplicate-token conflict
unsupported future watermark version
```

An unassociated connection is presented in the UI as a distinct object, but it must **not** receive a temporary `mouse-N`: discovery must never manufacture logical identities before identity has been established. Only an association decision (a watermark match or an explicit physical ceremony) creates or attaches a logical mouse, and unassociated connections are **never persisted as logical devices**.

---

# 2. Default single-mouse semantics

Before the user has initialized persistent physical identity, the application intentionally uses the simple legacy model:

```text
mouse-1 = "the compatible mouse I use"
```

There is no claim that `mouse-1` refers to a particular physical unit.

Legacy mode has exactly **one durable fuzzy logical mouse**. Any number of compatible connections may be displayed beneath it; a USB port change simply changes the available connection and does not create another logical mouse. If several physical mice are present, the app makes no claim about which physical unit is behind any given connection.

Important consequences:

* no watermark is required;
* no watermark should be written;
* no watermark read is added on startup;
* FA60 users do not pay the ~500 ms armed-read cursor freeze merely for identity;
* if the user silently replaces their only mouse with another identical supported mouse, the application may treat it as the same logical mouse.

That ambiguity is acceptable because persistent physical distinction has not been requested.

### Critical invariant

**Do not opportunistically watermark single-mouse devices.**

Otherwise this can happen:

```text
A previously represented mouse-1
        ↓
user silently substitutes physical B
        ↓
driver assumes B == mouse-1
        ↓
normal DPI write overlays token A
        ↓
B has now been permanently misidentified as A
```

A watermark is only useful if we know we're stamping the correct physical mouse.

Therefore:

> No physical identity token may be written until the physical/logical association has been explicitly established.

---

# 3. Entering persistent identity mode

There does not need to be a technical-looking **Multi-Mouse Mode** toggle.

The user-facing action should be:

> **Add another mouse**

The first time this is used, the installation transitions from:

```text
one fuzzy logical mouse
```

to:

```text
physically identified logical mice
```

This first transition is special because the existing `mouse-1` state is not yet physically bound to anything.

## Initial setup UX

Before beginning, explicitly tell the user:

> **You'll need both mice nearby to set this up.**

Do not ask questions such as:

* "Is this your usual mouse?"
* "Is this Mouse 1?"
* "Which logical mouse is this?"

Those require the user to understand internal state or make fuzzy judgments.

Instead use **physical actions as authentication**.

Suggested ceremony:

```text
Add another mouse
        ↓
"You'll need both mice nearby."
        ↓
Step 1:
"Reconnect the mouse you're adding."
        ↓
capture / identify / watermark this physical mouse
        ↓
Step 2:
"Now reconnect your other mouse."
        ↓
capture / identify / watermark this physical mouse
        ↓
setup complete
```

The reconnect gesture supplies the semantics. The program does not guess from paths, configuration similarity, endpoint count, or model.

Each mouse must be presented over **wired or receiver USB**; BLE alone cannot enroll or verify a watermark (see §13).

The two mice do not need to be simultaneously connected; the user merely needs both available.

---

# 4. Why initial setup captures both mice

During this first transition, avoid guessing which current/stored device configuration belongs to which physical mouse.

It is acceptable for this setup to be deliberately invasive because:

* it happens once;
* the user explicitly requested multi-mouse setup;
* correctness is more important here than minimizing reads/writes.

Use the existing full-profile capture mechanism or equivalent to establish the actual state of each physical mouse.

`refresh_all_profiles()`-style behavior is acceptable here:

```text
temporarily expose available firmware profile slots
capture complete state
restore original metadata exactly
```

This is not being used to fingerprint the mouse.

It is only used so that:

> while the user has explicitly authenticated "this physical mouse", we capture *this physical mouse's actual state* instead of guessing from old local state.

---

# 5. Initial-state/name migration

The existing single logical mouse may have local configuration evidence and possibly a logical-device name.

Do not build fuzzy configuration matching to assign cosmetic metadata.

A simple migration rule is enough.

If the old single-mouse state has a **unique exact match** to one of the captured physical mice:

```text
old state == physical A
old state != physical B
```

then A may inherit the old logical name/state where appropriate.

Likewise for B.

If:

```text
both match
neither matches
comparison is incomplete
```

do not guess.

Use fresh logical-device names/defaults.

For example:

```text
Mouse 1
Mouse 2
```

The user can rename them afterward.

### Do not implement

Weighted matching such as:

```text
DPI match      +4
buttons match  +3
polling match  +2
```

is specifically not worth the complexity merely to preserve a label.

Most importantly:

> Configuration/name similarity must never establish physical identity.

Only the watermark or explicit user enrollment does that.

---

# 6. Watermark storage

Preferred storage is the opaque tail in DPI report `0x04`.

There are 25 writable bytes at offsets 25–49.

Live experiments established that the candidate tail:

* survives power cycle;
* survives firmware `0x0c` profile-slot load;
* survives button `0x08` changes;
* survives unrelated preference `0x05` changes;
* is overwritten by external/full `0x04` rewrites;
* is overwritten by the stock software's virtual-profile switching.

That is acceptable.

The watermark format should contain at minimum:

```text
magic
format version
random device token
integrity/check value
```

The format fills the entire 25-byte surface exactly:

```text
bytes  0..=3   magic: "X3ID"
byte       4   format version: 1
bytes  5..=20  random 128-bit token (16 bytes)
bytes 21..=24  CRC-32 over bytes 0..=20
```

The token is generated from the operating system RNG; it has no relationship to `mouse-N`, names, paths, or installations.

The codec lives in the protocol crate. It exports an opaque `PhysicalId` value type plus a strict watermark decode status usable by the manager and state layers:

```text
Valid
Absent
Malformed
UnsupportedVersion
```

Decoding is all-or-nothing: wrong magic, unsupported version, or bad CRC means **no valid identity**, never a best-effort token. An unsupported future version remains distinguishable from absent/malformed so an older build refuses to overwrite a marker written by a newer format instead of treating it as unmarked.

The CRC is corruption detection, not authentication. The outer `0x04` packet checksum is recalculated normally after the watermark is overlaid.

---

# 7. Durable state model and schema

The on-disk state schema becomes **5**, with **no migration from schema 4**: schema 4 is pre-release and unreleased, so a schema-4 document is rejected and the state is recreated fresh (recover anything needed with the older build first). Public Rust types derive `Debug` and `serde` per crate convention.

```rust
enum IdentityMode {
    Legacy,      // exactly one fuzzy logical mouse, no physical IDs
    Persistent,  // physical_id-bearing logical mice
}

struct DeviceIdentity {
    id: DeviceId,
    display_name: Option<String>,
    physical_id: Option<PhysicalId>,  // absent before physical identity setup
    endpoints: ...,
}
```

The `StateFile` additionally exposes:

```rust
identity_mode: IdentityMode,
identity_setup: Option<IdentitySetup>,
```

`IdentitySetup` is the durable, resumable identity-setup journal. Before every irreversible hardware write it persists: the current phase, the captures so far, the proposed token(s), and per-profile stamp progress. If the app exits mid-ceremony, startup resumes from the journal; it never silently returns to legacy mode and never generates replacement tokens.

Validation invariants:

* legacy mode has exactly one fuzzy logical mouse and no physical IDs;
* persistent logical mice have unique physical IDs;
* no token appears under two logical mice;
* unassociated connections are never persisted as logical devices;
* a pending `IdentitySetup` contains exactly the captures, tokens, and progress its phase requires;
* completed persistent mode cannot contain a half-enrolled logical mouse.

There is **no tombstone list**. Replaced or forgotten tokens are simply removed from the local token index; a returning old token is a valid-but-unknown physical identity that may be adopted again after confirmation.

---

# 8. Stock software compatibility

The stock app is not a first-class concurrent configuration authority.

It maintains its own virtual profiles and overwrites the mouse's firmware state when switching them.

Testing established that stock profile switching destroys both candidate watermark surfaces.

Therefore:

> Using stock configuration software may erase the physical identity watermark and invalidate trusted local configuration state.

Do not engineer around this with hidden heuristics.

Do not currently parse the stock datafile for identity.

The stock app also does not meaningfully support multiple independently managed physical mice, so its local state cannot solve our multi-mouse identity problem anyway.

If a watermark is missing after persistent identity has been initialized:

```text
identity is unknown
```

Do not restore an old token merely because the endpoint/path resembles a known one.

Require explicit reassociation.

Restore never reuses the old token; it always rotates to a fresh one (see §16).

---

# 9. Watermark ownership and configuration writes

Watermark bytes are **identity metadata**, not transferable DPI configuration.

They must not behave like ordinary `preserved_tail`.

In particular:

```text
export mouse A settings
import onto mouse B
```

must not copy A's physical identity.

Conceptually every driver-owned `0x04` write should be:

```text
construct desired DPI state
        ↓
overlay THIS physical mouse's watermark
        ↓
recalculate packet checksum
        ↓
write
```

Once a logical mouse has a physical watermark:

> every `0x04` write made by this driver must preserve/re-stamp that mouse's watermark.

This should ideally be centralized at the lowest sensible DPI-write layer so individual callers cannot accidentally omit it.

---

# 10. Profiles and watermarking

The watermark should be available in every firmware profile the driver meaningfully uses.

Do not necessarily perform an invasive "stamp all five forever" operation during ordinary use.

A reasonable invariant is:

> Every firmware profile that the driver has activated/configured for an identified mouse carries that mouse's watermark.

Therefore:

```text
initial enrollment
    → capture/stamp relevant profiles

normal DPI write(profile N)
    → automatically stamp N

driver activates/enables a previously unused profile
    → ensure N is stamped
```

The first multi-mouse bootstrap may legitimately walk/capture all profiles because it is already an explicit invasive enrollment operation.

Ordinary later behavior need not repeatedly do so.

---

# 11. Subsequent `Add another mouse`

Once at least the existing mice already have physical watermarks, adding mouse 3+ becomes much simpler.

The user does **not** need all previously registered mice nearby.

UI:

> **Add another mouse**
> Reconnect the mouse you want to add.

Then:

```text
read candidate watermark
        ↓
known local token
    → "This mouse is already added."

absent or malformed watermark
    → capture its current state
    → generate token C
    → stamp C
    → create new logical mouse

unsupported future version
    → refuse; do not overwrite

valid token unknown to this installation
    → already-tagged physical mouse
    → require explicit confirmation, then adopt as a new local logical mouse
```

Existing A/B do not need to be connected because their physical/logical association is already proven.

This is why the **both mice nearby** requirement applies only to the initial migration from fuzzy single-mouse state.

---

# 12. FA61 versus FA60

The transports have very different identity-read costs.

### FA61 wired

Armed/readback operations are cheap.

When persistent physical identity exists, reading the watermark on attachment is reasonable.

### FA60 receiver

An armed read currently produces roughly a ~500 ms cursor freeze.

Avoid making ordinary single-mouse users pay that cost.

Once persistent multi-mouse identity has been explicitly initialized, the cost becomes justified when physical distinction is required.

The receiver is treated as effectively paired with its physical mouse in normal OEM operation; there is no OEM user-facing pairing flow. Therefore the relevant cache invalidation boundary is receiver endpoint attachment/disappearance, not arbitrary mouse RF sleep/wake.

Do not build a complicated permanent identity-provenance protocol merely to optimize this niche case.

The explicit Add-mouse ceremony is where expensive reads are most acceptable.

---

# 13. BLE participation

BLE cannot establish or verify a DPI watermark.

* Physical enrollment — initial setup, Restore, or any watermark stamping — requires the mouse presented through **wired or receiver USB**.
* After a mouse has been identified over USB, its BLE platform ID may be cached as the locator for an **explicitly associated** BLE connection.
* The platform ID is a locator only; it never becomes physical identity.
* BLE association happens only through an explicit reconnect ceremony. If the platform ID changes, the BLE connection becomes unassociated again.
* While operating over cached BLE, identity cannot be revalidated until the mouse is seen over USB again. This limitation is documented rather than obscured.

### Authentication cache lifetime

Identity is authenticated **once per continuous attachment**:

* legacy mode performs no identity read at all;
* a persistent-mode USB endpoint is read/authenticated once when it appears;
* endpoint disappearance invalidates the cache, even if the OS later reuses the same HID path — the cache is keyed by attachment generation, not by locator text;
* reappearance authenticates again;
* each short-lived CLI invocation authenticates independently.

This keeps the FA60 receiver's ~500 ms armed-read cursor freeze off ordinary operation: one identity read per attachment, never per operation and never once forever.

---

# 14. Endpoint linking

Because one physical mouse may have multiple transports:

```text
FA61 token A
FA60 token A
```

means those endpoints belong to the same logical mouse.

Conversely:

```text
FA61 token A
FA60 token B
```

means two different physical mice.

Before physical identity exists, do not infer this relationship from:

* endpoint count;
* VID/PID;
* connection timing;
* similar HID path;
* similar configuration.

Those can be hints during research/debugging but not identity evidence.

---

# 15. Failure rules

Prefer unknown over incorrect identity.

Important cases:

```text
valid known token
    → resolve exact logical mouse

valid unknown token
    → already-tagged physical mouse
    → offer confirmed adoption

absent or malformed token during persistent-identity mode
    → unassociated connection
    → offer Restore or Add

unsupported future watermark version
    → refuse with an upgrade-oriented message
    → never overwrite

same token observed on two independently authenticated physical devices
    → DuplicateIdentity
    → refuse automatic association
```

Never silently choose one.

Never automatically stamp an existing known token onto an unverified device.

That is the most important safety invariant in the whole design.

Discovery resolution and operation targets are typed. A discovered endpoint resolves to either an associated logical mouse or an **unassociated connection** with a specific reason:

```rust
Known(DeviceId)
UnknownTagged(PhysicalId)
Unmarked
Malformed
DuplicateIdentity
```

Associated logical mice and unassociated connections are distinct operation targets; the manager never manufactures a `mouse-N` for the latter, and ceremony progress (enrollment / restoration / adoption) is represented as typed phases rather than ad-hoc flags.

---

# 16. Restoration, adoption, and forgetting

In persistent mode, an unassociated USB mouse offers two paths:

```text
"This mouse isn't recognized."
    ├── Restore a saved mouse
    └── Add as a different mouse
```

"Restore" is an explicit reassociation ceremony for a mouse whose watermark was lost (stock software, user action). It is a user assertion: no hardware fact can prove which former physical unit this is.

### Restore rotates to a fresh token

Restore must **not** reuse the old token, even though the logical mouse keeps its display name:

```text
saved logical mouse: token A
unrecognized physical mouse selected for Restore
        ↓
freshly capture every profile
        ↓
generate token B (OS RNG)
        ↓
stamp and confirm B across the captured profiles
        ↓
logical mouse keeps its name; physical_id becomes B
token A leaves the local token index
```

After the transaction commits:

```text
Desk = token B
token A = valid, but unknown to this installation
```

A returning A-tagged physical mouse appears as an already-identified mouse that hasn't been added here, and may be adopted with confirmation as a separate logical mouse. That surfaces a mistaken Restore without duplicate watermarks and without destroying either device's identity.

"Invalidate A" means **invalidate the local `A → Desk` association only**. There is no global identity authority and no permanent tombstone/revocation ledger: tokens remain physical identifiers, not installation-owned credentials.

### Restore transaction

Restore proceeds as one transaction:

1. The selected saved mouse must not already have an authenticated connected USB endpoint.
2. Await a fresh reconnect of the physical mouse being assigned.
3. Capture every profile from that mouse.
4. Generate token B and persist the journal (old token A, proposed token B, captures, per-profile progress) **before any hardware write**.
5. Stamp and confirm B across the relevant profiles.
6. Replace the old saved configuration with the fresh capture; presentation metadata such as the name is preserved.
7. Drop every old endpoint association — the old wired/receiver locators **and the cached BLE platform ID** — keeping only the USB endpoint authenticated by this reconnect.

While the transaction is incomplete, the journal **reserves both tokens** (A and B); neither may be independently adopted. A concurrent transition (another enrollment / restoration / adoption) is refused installation-wide until the journal completes or is explicitly recovered, and no ordinary configuration write is allowed to the involved mouse or endpoint until completion.

After commit, matching USB endpoints may reattach automatically by presenting B; BLE must go through explicit association again.

### Intact tokens are never overwritten

If the mouse presented for Restore carries a valid token, stop before writing:

```text
known token
    → open that saved mouse

foreign token
    → offer confirmed adoption

token reserved by an active journal
    → wait/resume that transition

absent or malformed
    → eligible for Restore or Add

unsupported future version
    → refuse with an upgrade-oriented message; do not overwrite
```

Restore is never a general "overwrite whatever identity is here" button.

### Foreign tokens require confirmation

A valid token unknown to this installation means an already-tagged physical mouse. Adopting it:

* requires explicit user confirmation;
* captures all profiles;
* verifies that the valid profile watermarks agree;
* if some relevant profiles are unmarked, stamps them with the adopted token;
* creates a fresh local logical mouse around the existing physical token;
* **never** replaces the foreign token merely because it originated in another installation.

If one physical mouse presents different valid tokens in different profiles, do not choose one: treat it as damaged identity state and require an explicit recovery/enrollment path.

### Forget

Forgetting a logical mouse removes the local association **without touching hardware**; it works while the mouse is disconnected. If that physical mouse returns, its token is valid but locally unknown, and the app offers confirmed adoption. Adopting it again creates a fresh local logical object around the existing physical token; no hardware write is necessary unless some profiles need normalization.

---

# 17. Naming model

Mouse names belong to logical devices.

For example:

```rust
DeviceIdentity {
    id: DeviceId,              // internal: mouse-2
    display_name: Option<String>,
    physical_id: Option<PhysicalId>,
    endpoints: ...,
}
```

GUI should primarily use:

```text
Desk
Backpack
White mouse
```

rather than exposing `mouse-1` unless needed for diagnostics/CLI.

Renaming a logical mouse never affects its watermark.

A physical watermark also never encodes the display name.

---

# 18. Intended lifecycle

The complete lifecycle should look roughly like this:

```text
INSTALL / FIRST USE

one supported mouse
        ↓
create logical mouse-1
physical_id = None
        ↓
normal single-mouse operation
NO watermark reads/writes required


USER CHOOSES "ADD ANOTHER MOUSE"

show:
"You'll need both mice nearby."
        ↓
user reconnects mouse being added
        ↓
explicitly capture that physical mouse
        ↓
user reconnects other mouse
        ↓
explicitly capture that physical mouse
        ↓
assign distinct random tokens
stamp watermark(s)
        ↓
migrate names/state only when unambiguous
        ↓
persistent physical identity initialized


NORMAL MULTI-MOUSE USE

endpoint appears
        ↓
read watermark when needed
        ↓
token A → logical A
token B → logical B
        ↓
normal writes always preserve corresponding token


ADD MOUSE 3

"Reconnect the mouse you want to add."
        ↓
unmarked → capture + token C + create logical C
known token → already registered
        ↓
done
```

UNRECOGNIZED / LOST IDENTITY

unmarked, malformed, or unknown endpoint
        ↓
"This mouse isn't recognized."
    ├── Restore a saved mouse → rotate to fresh token B
    │       (old token A becomes valid-but-unknown)
    └── Add as a different mouse → adopt with explicit confirmation
        ↓
an interrupted ceremony resumes from the persisted journal;
it never silently returns to legacy mode


FORGET A SAVED MOUSE

removes the local association only
hardware watermark untouched
        ↓
physical mouse returns
    → valid-but-unknown token
    → offer confirmed adoption again
```

# 19. Non-goals

For the initial implementation, explicitly do **not** add:

* factory/unit-ID fingerprinting;
* configuration-similarity identity heuristics;
* weighted migration scoring;
* endpoint-count physical-mouse inference;
* stock-state-file identity recovery;
* stock-software concurrent synchronization;
* hidden-button watermark fallback unless DPI-tail storage later proves insufficient;
* a general identity provenance/evidence framework;
* automatic foreign-token adoption (explicit confirmation is always required);
* permanent token tombstones or a revocation ledger;
* identity revalidation over cached BLE;
* automatic watermarking of ordinary single-mouse installations.

The design should remain much simpler:

> **Single-mouse mode does not claim physical identity.
> “Add another mouse” explicitly establishes physical identity.
> From that point onward, the watermark is authoritative and must never be guessed or copied.**