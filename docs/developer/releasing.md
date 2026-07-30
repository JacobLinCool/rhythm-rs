# Preview releases and installer

Every successful push to `main` produces one mutable GitHub prerelease named
`Taiko on Terminal — Latest Preview`. Its tag is `latest`; both the tag and the
release assets advance to the exact `main` commit that passed the release
quality gate.

This is intentionally a preview channel, not a stable semantic-version release.
Stable versioning and crates.io publication are separate release decisions and
are not performed by this workflow.

## Release contract

`.github/workflows/release.yml` runs the full format, Clippy, workspace test,
installer test, and benchmark-smoke gates before building:

| Runner | Rust target | Release archive |
| --- | --- | --- |
| Ubuntu 22.04 x86-64 | `x86_64-unknown-linux-gnu` | `taiko-x86_64-unknown-linux-gnu.tar.gz` |
| macOS 15 Intel | `x86_64-apple-darwin` | `taiko-x86_64-apple-darwin.tar.gz` |
| macOS 15 Apple silicon | `aarch64-apple-darwin` | `taiko-aarch64-apple-darwin.tar.gz` |
| Windows Server 2025 x86-64 | `x86_64-pc-windows-msvc` | `taiko-x86_64-pc-windows-msvc.tar.gz` |

Each archive contains the platform binary, the repository `README.md`, and the
MIT `LICENSE`. The publish job refuses to continue unless all four archives
exist, generates one `SHA256SUMS` manifest, and removes any release asset
outside this canonical set.

GitHub build-provenance attestations are generated for each archive and the
checksum manifest. A downloaded asset can be checked with:

```bash
gh attestation verify taiko-x86_64-unknown-linux-gnu.tar.gz \
  --repo JacobLinCool/rhythm-rs
```

## Installer contract

The convenience installer supports:

- macOS on Apple silicon or Intel;
- Linux on x86-64.

It selects exactly one release target, downloads the archive and
`SHA256SUMS` from the `latest` tag, verifies SHA-256 before extraction, confirms
that the downloaded binary can report its version, and then atomically replaces
`taiko` in the destination directory. Unsupported systems fail without
downloading an unrelated binary.

The default destination is `$HOME/.local/bin`. Set `TAIKO_INSTALL_DIR` to use
another user-writable directory. The installer never edits shell startup files
and reports when the destination is not already in `PATH`.

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://raw.githubusercontent.com/JacobLinCool/rhythm-rs/main/install.sh | sh
```

`scripts/test-install.sh` exercises the real packaging and installer scripts
against a local mock release. It proves successful installation, the exact
GitHub download URLs, checksum rejection, and unsupported-platform rejection
without relying on the network.

## Maintainer verification

Run the local release-facing checks before pushing:

```bash
shellcheck install.sh scripts/package-release.sh scripts/test-install.sh
sh scripts/test-install.sh
actionlint
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p rhythm-mode-taiko --release bench_smoke_large_chart -- --ignored
cargo test --locked -p taiko-game --release bench::bench_smoke -- --ignored
```

After `main` is pushed, the `Latest Preview` workflow is the release authority.
Do not upload preview assets manually. Release archives are uploaded first,
then the `latest` tag advances, and `SHA256SUMS` is uploaded last. An
interrupted update therefore makes the installer reject any archive/checksum
mismatch instead of installing a partial release. Rerun the failed workflow
after correcting the cause.
