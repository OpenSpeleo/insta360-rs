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
The Python extension has `publish = false`, leaving seven publishable crates.
Cargo packages and verifies all selected crates before its first upload. A
deliberate compilation failure in any crate must therefore leave every crate
unpublished. Separate registry requests can still fail after an earlier upload;
see [release recovery](RELEASE.md#failures-and-retries).

## Compiler and native dependencies

[rust-toolchain.toml](../rust-toolchain.toml) is the authority for the main Rust
compiler. Local Cargo, the shared CI setup action, and every wheel builder use
its channel. Update that file to change the primary compiler everywhere. All
eight workspace members share the committed root `Cargo.lock`, and builds use
`--locked`. Default members are the library and six data crates. Python is
tested explicitly so its media/GPU dependencies do not change the library's
feature boundaries.

The minimum supported compiler is separately declared by
`workspace.package.rust-version` in `Cargo.toml`. It is 1.90 because the locked
`ordered-float` dependency used by the Linux and Windows GPU backends requires
that version. The Linux job runs [`check-msrv.py`](../scripts/ci/check-msrv.py),
which reads that declaration, installs the matching patch release, and checks
all targets and features of the published workspace members with `rustup run`.
It preserves the prepared FFmpeg and compiler environment, lockfile, Cargo
flags, profile, and target directory; the main compiler remains pinned. This is
a compile check on the minimum compiler, while runtime tests and release builds
use the primary compiler.

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
with the verified source CI artifact as fallback before rebuilding. MNN is built
by Cargo through the exact `mnn-src` dependency, with the same compiler and
deployment-target environment as the native adapter. Ordinary Cargo target
caching covers that build; there is no separate MNN SDK preparation job.

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

Windows x64 MSVC GPU builds embed DXC through `wgpu/static-dxc`; both adapter
discovery and rendering select it explicitly. FXC is not a supported fallback:
it can compile the stitch shader successfully while producing incorrect pixels.
The `mach-dxcompiler-rs` dependency downloads its versioned native archive with
`curl` and extracts it with `tar` on the first build, then reuses Cargo's build
output. Its MSVC archive requires Visual Studio 2022 17.11 or newer and follows
the existing static/dynamic CRT setting. The current upstream build script does
not checksum that native archive; `Cargo.lock` verifies the Rust package only.
No DXC DLL installation or wheel/runtime staging step is needed. Other Windows
targets require a compatible dynamic DXC installation and report GPU
unavailability when it is absent.

Every installed-wheel test job also sets `INSTA360_RS_REQUIRE_UNDERWATER_AI=1`
and `INSTA360_RS_REQUIRE_AI_STITCHING=1`. The runner checks the actual
extension's AquaVision capability and `SeamMode.AI.unavailable_reason()` before
discovering tests. A missing shipped engine fails the job instead of becoming
skipped coverage. Generated-fixture exports execute Dynamic, Optical Flow, and
AI stitching through the installed Python API. Development builds can omit these
requirements to test explicit disabled-feature errors.

## Checks and artifacts

| Job                              | Coverage                                                                                                                                                                           |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Prepare FFmpeg (each platform)   | Restore or build the matching native SDK once for downstream consumers                                                                                                             |
| Full prek                        | Both hook configurations, all Rust manifests and CI-script unit tests                                                                                                              |
| Tests (each platform)            | All-feature Rust targets, isolated neural numerical references, doctests, release builds and binding Rust tests; build/repair a wheel and run the full Python 3.14 suite           |
| Tests (Linux), additional checks | Compile isolated features and the declared Rust minimum, execute disabled-AI failure tests, build API docs, verify extracted crates, and smoke-test the wheel in a clean container |
| Python 3.10–3.13 installed wheel | Test the same Linux wheel on the remaining supported interpreters; 3.14 already ran in Tests (Linux)                                                                               |
| Trigger matching release         | Dispatch release for the matching tag only after every test platform and compatibility row passes                                                                                  |

`--all-features` excludes code guarded by disabled-feature conditions. Linux
therefore retains `cargo check --all-targets` for default, media, GPU, CLI and
underwater-AI and AI-stitching configurations, plus the specific unit/export
failures for missing AI support. These checks do not repeat the complete runtime
suites. Extracted crate tests are retained because they verify the shipped files
rather than the checkout.

The two ignored CPU neural reference tests run separately in fresh processes on
every test platform and for extracted all-feature crate archives. MNN's
process-global CPU worker pool retains its initial size, so running these amid
other tests could silently clamp a requested thread budget. Each reference test
creates its largest budget first and verifies the runtime's effective thread
counts. Ordinary `--all-targets` tests alone do not execute this numerical
contract.

Two isolated GPU checks use strict backend selection, the same numerical
references and tolerances, and repeated session creation/destruction:

- `underwater::model::tests::gpu_varied_tensors_match_complete_reference`
- `seam_ai::tests::gpu_every_flow_value_matches_independent_direct_mnn_reference`

Set `INSTA360_RS_REQUIRE_MNN_GPU=1` to fail if the platform backend is
unavailable. Without it, unavailable GPU support produces an explicit skip;
initialization or numerical failures still fail. These tests assert the actual
Metal/Vulkan session backend, so CPU fallback cannot satisfy GPU coverage. Local
macOS ARM64 execution passed both strict Metal checks with unchanged reference
tolerances. Workflow configuration alone does not establish execution on
Linux/Windows or physical AMD/NVIDIA hardware.

Linux also runs the ignored
`underwater::ilut::tests::exhaustive_optimized_grids_match_scalar_byte_domain`
test once, after the release build, using the primary compiler and ordinary
release profile. It compares optimized step-4 and step-16 LUT application with
the scalar reference across all 16,777,216 RGB inputs. This checks byte
equality; it runs no timing benchmarks and adds no timing threshold to CI.

CI's `python-test-dist-<platform>` artifacts contain repaired test wheels;
`python-test-dist-linux-x86_64` also contains the source distribution.
`rust-crates` contains verified crate archives. These are available for
inspection, while release builds its own distributions. Linux's clean smoke test
uses a fresh Python container without system FFmpeg libraries and checks import,
both shipped AI capabilities, probing, decoding, extraction, and typing
metadata. GPU availability is not required in that container.

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

These local commands use the distro's FFmpeg development libraries. When none
are discoverable, the Clippy hook can prepare the pinned SDK on Linux x86_64
using Docker. Other Linux architectures require system development libraries or
an explicit `FFMPEG_DIR`. To use the exact source-built Linux FFmpeg libraries,
install Docker, run `bash scripts/ci/build-wheel.sh --prewarm`, and inspect the
installation prefix selected by `.cache/wheel/sdk-prefix.txt`. CI
downloads/extracts that archive and runs `use-ffmpeg.py` to export its
environment through `GITHUB_ENV`.

On macOS, install Xcode or Command Line Tools plus CMake, pkg-config and FFmpeg
development libraries (for example, `brew install cmake pkg-config ffmpeg`). The
Clippy hook selects `SDKROOT` with `xcrun --sdk macosx --show-sdk-path` when it
is unset. This pairs the SDK with the selected developer installation: an
implicit Command Line Tools SDK can otherwise be newer than Xcode's linker and
fail with a TAPI `unknown architecture` error. Explicit `SDKROOT` and other
compiler settings are preserved; no global Xcode selection is changed. For
direct Cargo builds, export the same SDK before compiling native dependencies:

```sh
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
```

Prek discovers [.pre-commit-config.yaml](../.pre-commit-config.yaml) and the
nested [Python configuration](../src-python/.pre-commit-config.yaml). It checks
common file errors, Markdown formatting, YAML, GitHub Actions, Rust formatting,
unused dependencies, and Clippy across all eight workspace members with all
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
prek invocations prepare the FFmpeg environment before Cargo. MNN is a normal
optional Cargo dependency and needs no separate preparation. Explicit
`FFMPEG_DIR`, pkg-config search/tool settings and explicit vcpkg selections are
preserved, as are system libraries discoverable through pkg-config. Otherwise,
the hook uses the existing wheel SDK recipes (`build-native-wheel.py --prewarm`
on macOS/Windows and `build-wheel.sh --prewarm` on Linux x86_64). It checks the
recipe's current cache identity and reuses a completed matching SDK without
rebuilding or archiving it. `use-ffmpeg.py` owns SDK relocation and
compiler/runtime path exports. Preparing this environment before Cargo starts is
necessary because `ffmpeg-sys-next` builds before this crate's own build script.

The hook's isolated Python environment supplies the same pinned CMake 3 as the
native wheel recipe; macOS also needs Ninja and pkg-config
(`brew install ninja pkg-config`). Windows requires the MSVC developer
environment and MSYS utilities described in `setup-native-wheel`, including
`INSTA360_MSYS_BIN`. First-time SDK preparation downloads verified FFmpeg/x265
sources and compiles them; subsequent runs reuse the cache. Setup errors stop
before Cargo, and no parent application checkout or global shell configuration
is needed.

To run the same check directly, use `python3 scripts/ci/run-clippy.py` with the
native prerequisites installed (including `cmake==3.31.10` as a Python package
when building the macOS/Windows SDK); raw
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
cargo test --locked --all-targets --all-features
for reference in underwater::ai::tests::aquavision_thread_budget_preserves_numerical_and_temporal_contracts underwater::model::tests::model_varied_tensors_match_complete_reference; do
  cargo test --locked --all-features --lib "$reference" -- --ignored --exact --nocapture --test-threads=1
done
cargo test --locked --doc --all-features
cargo build --locked --release --lib --bins --examples --all-features
cargo test --locked --release --all-features --lib \
  underwater::ilut::tests::exhaustive_optimized_grids_match_scalar_byte_domain \
  -- --ignored --exact
cargo test --locked --manifest-path src-python/Cargo.toml --all-targets
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features
python3 scripts/ci/check-packages.py --all-features
python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v
```

The Linux-only feature-boundary checks are:

```sh
for features in '' media gpu cli underwater-ai ai-stitching; do
  cargo check --locked --all-targets --no-default-features --features "$features"
done
cargo test --locked --no-default-features --lib ai_capability_fails_before_requesting_resources_without_native_feature
cargo test --locked --no-default-features --lib seam_ai::disabled_tests
cargo test --locked --no-default-features --features media --test decoded_layouts disabled_ai_stitching_fails_
cargo test --locked --no-default-features --features media --test decoded_layouts unavailable_ai_fails_preflight_and_both_exports_before_creating_outputs
python3 scripts/ci/check-msrv.py
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
keys for each platform and each wheel target. Its `sccache-v1` prefix separates
Ninja build trees from older Makefile/Visual Studio caches; local caches are not
deleted or migrated. `actions/cache` stores prek environments, completed SDKs,
and tool downloads. The Linux wheel tools cache contains Cargo and rustup
directories, avoiding another copy of the FFmpeg SDK and native build trees.
Build artifacts transfer prepared libraries between CI jobs and provide each
release platform's cache fallback. GitHub scopes cache access by ref: releases
can restore the Linux SDK populated on the default branch, but a native cache
created on one release tag is not available to another tag. Native caches can
help same-tag reruns or restore matching default-branch entries; cross-tag hits
are not guaranteed. See
[GitHub's cache access rules](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#restrictions-for-accessing-a-cache).
Cache storage is subject to the repository limit and eviction policy. A miss
must remain buildable. Check the cache result and key before treating a slow
prewarm job as a compiler problem.

Every Cargo build job also uses the shared `setup-sccache` action, including
Clippy, extracted-package checks, the minimum-Rust check, wheels and Rust
publication. It pins sccache 0.19.0 and Ninja 1.13.0, sets `RUSTC_WRAPPER` and
disables incremental compilation in CI. `cc` uses that wrapper for the adapter;
MNN's CMake build uses a compile-rule launcher covering C/C++, assembly and
Metal Objective-C++. On Windows the launcher converts `/Zi` and `/ZI` to `/Z7`,
including response files, so debug information remains in cacheable objects.
Linking and archiving are not wrapped. Normal local Make/Cargo settings and the
completed FFmpeg SDK cache identity remain unchanged.

Compiler objects use local disk caches with direct mode disabled. Actions saves
one snapshot per OS, architecture, build case and run attempt; subsequent runs
restore the same case's latest snapshot. Release wheels share their platform's
CI case key so tag jobs can restore default-branch snapshots. This avoids one
network request per compiler invocation. Host and manylinux servers use separate
directories in the snapshot. The Linux wheel builder mounts the action's
checksum-verified Linux musl binary, rebases source/toolchain paths inside the
container, and flushes its server before restoring output ownership. It never
forwards a macOS or Windows binary into Linux. Ordinary local wheel builds
without the CI setup retain their existing behavior. Jobs report compiler
statistics and stop the host server before the cache save; container statistics
are reported separately in its build log. A restored Cargo target directory or
successful backend compile does not prove compiler-cache hits or GPU execution.

To qualify cache changes, use isolated temporary source/target/cache paths and
rebuild the same inputs twice, retaining only the compiler cache between builds.
Keep the source path and compiler options fixed, remove only the disposable
target directory, and compare `sccache --show-stats --stats-format=json` after
each build. The warm build must show Rust and native compiler hits; Metal
coverage includes Objective-C++ and assembly. CI normalizes paths under the
workspace and runner temporary directory through `SCCACHE_BASEDIRS`; manylinux
uses its `/build`, `/wheel-target` and `/wheel-cache` paths. This qualification
does not clear normal developer caches or alter their Make configuration.

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

## Optional AI native prerequisite

The core and non-AI modes build without MNN. Enabling `underwater-ai` or
`ai-stitching` activates the exact registry dependency `mnn-src = "=3.6.2"`,
which packages pristine MNN 3.6.2 sources at commit
`7e3c6fb5225d3ccf22807ee641332669df34650d`. CMake and a C/C++ compiler are
required; no separate native prefix or preparation script is needed:

```sh
cargo test --locked --no-default-features --features underwater-ai --lib underwater
```

Cargo builds static MNN with position-independent code and Metal on macOS or
Vulkan on Linux/Windows before compiling this crate's private C++17 adapter.
`DEP_MNN_INCLUDE` supplies matching headers; `mnn-src` owns native linking. The
adapter checks dependency metadata and the actual linked `MNN::getVersion()`
remains checked at runtime. OpenCL, CUDA, OpenMP, training, converters, tools,
KleidiAI and SME2 remain disabled. Windows uses the dynamic MSVC runtime for
both MNN and its adapter. SDK selection and macOS deployment targets remain
caller build inputs.

Both AI features share the adapter and native dependency. Precision, bounded
thread budgets, model decoding, tensor contracts and resource ownership remain
in this crate; [video AI stitching](ai-stitching-model.md) describes its
separate preprocessing and numerical references. The adapter prefers the
platform GPU with `Precision_High`, checks session readiness, and falls back to
CPU when GPU initialization is unavailable. MNN can also execute unsupported
operators on CPU. Inference/transfer errors after initialization remain errors.
CPU thread budgets and GPU tuning modes are configured separately.

Metal uses the system default device. Vulkan uses the first enumerated device;
this may be an integrated or software device rather than the discrete GPU.
Compatible AMD and NVIDIA Vulkan drivers can supply Linux/Windows acceleration,
but each device/driver combination still needs runtime qualification. A selected
GPU session does not imply every operator executes on it.

This crate owns `links = "insta360_mnn"` and forwards MNN's installed notice
directory as `DEP_INSTA360_MNN_LICENSE_DIR` to direct dependents' build scripts.
Applications retain final resource staging and MSVC redistributable packaging.
There are no MNN runtime plugins or shared MNN libraries in this configuration.
Metal uses system frameworks; Vulkan needs the system driver and loader. On
Linux, MNN 3.6.2 opens `libvulkan.so`, not `libvulkan.so.1`, so standalone
applications and wheels require the unversioned loader to be installed (often
provided by `libvulkan-dev`). FrameForge separately stages that loader alias and
its notices in application packages. Windows uses driver-provided
`vulkan-1.dll`. Native driver failures that abort inside MNN cannot be recovered
by the adapter's C++ exception boundary.

Wheel builders stage original notices under `python/insta360_rs/_licenses/mnn/`
and include that tree in wheel license metadata and source distributions.
`scripts/stage-mnn-licenses.py` in the Python project reads the exact package
selected by locked Cargo metadata and its bundled source archive; it neither
compiles native code nor downloads a separate MNN source tree. It preserves
notice paths and bytes and validates the complete compressed stream.

`underwater::mnn_runtime_version()` and Python's `mnn_runtime_version()` report
the linked library version. Rust's
`UnderwaterColorSession::inference_backends()` reports feature/preset session
backends, or `None` for Off/Legacy; `SeamModel::inference_backend()` reports the
seam session backend. Values are `cpu`, `metal` or `vulkan`, describing the main
pipeline rather than every operator. Existing model tests execute all three
original graphs, every underwater style, temporal processing and complete
independent tensor references. Run the two ignored thread-budget reference tests
in separate processes as documented above: MNN's initial process-global worker
pool cannot grow. Keep numerical references unchanged during packaging
migrations. See [reference provenance](../tests/reference/README.md).
Default-feature tests also check that requesting AI without the feature returns
`MissingCapability`.
