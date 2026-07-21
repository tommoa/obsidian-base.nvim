# Releasing

Workers are released as a platform-specific binary and a sibling `.sha256`
file, following blink.cmp's prebuilt-binary convention. The installer uses a
prebuilt worker only when the plugin checkout is exactly at its release tag,
then verifies the downloaded binary against that sidecar before installing it.

1. Update the worker version in `worker/Cargo.toml` and `flake.nix`, then commit
   the release preparation.
2. Tag that commit with the matching version, for example `v0.1.0`, and push the
   tag. The **Release** workflow builds and smoke-tests every target, generates
   a `.sha256` sidecar for every staged binary, and publishes both files.

Do not change Rust source, dependency locks, build settings, or worker target
definitions after creating the release tag.
