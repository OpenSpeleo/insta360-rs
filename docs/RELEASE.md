# Releasing to crates.io and PyPI

Pushing a tag such as `v0.1.0` starts
[Release](../.github/workflows/release.yml). It validates the package versions,
runs the entire [CI workflow](CI.md) on that commit, and then publishes the
three Rust crates to crates.io and the Linux, macOS, and Windows wheels plus
source distribution to PyPI. Branch pushes and pull requests cannot publish. The
two registries have separate publishing jobs and credentials.

## One-time setup

The checked-in configuration is for `OpenSpeleo/insta360-rs`. If the repository
moves, update the trusted-publisher registrations in both registries.

1. Enable GitHub Actions and create environments named `crates-io` and `pypi` in
   repository Settings → Environments. Set their deployment tag rule to `v*`.
   Keep required reviewers disabled for automatic releases, or enable them if
   maintainers want an approval gate. Do not restrict these environments to only
   the default branch: publication runs on tags.
2. Configure the PyPI project's GitHub trusted publisher with owner
   `OpenSpeleo`, repository `insta360-rs`, workflow filename `release.yml`, and
   environment `pypi`. For a new package, create a pending trusted publisher for
   project `insta360-rs` in your PyPI account's Publishing settings. A pending
   publisher allows the first successful upload to create the project. See
   [PyPI's setup guide](https://docs.pypi.org/trusted-publishers/adding-a-publisher/)
   and
   [new-project guide](https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/).
3. Configure all three crates.io crates' GitHub trusted publishers with owner
   `OpenSpeleo`, repository `insta360-rs`, workflow filename `release.yml`, and
   environment `crates-io`. Each first crate version must be published manually
   before its registration can be created; see the bootstrap procedure below and
   [crates.io trusted publishing](https://crates.io/docs/trusted-publishing).

No `PYPI_API_TOKEN` or permanent `CARGO_REGISTRY_TOKEN` repository secret is
needed. Only publishing jobs receive `id-token: write`. The crates.io action
exchanges GitHub's identity for a short-lived token immediately before Cargo
publishes. PyPI uses the official PyPA publishing action and its trusted
publishing support. CI/build jobs have read-only repository permissions.

The library depends on `insta360-rs-data-core` and
`insta360-rs-data-enhancement`; all three packages must be available on
crates.io. Every compressed archive must be strictly below **10,000,000 bytes**.
The [packaging verifier](../scripts/ci/check-packages.py) checks all archives
before any upload, including builds and tests using only extracted package
contents. The split preserves all resources without a registry size-limit
exception.

The repository is an independent Cargo workspace containing the library, both
data crates, and the Python extension. The extension has `publish = false`, so
`cargo publish --workspace` selects exactly the three crates.io packages. The
release uses the checked-in workspace and its shared lockfile directly.

For the initial crates.io publication, run the full checks on a committed, clean
checkout and verify ownership/name availability for all three packages. Verify
the entire release before authenticating:

```sh
cargo publish --workspace --dry-run --locked
python3 scripts/ci/check-packages.py --check-size-only target/package/tmp-crate/*.crate
cargo login
cargo publish --workspace --locked
```

If `CARGO_TARGET_DIR` is set, use that directory instead of `target` when
checking the archives.

One native Cargo invocation packages and verifies **all three crates before its
first upload**, then uploads those verified archives in dependency order. It
uses a temporary registry overlay to resolve the unpublished data crates during
verification, so the library can build before either dependency exists on
crates.io. Verification stays enabled during publication. If any package fails
to build, Cargo exits without uploading any of them. See the
[pinned Cargo publication implementation](https://github.com/rust-lang/cargo/blob/c980f4866/src/cargo/ops/registry/publish.rs#L150)
and
[package verification](https://github.com/rust-lang/cargo/blob/c980f4866/src/cargo/ops/cargo_package/mod.rs#L308).

CI separately tests the extracted contents of all three package archives and
enforces the size budget. The release job also performs the native workspace dry
run and checks every generated archive's size before requesting its short-lived
credential. A dry run performs no uploads and cannot verify remote publication
permissions.

Register the same trusted publisher for **each** of the three crates after the
initial manual uploads. If the matching first tag is also pushed to publish
Python, the Rust job will encounter already published versions. Verify those
versions and allow the independent Python job to complete; do not delete or
retag the release. Future releases publish both data crates before the library,
after all three verification builds have passed. A local dry run cannot verify
remote permissions or replace the explicit archive-size check.

## Prepare a release

Edit `[workspace.package].version` in the root `Cargo.toml`. All four Rust
packages inherit it, and Maturin derives the Python distribution version from
Cargo through `project.dynamic`. Author, repository, edition, and minimum Rust
version are shared there too. The library and Python extension inherit the
project license; each data crate retains its vendor-resource license file.

Update the three exact local dependency pins in the same manifest’s
`[workspace.dependencies]` to match (`=0.1.0` for the initial release). Cargo
does not interpolate the workspace version into dependency requirements. The
release gate checks the version, inheritance, and pins before publication. The
Python Rust extension remains `publish = false`; the library and its two data
crates go to crates.io. Tags use stable `vMAJOR.MINOR.PATCH` versions.
Prerelease tags and Python’s differing prerelease syntax are deliberately
rejected by the version check; add an explicit version mapping before supporting
those releases.

After editing the root manifest, refresh the shared lockfile and run the checks:

```sh
cargo check
python3 scripts/ci/check-release.py v0.1.0
prek run --all-files --show-diff-on-failure
```

Run the tests, builds, package dry run, and wheel build described in
[CI.md](CI.md). Review the crate's package inventory, the wheel's
metadata/runtime licenses, source distribution, and changes in dependency
resolution. Licensed recordings, physical GPU qualification, and vendor-oracle
checks follow [testing.md](testing.md) and remain separate from
generated-fixture CI.

Commit all source, manifest, lockfile, and documentation changes. From the
tested commit, create and push the release tag (replace the example version):

```sh
git tag -a v0.1.0 -m 'Release 0.1.0'
git push origin HEAD
git push origin v0.1.0
```

The release job checks versions before starting CI. Both publication jobs wait
for the complete reusable CI workflow. PyPI downloads every `python-dist-*`
artifact from that same run. Linux wheels pass installed-package tests; macOS
and Windows wheels pass build and repair checks. Publication does not rebuild
any wheel. Maturin builds each wheel from an sdist to verify source
completeness. Rust packages and verifies all three crates and checks their sizes
before requesting its temporary credential. It then uses one
`cargo publish --workspace --locked` command with verification enabled: every
build finishes before any crate is uploaded.

## Distributed artifacts

The Rust data dependencies include the original licensed resources and notices
listed in [packaging.md](packaging.md). Python publishes CPython 3.10+ ABI3
wheels for:

| Platform       | Wheel target                 | Validation                                         |
| -------------- | ---------------------------- | -------------------------------------------------- |
| Linux x86_64   | manylinux_2_28 (glibc 2.28+) | Build, repair, clean smoke test, full Python tests |
| macOS ARM64    | macOS 11+                    | Build and repair only                              |
| macOS x86_64   | macOS 11+                    | Build and repair only                              |
| Windows x86_64 | win_amd64                    | Build and repair only                              |

The release also includes a Linux-produced sdist. Source builds need Rust,
libclang, pkg-config, and compatible shared FFmpeg development libraries; the
sdist does not install system prerequisites automatically.

Each wheel includes its shared FFmpeg/x265 runtime, license texts, build
configuration/provenance, and corresponding source material. Windows also builds
and bundles zlib. Source versions, SHA-256 hashes, codec selection, and common
notices live in `scripts/ci/ffmpeg-runtime-config.sh`; platform builders add
their compiler/linker options. Compiler selection always comes from
`rust-toolchain.toml`.

FFmpeg's x265-enabled build includes GPL components. Project-authored source
retains its Apache-2.0 metadata, and vendor-resource notices retain their own
attribution. The builders disable automatic optional dependency detection so
incidental runner libraries do not silently enter the wheel.

The project's original `LICENSE.md` and `NOTICE.md` files remain in place.
Release staging copies them under a distinct nested license path along with
runtime notices. This avoids archive path collisions between the core crate and
Python package when Maturin assembles the sdist. Preserve that distinction when
changing license-file metadata.

Linux ARM64, Windows ARM64, and musl wheels are not produced. Repaired wheels
bundle the required FFmpeg libraries; the separate `ffmpeg` executable used to
generate CI fixtures is not part of the Python API. See
[Maturin's distribution guide](https://www.maturin.rs/distribution.html).

## Failures and retries

A failed tag/version check or CI job prevents both registries from being
written. Fix source failures in a new commit and create a new version/tag; do
not move an already published tag. Fix external configuration errors in GitHub
or the registry, then use GitHub's **Re-run failed jobs** for the original
release run.

The registries cannot commit a release atomically. If one publication succeeds
and the other fails, keep the successful version and rerun only the failed job.
PyPI's `skip-existing` allows retrying a partial upload of the already-tested
artifact set. Cargo does not overwrite an existing version. If a Cargo upload
succeeded before a later network failure, inspect the registry before retrying;
a duplicate-version error can mean the original upload completed. For a partial
Rust release, verify the already published versions and their contents, then
publish only the remaining packages from a clean checkout of the same tagged
source. Cargo rejects a workspace selection that includes an existing version;
the automatic job does not skip it. For example, when both data crates are
already published and only the library remains:

```sh
cargo publish --locked -p insta360-rs
```

Use multiple `-p` selections if more than one package remains. All selected
packages are verified before any of those remaining uploads. This protects
against build failures; crates.io's separate upload requests are not an atomic
transaction, so network or registry failures can still leave a partial release.

If artifacts have expired, rebuild the checks for the same immutable tag to
regenerate and test them. Do not substitute local or unrelated workflow
artifacts. Keep the repository, workflow filename, environment, and tag rules
aligned with the trusted publisher when diagnosing OIDC errors. Changing only an
API token secret does not repair an OIDC identity mismatch.
