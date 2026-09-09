# Releasing to crates.io and PyPI

[CI](CI.md) runs all tests on `master` pushes or manual dispatch. After every
check passes, it fetches tags and dispatches
[Release](../.github/workflows/release.yml) for the workspace-version tag on the
tested commit, such as `v0.1.0`. Release accepts only workflow dispatch at a tag
and verifies the source CI run ID and attempt. It then builds fresh Rust and
Python distributions and publishes to crates.io and PyPI. Test suites remain in
CI; release keeps package build verification and wheel repair. The two
registries have separate publishing jobs and credentials.

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
publishing support. The Python publishing job also receives
`attestations: write` to store build provenance on GitHub. CI's final dispatch
job receives `actions: write`; release's CI verification and Linux build jobs
receive `actions: read` to inspect the run and retrieve its FFmpeg SDK. Build
jobs have read-only repository permissions.

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
initial manual uploads. If CI dispatches the matching first tag to publish
Python, the Rust job will encounter already published versions. Verify those
versions and allow the independent Python job to complete; do not delete or
retag the release. Future releases publish both data crates before the library,
after all three verification builds have passed. A local dry run cannot verify
remote permissions or replace the explicit archive-size check.

## Prepare a release

`[workspace.package].version` in the root `Cargo.toml` is the version authority.
All four Rust packages inherit it, and Maturin derives the Python version from
Cargo through `project.dynamic`. Author, repository, edition, and minimum Rust
version are shared there too. The library and Python extension inherit the
project license; each data crate retains its vendor-resource license file.

The three exact local dependency pins in the same manifest’s
`[workspace.dependencies]` must match (`=0.1.0` for the initial release). Cargo
does not interpolate the workspace version into dependency requirements. Use
`cargo set-version --workspace <version>` from cargo-edit to update the version,
pins, and lockfile together, previewing with `--dry-run` first. The release gate
checks the version, inheritance, and pins before publication. The Python Rust
extension remains `publish = false`; the library and its two data crates go to
crates.io. Tags use stable `vMAJOR.MINOR.PATCH` versions. Prerelease tags and
Python’s differing prerelease syntax are deliberately rejected by the version
check; add an explicit version mapping before supporting those releases.

Review the root manifest and shared lockfile, then run the checks:

```sh
python3 scripts/ci/check-release.py v0.1.0
prek run --all-files --show-diff-on-failure
```

Run the tests, builds, package dry run, and wheel build described in
[CI.md](CI.md). Review the crate's package inventory, the wheel's
metadata/runtime licenses, source distribution, and changes in dependency
resolution. Licensed recordings, physical GPU qualification, and vendor-oracle
checks follow [testing.md](testing.md) and remain separate from
generated-fixture CI.

Commit all source, manifest, lockfile, and documentation changes on `master`.
Create the release tag on that commit and push the commit and tag together
(replace the example version):

```sh
git tag -a v0.1.0 -m 'Release 0.1.0'
git push --atomic origin master v0.1.0
```

The `master` push starts CI. Its final job fetches tags, checks that the exact
workspace-version tag points to the tested commit, and dispatches release at
that tag with its run ID and attempt. A tag push alone starts neither workflow.
If the commit was already pushed and CI finished before the tag appeared, rerun
CI on that commit or dispatch it explicitly:

```sh
gh workflow run ci.yml --repo OpenSpeleo/insta360-rs --ref v0.1.0
```

Release checks package versions and verifies the source run's repository,
`.github/workflows/ci.yml` identity, event (`master` push or manual dispatch),
commit SHA, and exact current attempt. It polls while the dispatching CI run
finishes and requires a successful conclusion before building. Failed,
cancelled, mismatched, or superseded attempts cannot authorize publication.

Release builds fresh Linux, macOS ARM64/x86_64, and Windows x86_64 wheels; Linux
also supplies the sdist. Both registry jobs wait for every wheel build. No CI
distribution artifacts are reused. Maturin builds each wheel from an sdist to
verify source completeness, using the existing release settings and Rust
profiles. Release first tries the exact native/image cache key for the completed
Linux FFmpeg SDK. On a miss it downloads only `ffmpeg-linux-x86_64` from the
verified source CI run; if that artifact is absent or expired, the builder
compiles the SDK. Other platforms keep separate native caches because their
compiled libraries are incompatible. Native caches created on a release tag are
available to same-tag reruns, not other release tags; matching default-branch
caches may also be restored. See
[cache scope](CI.md#github-setup-and-maintenance).

Rust packages and verifies all three crates and checks their sizes before
requesting its temporary credential. It then uses one
`cargo publish --workspace --locked` command with verification enabled: every
crate build finishes before any crate is uploaded.

To dispatch release manually using an existing successful CI run on the same
tagged commit, inspect its identity and current attempt first:

```sh
gh run list --repo OpenSpeleo/insta360-rs --workflow ci.yml
gh api repos/OpenSpeleo/insta360-rs/actions/runs/CI_RUN_ID \
  --jq '{id, run_attempt, path, event, head_branch, head_sha, status, conclusion}'
gh workflow run release.yml --repo OpenSpeleo/insta360-rs --ref v0.1.0 \
  -f ci_run_id=CI_RUN_ID -f ci_run_attempt=CI_RUN_ATTEMPT
```

Replace `CI_RUN_ID` and `CI_RUN_ATTEMPT` with the inspected numeric values.
Manual dispatch performs the same verification and fresh builds; it cannot
bypass CI.

## Distributed artifacts

The Rust data dependencies include the original licensed resources and notices
listed in [packaging.md](packaging.md). Python publishes CPython 3.10+ ABI3
wheels for:

| Platform       | Wheel target                 | Release validation |
| -------------- | ---------------------------- | ------------------ |
| Linux x86_64   | manylinux_2_28 (glibc 2.28+) | Build and repair   |
| macOS ARM64    | macOS 11+                    | Build and repair   |
| macOS x86_64   | macOS 11+                    | Build and repair   |
| Windows x86_64 | win_amd64                    | Build and repair   |

CI separately runs a clean-container smoke test and full Python 3.10–3.14 suites
against its Linux wheel from the same source commit. Release wheels are fresh
builds and do not undergo those runtime suites. macOS/Windows runtime and
physical GPU qualification remain separate.

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

## Attestations and verification

After CI verification and all release wheel builds succeed, the Python
publishing job downloads `python-dist-*` artifacts from its own release run and
signs every wheel and sdist using the pinned `actions/attest` action. All
published Python distributions are built within that release workflow. Its
default predicate is SLSA build provenance, binding the distribution digests to
the repository, source commit, and release workflow run. See the
[GitHub attestation action](https://github.com/actions/attest).

GitHub stores the attestations with the repository and links them from the job
summary. The release run also retains the signed Sigstore bundle as the
`python-provenance-<attempt>` artifact, separately from the distributions
uploaded to PyPI. Each retry retains its own bundle without colliding with an
earlier attempt's artifact. Attestation generation and bundle retention must
succeed before the PyPI upload starts. Only the publishing job receives signing
permissions.

The PyPA publisher explicitly enables PEP 740 attestations, metadata validation,
file hashes, and verbose upload logs. PyPI publish attestations identify the
trusted publisher that uploaded each file; they are separate from GitHub's SLSA
build-provenance attestations. The PyPA action generates and uploads the PyPI
attestations automatically during trusted publishing. See
[PyPI's attestation guide](https://docs.pypi.org/attestations/producing-attestations/).

To verify a downloaded wheel or sdist against GitHub's provenance, use:

```sh
gh attestation verify dist/insta360_rs-0.1.0.tar.gz \
  --repo OpenSpeleo/insta360-rs \
  --signer-workflow OpenSpeleo/insta360-rs/.github/workflows/release.yml
```

Replace the example path with the exact downloaded distribution. PyPI exposes
its publish attestations through the
[Integrity API](https://docs.pypi.org/api/integrity/); follow the
[PyPI verification guide](https://docs.pypi.org/attestations/consuming-attestations/)
to verify those with `pypi-attestations`. Local builds cannot generate the
GitHub Actions signing identity; the first tagged release run validates the OIDC
signing and registry integration.

## Failures and retries

A failed tag/version check, CI verification, or release wheel build prevents
both registries from being written. Fix source failures in a new commit and
create a new version/tag; do not move an already published tag. Fix external
configuration errors in GitHub or the registry, then use GitHub's **Re-run
failed jobs** for the original release run.

The registries cannot commit a release atomically. If one publication succeeds
and the other fails, keep the successful version and rerun only the failed job.
PyPI's `skip-existing` allows retrying a partial upload of the same release
artifact set. Attestations are attached to PyPI files at upload time; skipping
an existing file does not add missing attestations to a previous upload. Cargo
does not overwrite an existing version. If a Cargo upload succeeded before a
later network failure, inspect the registry before retrying; a duplicate-version
error can mean the original upload completed. For a partial Rust release, verify
the already published versions and their contents, then publish only the
remaining packages from a clean checkout of the same tagged source. Cargo
rejects a workspace selection that includes an existing version; the automatic
job does not skip it. For example, when both data crates are already published
and only the library remains:

```sh
cargo publish --locked -p insta360-rs
```

Use multiple `-p` selections if more than one package remains. All selected
packages are verified before any of those remaining uploads. This protects
against build failures; crates.io's separate upload requests are not an atomic
transaction, so network or registry failures can still leave a partial release.

If release artifacts have expired, dispatch a new release run at the same
immutable tag with a still-successful CI run ID and its current attempt. This
rebuilds distributions without rerunning CI or reusing CI's test artifacts.
Expired FFmpeg SDK artifacts do not require a new CI run: release can restore
the exact compatible cache or compile the SDK. If the source CI run itself is no
longer available or successful, run CI at the tag first. Before regenerating a
partially published release, verify existing registry files: a fresh build can
have different bytes and cannot replace an existing upload. Do not substitute
local or unrelated workflow artifacts. Keep the repository, workflow filename,
environment, and tag rules aligned with the trusted publisher when diagnosing
OIDC errors. Changing only an API token secret does not repair an OIDC identity
mismatch.
