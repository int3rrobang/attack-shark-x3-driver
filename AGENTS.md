# Repository Guide for Agents

## Project scope

This repository is a TypeScript USB HID driver and CLI for Attack Shark mice. It originated as an X11 driver and now contains partial X3/FA61 and Kysona M600-family protocol support, probes, captures, and reverse-engineering documentation.

Supported production connection modes are defined in `src/types.ts`:

- `ConnectionMode.Adapter` (`0xfa60`): X11 2.4 GHz adapter.
- `ConnectionMode.Wired` (`0xfa55`): X11 wired.
- `ConnectionMode.X3Wired` (`0xfa61`): X3/FA61 wired.

The CLI defaults to `x3-wired`. BLE support is currently experimental tooling, not part of the production TypeScript transport.

## Sources of truth

- Start with `docs/README.md` for protocol documentation and the evidence legend.
- Treat `src/` and focused tests in `__tests__/` as the source of current implemented behavior.
- Treat files under `docs/samples/`, JSONL probe results, and packet captures as evidence. Do not rewrite raw evidence to match a theory.
- More recent live-confirmed or capture-confirmed evidence overrides static inference. Clearly label unconfirmed interpretations.
- Keep model/transport distinctions explicit. Do not generalize an X3 result to X11, or BLE behavior to USB, without evidence.

## Development commands

Use Bun. The expected version is recorded in `package.json`.

```bash
bun install
bun test
bun run typecheck
bun run build
bun run format
bun run lint
```

Useful focused checks:

```bash
bun test __tests__/DpiBuilder.test.ts
bun run cli --help
bun src/cli.ts hex dpi --stages 800,1600,2400 --active 2
```

Run focused tests while developing, then run the full relevant suite before completion. `bun run format` is a check; use `bun run format:fix` only when formatting changes are intended.

## TypeScript conventions

- The project is strict ESM TypeScript with `NodeNext` resolution.
- Use `.js` extensions in relative TypeScript imports, matching existing files.
- Preserve the strict settings in `tsconfig.json`; do not work around them with broad casts or `any`.
- Follow the existing builder pattern under `src/protocols/` and transport orchestration under `src/core/`.
- Keep packet offsets, fixed bytes, checksums, and model-specific branches named and testable.
- Prefer readonly inputs and explicit unions/enums for constrained protocol values.
- Match the repository's Prettier formatting (tabs, as configured in `.prettierrc`).
- Add or update focused tests whenever packet bytes, lengths, validation, defaults, or mode branching change.

## Protocol implementation rules

- Preserve established X11 wired/adapter output unless the task explicitly changes it with independent evidence.
- Gate X3-specific layouts and checksums by the appropriate model/connection mode; accidental compatibility from checksum overflow or default values is not evidence.
- Packet builders should remain transport-independent where practical. USB/BLE framing and device access belong in transport layers.
- Low-level experimental tools may expose raw writes, but production-facing APIs must validate ranges and block known-dangerous operations.
- Do not rename unknown fields based only on host UI labels. Describe how bytes are used when semantics are unresolved.
- Do not describe RF slots as firmware versions or profile “personas.”

## Hardware and firmware safety

Prefer offline builders, fixtures, and `hex` CLI commands. Do not access hardware unless the user explicitly requests a hardware test.

When hardware testing is authorized:

- Change one field at a time from a known-good packet.
- Back up or record the current state and prepare a known-good recovery sequence first.
- Use conservative delays between configuration packets.
- Record exact bytes, transport, response/ACK, observable effect, and recovery outcome.
- Do not fuzz arbitrary values or unchecked indices.
- Do not run firmware updater executables.
- Do not access or write firmware-update characteristics such as FFC1/FFC2 during normal probing.
- Do not send report `0x06` over X3 BLE; established firmware behavior rejects/skips it.
- Treat report `0x05` mode byte `0x00` over BLE as dangerous until independently characterized.
- Treat experimental scroll-button remaps as unsafe because they may repeat indefinitely until unplug/reboot.
- An ACK means the parser accepted a packet; it does not by itself prove application or persistence.

## Documentation and evidence

- Update the relevant protocol document when implementation behavior or confirmed understanding changes.
- Include exact packet bytes and variant/transport context for important protocol claims.
- Use evidence labels from `docs/README.md`: `live-confirmed`, `capture-confirmed`, `static-analysis`, `inference`, and `corrected`.
- Preserve provenance for copied samples. Do not include private session databases, machine-local paths, or third-party reports wholesale; summarize relevant evidence in-repo.
- Check relative Markdown links after documentation changes.

## Repository hygiene

- Do not edit generated `dist/` output directly.
- Do not modify dependency lockfiles unless dependencies intentionally change.
- Do not commit probe logs, captures, binaries, or extracted artifacts without explicit direction.
- Do not commit, amend, push, or create a pull request unless requested.
- `origin` is the upstream X11 repository. `x3-private` is the private X3 fork. Never push X3 work to `origin` accidentally.
- Keep unrelated existing working-tree changes intact.
