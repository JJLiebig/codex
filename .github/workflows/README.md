# Workflow Strategy

The workflows in this directory are split so that pull requests get fast, review-friendly signal while `main` still gets the full cross-platform verification pass.

## Pull Requests

- Required checks run against GitHub's synthetic merge commit, not the pull
  request head alone. This includes changes already on `main` and catches
  conflicts before they reach the branch.
- `bazel.yml` is the main pre-merge verification path for Rust code.
  It runs Bazel `test` and Bazel `clippy` on the supported Bazel targets,
  including the generated Rust test binaries needed to lint inline `#[cfg(test)]`
  code.
- `rust-ci.yml` keeps the Cargo-native PR checks intentionally small:
  - `cargo fmt --check`
  - `cargo shear`
  - `argument-comment-lint` on Linux, macOS, and Windows
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes

## Post-Merge On `main`

- `bazel.yml` also runs on pushes to `main`.
  This re-verifies the merged Bazel path and helps keep the BuildBuddy caches warm.
- `rust-ci-full.yml` is the full Cargo-native verification workflow.
  It keeps the heavier checks off the PR path while still validating them after merge:
  - the full Cargo `clippy` matrix
  - the full Cargo `nextest` matrix via per-platform archive-backed shards
  - Windows ARM64 nextest archives cross-compiled on Windows x64, then replayed on native Windows ARM64 shards
  - release-profile Cargo builds
  - cross-platform `argument-comment-lint`
  - Linux remote-env tests

## Codex++ macOS Allocation Retries

`codex-plus-plus-retry.yml` runs twice an hour after it reaches the default branch;
it can also be dispatched manually on that branch. It retries one macOS app,
voice, or assembly job when its latest check proves hosted-runner allocation failed.
The source release must be completed, match its tag and commit, and be less than
one day old so its retained artifacts can still be used. At most three extra run
attempts are allowed in total across the three macOS jobs, including manual reruns.
Active releases, unrelated failures, and manual job cancellations are left alone.
Exhausted or expired releases need operator investigation rather than more retries.

## Rule Of Thumb

- If a build/test/clippy check can be expressed in Bazel, prefer putting the PR-time version in `bazel.yml`.
- Keep `rust-ci.yml` fast enough that it usually does not dominate PR latency.
- Reserve `rust-ci-full.yml` for heavyweight Cargo-native coverage that Bazel does not replace yet.

## Ready Codex++ platform assets

Each successful platform build uploads its standalone archive and SHA-256
sidecar. The GitHub release goes public once both Windows and Linux archives
and checksums are present. Installers and their checksums come from the validated
release-tag commit. A later macOS build can append its archive, checksums, and
published npm tarball to the same release. Existing assets must match the bytes
being published. The repository must allow mutable releases to append assets.

All partial publishers and the final GitHub publisher share one repository-wide
job concurrency group with `queue: max` and `cancel-in-progress: false`.
Publication compares numeric release versions before setting GitHub latest, so an
older platform finishing late cannot rewind it. Root npm publication waits for
Windows and Linux. The root package pins the matching macOS version as an
optional dependency. The macOS job publishes that package under `darwin-arm64`
when ready. Finalization deletes the Windows, Linux, and root npm intermediates
after their publishers finish. macOS intermediates expire on their own schedule.

Latest standalone installers fail before invoking the managed installer if the
latest release lacks this machine's archive or checksum, preserving the installed
generation. The package-manager update notice and `codex update` resolve npm
`latest` and check its dependency for the current platform. If the dependency is
still pending, the updater leaves the installed version in place. Explicit
failed-run recovery checks the identity of the release tag and source run.
