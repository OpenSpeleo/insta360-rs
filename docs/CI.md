# Continuous integration

[CI](../.github/workflows/ci.yml) owns all test suites and runs on pushes to
`master` and manual dispatch. Pull requests, tag pushes, and pushes to other
branches, including Dependabot branches, do not start CI. To check a branch
before merging, manually dispatch the workflow for that branch. The full lint,
Rust feature and Python matrices run on Linux x86_64 (`ubuntu-24.04`).
Additional underwater-engine Rust jobs execute on Linux, macOS ARM64/x86_64 and
Windows x86_64.

After every check passes, CI fetches tags and looks for the workspace's exact
`vMAJOR.MINOR.PATCH` version tag on the tested commit. If present, it dispatches
the [release workflow](../.github/workflows/release.yml) at that tag, passing
the CI run ID and attempt. Otherwise CI finishes without releasing. A tag pushed
after CI finishes requires rerunning CI or dispatching CI at that tag.

Release validates the tag and originating CI run before building fresh Linux,
macOS, and Windows distributions. It waits for CI to reach a successful
conclusion, including the final dispatch job, and requires the same repository,
CI workflow, permitted event, commit, and run attempt. Release retains package
build/repair checks and Cargo publication verification; test suites remain in
CI. No CI distribution artifacts are published. See
[release instructions](RELEASE.md) for dispatch, attestations, and recovery.

For Rust publication, the release job uses this repository’s Cargo workspace.
The Python extension has `publish = false`, leaving six publishable crates.
Cargo packages and verifies all selected crates before its first upload. A
deliberate compilation failure in any crate must therefore leave every crate
unpublished. Separate registry requests can still fail after an earlier upload;
see [release recovery](RELEASE.md#failures-and-retries).

## Compiler and native dependencies

[rust-toolchain.toml](../rust-toolchain.toml) is the single authority for the
Rust compiler. Local Cargo, the shared CI setup action, and every wheel builder
use its channel. There is no independently pinned CI version, moving stable
channel, or alternate MSRV compiler. Update that file to change the compiler
everywhere. All seven workspace members share the committed root `Cargo.lock`,
and builds use `--locked`. Default members are the library and five data crates.
Python is tested explicitly so its media/GPU dependencies do not change the
library feature matrix.

A dedicated FFmpeg prewarm job runs directly in the pinned manylinux_2_28
Actions job container. A small preceding job reads the image and native cache
key from `scripts/ci/build-wheel.sh`, so CI and local builds share one image pin
and one native build identity. The key covers the image digest, FFmpeg build
script, and `scripts/ci/ffmpeg-runtime-config.sh`; Rust/Python and
wheel-packaging changes do not invalidate the native SDK.

The prewarm job caches only the completed `dist/ffmpeg-sdk.tar.gz`, containing
the installed libraries, headers, notices, and verified source downloads. On an
exact cache hit it skips package installation and compilation; Actions still
starts the job container. On a miss it builds and validates the SDK, then saves
the archive before uploading `ffmpeg-linux-x86_64`. The job summary reports the
cache key and whether it restored or built the SDK. The Linux lint, Rust, and
wheel jobs download that exact artifact. `use-ffmpeg.py` relocates pkg-config
metadata and exports library paths for the consuming checkout. Release can
restore the same completed SDK using the exact native/image cache key. On a
miss, it downloads only `ffmpeg-linux-x86_64` from the verified source CI run.
If that SDK artifact is absent or expired, the Linux wheel builder compiles the
SDK. The wheel and sdist are always built afresh. Linux, macOS, and Windows
cannot share compiled native libraries; native release caches remain
target-specific.

The separate system `ffmpeg`/`ffprobe` executables generate test fixtures; the
application links the prepared FFmpeg libraries. Fixture generation needs
MPEG-4, AAC, ALAC, AC3, and libx265 encoders and lavfi sources. Linux GPU tests
use Mesa's software Vulkan driver with `INSTA360_RS_REQUIRE_GPU=1`, so an absent
adapter fails rather than silently skipping. This exercises Vulkan functionality
without requiring a physical GPU. Licensed recordings, vendor-oracle
comparisons, and physical GPU qualification remain separate; see
[testing.md](testing.md).

## Checks and artifacts

| Job                                            | Coverage                                                                                                                         |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| Prepare shared FFmpeg libraries                | Cached source build and runtime capability validation                                                                            |
| Full prek                                      | Both hook configurations and all Rust manifests                                                                                  |
| Rust (default/media/gpu/cli/underwater-ai/all) | All test targets, doctests, and release builds of the library, enabled CLI, and examples for each feature configuration          |
| Rust (all)                                     | Also executes the binding crate's Rust unit tests and builds API documentation with warnings denied                              |
| Rust (all)                                     | Also packages, size-checks, tests, and builds all six extracted crates with `python scripts/ci/check-packages.py --all-features` |
| Linux Python build                             | Creates an sdist, builds an ABI3 wheel from it, repairs its libraries, and smoke tests a clean installation                      |
| Python 3.10–3.14                               | Installs the repaired wheel and executes the full Python suite on every interpreter, requiring usable GPU and compiled AI        |
| Underwater engine platforms                    | All core/AI test targets and doctests on Linux, macOS ARM64/x86_64 and Windows; independent MNN build on each host               |
| Trigger matching release                       | Dispatches release only for the workspace-version tag on the commit that passed all checks                                       |

CI's `python-test-dist-linux` artifact contains its test wheel and sdist;
`rust-crates` contains its verified crate archives. These are available for
inspection, while release builds its own distributions. Linux's clean smoke test
uses a fresh Python container without system FFmpeg libraries and checks import,
capabilities, probing, decoding, extraction, and typing metadata.

The PyO3 `extension-module` feature is enabled by Maturin when building wheels.
Plain `cargo test --manifest-path src-python/Cargo.toml` leaves that feature off
so Rust test executables can link libpython. The resulting wheel still uses the
Python 3.10 stable ABI. Wheel builders retain their existing Maturin release
settings; this workflow separation does not change Rust optimization profiles.

## Local setup and checks

Run these commands from this independent repository’s workspace root. Install
rustup and cargo-binstall using their
[official installation instructions](https://github.com/cargo-bins/cargo-binstall#installation).
On Ubuntu 24.04:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends -y \
  build-essential cmake pkg-config clang libclang-dev llvm-dev \
  ffmpeg libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
  mesa-vulkan-drivers libvulkan1 vulkan-tools python3 python3-dev python3-venv
cargo --version # rustup installs the toolchain from rust-toolchain.toml
cargo binstall --no-confirm prek@0.4.14 cargo-machete@0.9.1
prek install
prek run --all-files --show-diff-on-failure
```

These local commands use the distro's FFmpeg development libraries. To use the
exact source-built Linux FFmpeg libraries, install Docker, run
`bash scripts/ci/build-wheel.sh --prewarm`, and inspect the installation prefix
selected by `.cache/wheel/sdk-prefix.txt`. CI downloads/extracts that archive
and runs `use-ffmpeg.py` to export its environment through `GITHUB_ENV`.

On macOS, install Xcode or Command Line Tools plus CMake, pkg-config and FFmpeg
development libraries (for example, `brew install cmake pkg-config ffmpeg`). The
Clippy hook selects `SDKROOT` with `xcrun --sdk macosx --show-sdk-path` when it
is unset. This pairs the SDK with the selected developer installation: an
implicit Command Line Tools SDK can otherwise be newer than Xcode's linker and
fail with a TAPI `unknown architecture` error. Explicit `SDKROOT` and other
compiler settings are preserved; no global Xcode selection is changed. For
direct native/Cargo builds, export the same SDK before building MNN:

```sh
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
```

Prek discovers [.pre-commit-config.yaml](../.pre-commit-config.yaml) and the
nested [Python configuration](../src-python/.pre-commit-config.yaml). It checks
common file errors, Markdown formatting, YAML, GitHub Actions, Rust formatting,
unused dependencies, and Clippy across all seven workspace members with all
features and warnings denied. Clippy also performs compilation checks. Rust
hooks trigger for manifests and lockfiles as well as Rust sources. The Clippy
hook also runs when its configuration or native setup scripts change. The Python
project adds Ruff lint/format and Bandit. Python tests run separately from
pre-commit, so hooks do not require an installed Python extension. CI caches
tools using both hook configurations. Fix Rust formatting with:

```sh
cargo fmt --all
```

The Clippy hook enters through
[`scripts/ci/run-clippy.py`](../scripts/ci/run-clippy.py), so direct Git and
prek invocations prepare the native environment too. If `MNN_ROOT` is unset, it
runs the pinned MNN builder and caches the verified prefix under `.cache/mnn/`,
keyed by the builder's configuration identity. The first run needs network
access, CMake and a C/C++ compiler; subsequent runs verify and reuse that
prefix. A changed configuration selects a separate cache directory. An explicit
`MNN_ROOT`, including CI's prepared prefix, is verified without downloading or
rebuilding it. Invalid prefixes and setup failures stop the hook before Cargo.
FFmpeg discovery and caller compiler/build settings are preserved. To run the
same check directly, use `python3 scripts/ci/run-clippy.py`; raw
`cargo clippy --workspace --locked --all-targets --all-features -- -D warnings`
still requires a prepared native environment. Runner regression tests are
included in the `scripts/ci` unittest suite.

Original vendor payload directories are excluded from text hooks: formatting
would invalidate their hashes. Rust's bundled-asset tests check their integrity.
The asset manifest and provider code remain covered by hooks. Markdown and
whitespace hooks may rewrite files; review those edits and rerun until clean.

Run the test/build matrix:

```sh
export XDG_RUNTIME_DIR="$(mktemp -d)"
export VK_LOADER_DRIVERS_SELECT='lvp_icd*'
export INSTA360_RS_REQUIRE_GPU=1
export LIBCLANG_PATH="$(llvm-config --libdir)"
python3 scripts/ci/build-mnn.py --output .cache/mnn
export MNN_ROOT="$PWD/.cache/mnn"
for features in '' media gpu cli underwater-ai media,gpu,cli,underwater-ai; do
  cargo test --locked --all-targets --no-default-features --features "$features"
  cargo test --locked --doc --no-default-features --features "$features"
  cargo build --locked --release --lib --bins --examples \
    --no-default-features --features "$features"
done
cargo test --locked --manifest-path src-python/Cargo.toml --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features
python3 scripts/ci/check-packages.py --all-features
python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v
```

For fast Python development, build and install a native wheel against system
FFmpeg, then run the complete installed-package tests:

```sh
cargo binstall --no-confirm maturin@1.15.0
maturin build --locked --release --manifest-path src-python/Cargo.toml \
  --compatibility linux --out dist
python3 -m venv .venv
.venv/bin/python -m pip install dist/*.whl
.venv/bin/python -I -X faulthandler src-python/scripts/run-tests.py
```

The same strict test runner is used for manual testing and every Python
3.10–3.14 installed-wheel job. It requires the native extension, fixture tools,
software HEVC encoding, and nonzero test collection before running the suite.

For the actual Linux distribution build, run:

```sh
bash scripts/ci/build-wheel.sh
```

It emits a repaired manylinux_2_28 x86_64 wheel, source distribution, and
runtime source material under `dist/`. Maturin builds the wheel from the sdist,
verifying that the Rust path dependency and embedded resources survived
packaging. See [RELEASE.md](RELEASE.md) for the complete artifact/platform
matrix.

## GitHub setup and maintenance

Enable Actions and allow the referenced actions. CI requires no repository
secrets or publishing permissions. Its final dispatch job receives
`actions: write`; other CI jobs have read-only repository permissions. Master
pushes run Full prek, every Rust matrix job, the Linux test-wheel build, and
every Python test job. PR checks are not started automatically, so requiring
them for merging would require a manual CI run on the PR's current commit.

`Swatinem/rust-cache` caches Rust dependencies and build outputs with separate
keys for each feature job and each wheel target. `actions/cache` stores prek
environments, completed SDKs, and tool downloads. The Linux wheel tools cache
contains only Cargo and rustup directories, avoiding another copy of the SDK and
native build trees. Build artifacts transfer the prepared libraries between CI
jobs and provide the Linux release job's cache fallback. GitHub scopes cache
access by ref: releases can restore the Linux SDK populated on the default
branch, but a native cache created on one release tag is not available to
another tag. Native caches can help same-tag reruns or restore matching
default-branch entries; cross-tag hits are not guaranteed. See
[GitHub's cache access rules](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#restrictions-for-accessing-a-cache).
Cache storage is subject to the repository limit and eviction policy. A miss
must remain buildable. Check the cache result and key before treating a slow
prewarm job as a compiler problem.

GitHub Actions are pinned by commit and updated by Dependabot, alongside the
shared Cargo dependency graph and Python dependencies in
`src-python/pyproject.toml` and `scripts/ci/requirements-wheel.txt`. Actions
monitoring includes local composite action directories. All three ecosystems are
checked weekly. CMake stays below 4 until x265 supports its removed policies.
Run `prek autoupdate` to update hooks, review the revisions, and rerun all
checks. Keep binstall tool versions aligned when upgrading them. Compiler
upgrades belong only in `rust-toolchain.toml`.

Windows/macOS wheels are built in release and receive compilation and repair
checks, without runtime testing. Linux ARM64, Windows ARM64, musl, PyPy, and
free-threaded CPython wheels are outside this build matrix. See
[RELEASE.md](RELEASE.md) for registry configuration and publication from a
version tag.

## Optional underwater AI native prerequisite

The core and legacy restoration build without MNN. Enabling `underwater-ai`
requires CMake, a C/C++ compiler and a verified static CPU prefix:

```sh
python3 scripts/ci/build-mnn.py --output /path/to/mnn-prefix --jobs 2
export MNN_ROOT=/path/to/mnn-prefix
cargo test --locked --no-default-features --features underwater-ai --lib underwater
```

`--prefix` is an alias for `--output`. The builder downloads the official MNN
3.6.1 source at commit `d407447ed56c4121a11ccbd266dc184ca1ead0c2`, verifies
archive SHA-256
`13dca9547df7dac40ab40c7318136406f4a33dfe99cd40bfaa4dbb2270cb8795`, and builds a
Release static library with position-independent code. CUDA, OpenCL, Metal,
Vulkan, OpenMP, training, converters, tools and optional KleidiAI/SME2 code are
disabled. Windows uses the dynamic MSVC C runtime with a static MNN library. The
Rust build compiles its private C++17 adapter and links MNN only when the
feature is enabled.

The prefix includes unchanged MNN licensing bytes and third-party notices.
Release wheels carry both `MNN-LICENSE.txt` and `MNN-THIRD-PARTY-NOTICES.txt`.
Source/configuration/platform/compiler-environment metadata and per-file hashes
validate cached prefixes. A mismatching or damaged existing prefix is rejected;
choose a new output directory or remove the stale build after inspecting it.
Build output is published only after compilation, header copying and notice
preparation succeed. The shared `setup-mnn` action and wheel builders use this
same script; no Insta360 SDK runtime is needed.

Application build wrappers can run
`python3 scripts/ci/build-mnn.py --configuration` to read the JSON cache
identity, including the builder digest, source pin, flags and compiler
environment. `--verify-only --output /path/to/mnn-prefix` validates an existing
prefix and never downloads or builds. Both commands are read-only. This lets
hosts share the builder's authoritative configuration instead of copying its
version pins or CMake flags.

`underwater::mnn_runtime_version()` returns the actual linked library version;
the adapter also rejects model creation unless that version is `3.6.1`. The
Python equivalent is `mnn_runtime_version()`. This runtime check supplements the
prefix manifest and does not replace file-integrity verification.

Run builder regression tests with
`python3 -m unittest discover -s scripts/ci -p test_mnn_build.py -v`. Model
tests actually execute both original graphs through the independent CPU
interpreter. They cover all four styles and temporal processing, and compare
complete tensors for two varied input patterns to an independent C++ MNN
reference. A separate complete RGB sequence fixture is explicitly a regression
snapshot of this implementation, not an independent restoration oracle. See
[reference provenance](../tests/reference/README.md). Default-feature tests also
check that requesting AI without the feature returns `MissingCapability`.
