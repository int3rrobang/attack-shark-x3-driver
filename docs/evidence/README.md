# Evidence archive

Raw captures, packet dumps, descriptors, and model-specific analysis live here. Files are grouped by the hardware that produced them so X11 evidence is not accidentally generalized to X3/FA61, or vice versa.

- [`x11/`](x11/README.md) — X11 wired and 2.4 GHz adapter descriptors, captures, DPI samples, and macro samples. X11 is historical and unsupported by the current Rust implementation; this evidence is preserved for dialect comparison and provenance.
- [`x3-fa61/`](x3-fa61/README.md) — X3/FA61 captures and exports. This is the current live-tested hardware path.

## Immutable raw evidence policy

Raw evidence under this directory is **immutable**. It must not be rewritten to match a theory, updated to reflect current implementation claims, or deleted because the tooling that produced it has been removed. Interpretations belong in the protocol or research documents and must retain a link to the source artifact.

Historical probing sessions may reference TypeScript or Bun scripts that no longer exist in the repository. Those script references are provenance records — they document what tool produced the capture, not runnable instructions. The raw JSON captures are the authoritative record. The current Rust implementation can reproduce equivalent experiments via `cargo run -p x3ctl -- ...` offline commands.
