# Rust Worker Decision

The strict protocol worker is implemented as a standalone Rust crate in `worker/`.

- Rust edition 2024, MSRV 1.85, committed lockfile, and `#![forbid(unsafe_code)]`.
- Tokio current-thread runtime with a bounded actor mailbox and one stdout writer.
- One actor owns service mutation, publication generations, overlays, result IDs/cache, watcher state, and diagnostics.
- Serde/serde_json classify envelopes before method decoding.
- yaml-rust2 event validation rejects unsafe YAML before conversion.
- pulldown-cmark supplies prose events; project scanners extract Obsidian metadata.
- Standard-library recursion defines traversal and symlink policy; notify supplies recursive invalidation hints.
- jiff defines strict timestamp parsing and UTC rendering.
- A handwritten expression parser and evaluator avoid dynamic execution.
- A project-owned natural comparator avoids locale and integer-overflow differences.
- thiserror defines typed internal failures; tempfile is test-only.

The worker uses strict deterministic date parsing and deterministic natural sorting. Native process memory has no fixed 512 MiB cap.
