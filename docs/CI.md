# Continuous integration

[CI](../.github/workflows/ci.yml) runs on every branch push, every pull request,
and manual dispatch. The release workflow calls the same workflow at the tagged
commit and waits for every job before publishing. All lint and tests run on
Linux x86_64 (`ubuntu-24.04`). macOS and Windows runners only build Python
wheels.

For Rust publication, the release job uses this repository’s Cargo workspace.
The Python extension has `publish = false`, leaving three publishable crates.
Cargo packages and verifies all selected crates before its first upload. A
deliberate compilation failure in any crate must therefore leave every crate
unpublished. Separate registry requests can still fail after an earlier upload;
see [release recovery](RELEASE.md#failures-and-retries).

## Compiler and native dependencies

[rust-toolchain.toml](../rust-toolchain.toml) is the single authority for the
Rust compiler. Local Cargo, the shared CI setup action, and every wheel builder
use its channel. There is no independently pinned CI version, moving stable
channel, or alternate MSRV compiler. Update that file to change the compiler
everywhere. All four workspace members share the committed root `Cargo.lock`,
and builds use `--locked`. Default members are the library and both data crates.
Python is tested explicitly so its media/GPU dependencies do not change the
library feature matrix.

A dedicated FFmpeg prewarm job builds the shared Linux FFmpeg libraries once
inside the pinned manylinux_2_28 container, using the source versions, hashes,
and configuration in `scripts/ci/ffmpeg-runtime-config.sh`. It caches the result
and uploads `ffmpeg-linux-x86_64`. The Linux lint, Rust, and wheel jobs download
that exact artifact. `use-ffmpeg.py` relocates pkg-config metadata and exports
library paths for the consuming checkout. This keeps the tested Rust library and
published Linux wheel on the same FFmpeg build.

The separate system `ffmpeg`/`ffprobe` executables generate test fixtures; the
application links the prepared FFmpeg libraries. Fixture generation needs
MPEG-4, AAC, and libx265 encoders and lavfi sources. Linux GPU tests use Mesa's
software Vulkan driver with `INSTA360_RS_REQUIRE_GPU=1`, so an absent adapter
fails rather than silently skipping. This exercises Vulkan functionality without
requiring a physical GPU. Licensed recordings, vendor-oracle comparisons, and
physical GPU qualification remain separate; see [testing.md](testing.md).

## Checks and artifacts

| Job                              | Coverage                                                                                                                           |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Prepare shared FFmpeg libraries  | Cached source build and runtime capability validation                                                                              |
| Full prek                        | Both hook configurations and all Rust manifests                                                                                    |
| Rust (default/media/gpu/cli/all) | All test targets, doctests, and release builds of the library, enabled CLI, and examples for each feature configuration            |
| Rust (all)                       | Also executes the binding crate's Rust unit tests and builds API documentation with warnings denied                                |
| Rust (all)                       | Also packages, size-checks, tests, and builds all three extracted crates with `python scripts/ci/check-packages.py --all-features` |
| Linux Python build               | Creates an sdist, builds an ABI3 wheel from it, repairs its libraries, and smoke tests a clean installation                        |
| Python 3.10–3.14                 | Installs the repaired Linux wheel and runs every Python unittest on each interpreter                                               |
| macOS/Windows Python builds      | Builds and repairs native ABI3 wheels; no test suites run on these runners                                                         |

Python distribution artifacts use `python-dist-*` names. Linux emits its wheel
and the sdist; macOS ARM64, macOS x86_64, and Windows x86_64 emit wheels. The
release job downloads these artifacts from its own CI run. Linux's clean smoke
test uses a fresh Python container without system FFmpeg libraries and checks
import, capabilities, probing, decoding, extraction, and typing metadata.

The PyO3 `extension-module` feature is enabled by Maturin when building wheels.
Plain `cargo test --manifest-path src-python/Cargo.toml` leaves that feature off
so Rust test executables can link libpython. The resulting wheel still uses the
Python 3.10 stable ABI.

## Local setup and checks

Run these commands from this independent repository’s workspace root. Install
rustup and cargo-binstall using their
[official installation instructions](https://github.com/cargo-bins/cargo-binstall#installation).
On Ubuntu 24.04:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends -y \
  build-essential pkg-config clang libclang-dev llvm-dev \
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

Prek discovers [.pre-commit-config.yaml](../.pre-commit-config.yaml) and the
nested [Python configuration](../src-python/.pre-commit-config.yaml). It checks
common file errors, Markdown formatting, YAML, GitHub Actions, Rust formatting,
unused dependencies, and Clippy across all four workspace members with all
features and warnings denied. Clippy also performs compilation checks. Rust
hooks trigger for manifests and lockfiles as well as Rust sources. The Python
project adds Ruff lint/format and Bandit. Python tests run separately from
pre-commit, so hooks do not require an installed Python extension. CI caches
tools using both hook configurations. Fix Rust formatting with:

```sh
cargo fmt --all
```

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
for features in '' media gpu cli media,gpu,cli; do
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
secrets or publishing permissions, including on fork pull requests. Configure
branch protection to require Full prek, every Rust matrix job, all wheel builds,
and every Python test job. Select the displayed check names after the first run.

`Swatinem/rust-cache` caches Rust dependencies and build outputs with separate
keys for each feature job and each wheel target. `actions/cache` stores prek
environments and native library/tool downloads. Source-build scripts, target,
container image, toolchain file, and lockfiles participate in the relevant cache
keys. Build artifacts transfer the prepared libraries between jobs in the same
run. Caches are disposable; delete the affected cache when investigating stale
state.

GitHub Actions are pinned by commit and updated by Dependabot, alongside the
shared Cargo dependency graph and Python dependencies in
`src-python/pyproject.toml` and `scripts/ci/requirements-wheel.txt`. Actions
monitoring includes local composite action directories. All three ecosystems are
checked weekly. CMake stays below 4 until x265 supports its removed policies.
Run `prek autoupdate` to update hooks, review the revisions, and rerun all
checks. Keep binstall tool versions aligned when upgrading them. Compiler
upgrades belong only in `rust-toolchain.toml`.

Windows/macOS wheels receive compilation and repair checks, without runtime
testing. Linux ARM64, Windows ARM64, musl, PyPy, and free-threaded CPython
wheels are outside this build matrix. Follow [RELEASE.md](RELEASE.md) to
configure registries and publish a version tag.
