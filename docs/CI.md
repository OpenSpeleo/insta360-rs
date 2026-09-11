# Continuous integration

[CI](../.github/workflows/ci.yml) owns all test suites and runs on pushes to
`master` and manual dispatch. Pull requests, tag pushes, and pushes to other
branches, including Dependabot branches, do not start CI. To check a branch
before merging, manually dispatch the workflow for that branch. One `Tests`
matrix runs all-feature Rust tests and the full installed-wheel Python 3.14
suite on Linux x86_64 (`ubuntu-24.04`), macOS ARM64 (`macos-15`), macOS x86_64
(`macos-15-intel`), and Windows x86_64 (`windows-2025`). Linux also runs lint,
feature-boundary checks, package verification and Python 3.10–3.13 compatibility
tests against its same wheel. There is no separate underwater inference job.

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
library's feature boundaries.

One Prepare FFmpeg matrix covers Linux, macOS ARM64/x86_64 and Windows. Each row
identifies, restores or builds, and uploads its platform's SDK. The Linux row
calls `scripts/ci/build-wheel.sh --prewarm`, which runs Docker with the pinned
manylinux_2_28 image. It reads the cache key from that same builder, so CI and
local builds share one image pin and one native build identity. The key covers
the image digest, FFmpeg build script, and
`scripts/ci/ffmpeg-runtime-config.sh`; Rust/Python and wheel-packaging changes
do not invalidate the native SDK.

The prewarm job caches only the completed `dist/ffmpeg-sdk.tar.gz`, containing
the installed libraries, headers, notices, and verified source downloads. On an
exact cache hit the Linux row skips Docker, package installation and
compilation. On a miss it builds and validates the SDK, then saves the archive
before uploading `ffmpeg-linux-x86_64`. The job summary reports the cache key
and whether it restored or built the SDK. The Linux lint, Rust, and wheel jobs
download that exact artifact. `use-ffmpeg.py` relocates pkg-config metadata and
exports library paths for the consuming checkout. After the Linux host checks,
Tests restores the pristine archive before building the wheel: its pkg-config
files must again refer to the container's `/wheel-cache` mount. Release can
restore the same completed SDK using the exact native/image cache key. On a
miss, it downloads only `ffmpeg-linux-x86_64` from the verified source CI run.
If that SDK artifact is absent or expired, the Linux wheel builder compiles the
SDK. The wheel and sdist are always built afresh.

The macOS and Windows rows in the same matrix prepare their SDKs using
`build-native-wheel.py --prewarm`. This mode builds only FFmpeg and x265 (plus
Windows zlib), then archives the completed prefix and verified source downloads;
it does not build MNN or a wheel. `--cache-key` shares the wheel builder's
native recipe, host, CMake and deployment-target identity, independently of Rust
or Python packaging changes. `prewarm-native-ffmpeg` restores the completed
archive and reruns preparation: an existing matching prefix skips compilation,
while a changed host fingerprint selects a new build. Artifacts are named
`ffmpeg-macos-arm64`, `ffmpeg-macos-x86_64` and `ffmpeg-windows-x86_64`.

Native tests consume those artifacts. `use-ffmpeg.py` relocates pkg-config paths
and exports compiler/runtime paths. macOS libraries use `@rpath` names with
`DYLD_FALLBACK_LIBRARY_PATH`, preserving the separate Homebrew fixture tool's
absolute library dependencies. Windows receives `FFMPEG_DIR` and the SDK DLL
directory on PATH. Native release builds use the same prewarm action and cache,
with the verified source CI artifact as fallback before rebuilding. Native wheel
builds verify and reuse the MNN prefix already prepared by `setup-mnn`. Its
cache key hashes `build-mnn.py --configuration`, including the SDK, deployment
target and compiler settings; an incompatible older prefix must not be restored
under a new configuration.

The separate system `ffmpeg`/`ffprobe` executables generate test fixtures; the
application links the prepared FFmpeg libraries. Fixture generation needs
MPEG-4, AAC, ALAC, AC3, and 10-bit-capable libx265 encoders and lavfi sources.
`check-test-tools.py` requires these capabilities and generates/probes a small
fixture before the suites run. macOS installs Homebrew FFmpeg; Windows installs
the static Chocolatey FFmpeg executables. The prepared SDK itself has no fixture
executables.

Linux requires Mesa software Vulkan and Windows requires a usable D3D12 adapter
(the hosted runner's software WARP adapter can satisfy this), with
`INSTA360_RS_REQUIRE_GPU=1`. macOS runs Metal tests when an adapter is
available; standard hosted runners are not treated as guaranteed GPU hosts. The
installed Python runner reports GPU availability in the job summary. An
unavailable macOS adapter leaves GPU execution unqualified even when the
all-feature build passes. Licensed recordings, vendor-oracle comparisons, and
physical GPU qualification remain separate; see [testing.md](testing.md).

## Checks and artifacts

| Job                              | Coverage                                                                                                                                                         |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Prepare FFmpeg (each platform)   | Restore or build the matching native SDK once for downstream consumers                                                                                           |
| Full prek                        | Both hook configurations, all Rust manifests and CI-script unit tests                                                                                            |
| Tests (each platform)            | All-feature Rust test targets, doctests, release builds and binding Rust tests; build/repair a wheel and run the full Python 3.14 suite                          |
| Tests (Linux), additional checks | Compile isolated features, execute the disabled-AI failure tests, build API docs, verify extracted crate archives, and smoke-test the wheel in a clean container |
| Python 3.10–3.13 installed wheel | Test the same Linux wheel on the remaining supported interpreters; 3.14 already ran in Tests (Linux)                                                             |
| Trigger matching release         | Dispatch release for the matching tag only after every test platform and compatibility row passes                                                                |

`--all-features` excludes code guarded by disabled-feature conditions. Linux
therefore retains `cargo check --all-targets` for default, media, GPU, CLI and
underwater-AI configurations, plus the specific unit/export failures for missing
AI support. These checks do not repeat the complete runtime suites. Extracted
crate tests are retained because they verify the shipped files rather than the
checkout.

CI's `python-test-dist-<platform>` artifacts contain repaired test wheels;
`python-test-dist-linux-x86_64` also contains the source distribution.
`rust-crates` contains verified crate archives. These are available for
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

Run the full test/build configuration:

```sh
export XDG_RUNTIME_DIR="$(mktemp -d)"
export VK_LOADER_DRIVERS_SELECT='lvp_icd*'
export INSTA360_RS_REQUIRE_GPU=1
export LIBCLANG_PATH="$(llvm-config --libdir)"
python3 scripts/ci/build-mnn.py --output .cache/mnn
export MNN_ROOT="$PWD/.cache/mnn"
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
cargo build --locked --release --lib --bins --examples --all-features
cargo test --locked --manifest-path src-python/Cargo.toml --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features
python3 scripts/ci/check-packages.py --all-features
python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v
```

The Linux-only feature-boundary checks are:

```sh
for features in '' media gpu cli underwater-ai; do
  cargo check --locked --all-targets --no-default-features --features "$features"
done
cargo test --locked --no-default-features --lib ai_capability_fails_before_requesting_resources_without_native_feature
cargo test --locked --no-default-features --features media --test decoded_layouts unavailable_ai_fails_preflight_and_both_exports_before_creating_outputs
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
pushes run Full prek, every platform's full Tests job, and the additional Linux
Python compatibility jobs. PR checks are not started automatically, so requiring
them for merging would require a manual CI run on the PR's current commit.

`Swatinem/rust-cache` caches Rust dependencies and build outputs with separate
keys for each platform and each wheel target. `actions/cache` stores prek
environments, completed SDKs, and tool downloads. The Linux wheel tools cache
contains Cargo, rustup and MNN directories, avoiding another copy of the FFmpeg
SDK and native build trees. Build artifacts transfer prepared libraries between
CI jobs and provide each release platform's cache fallback. GitHub scopes cache
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

Every platform builds and runtime-tests its CI wheel. Release builds fresh
wheels with compilation and repair checks without rerunning the runtime suites.
Linux ARM64, Windows ARM64, musl, PyPy, and free-threaded CPython wheels are
outside this build matrix. See [RELEASE.md](RELEASE.md) for registry
configuration and publication from a version tag.

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
