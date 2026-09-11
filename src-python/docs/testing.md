# Build, FFI, and API tests

Tests exercise the installed native extension using small generated recordings.
They require no private camera media or vendor executables. Python's `unittest`
runner is sufficient; wrapper mocks are limited to verifying argument
normalization and forwarding. All media, configuration, capability, error, job,
and output tests call Rust through the real PyO3 extension.

## Complete build verification

After installing the [development prerequisites](installation.md), activate the
development environment and run from the repository root:

```sh
python src-python/scripts/test.py
```

The runner:

1. Requires Cargo, `ffmpeg`, and `ffprobe`, then snapshots the workspace into a
   temporary directory. It stages byte-identical project notices at the same
   distinct package paths used by release builders, avoiding collisions with
   root Cargo notices. It requires the pinned CPU prefix in `MNN_ROOT`, copies
   its original MNN license and third-party notices, and builds a wheel and
   source archive using PEP 517 build isolation.
2. Verifies stable ABI tags, distribution metadata, license/notice files, native
   module, type stubs, and `py.typed`; requires both MNN notices under the
   distribution's licenses directory and rejects leaked development artifacts.
3. Checks that the source archive includes docs, tests, build scripts, core Rust
   sources, and byte-identical embedded assets.
4. Installs the wheel without dependencies in a fresh virtual environment and
   runs all tests from outside the checkout with Python isolated mode (`-I`).
5. Rebuilds a wheel from the extracted source archive and repeats the isolated
   installation and suite using the tests shipped in that archive.

Build artifacts and test environments use temporary directories; Cargo output is
cached in the workspace's `target/python-tests`, or your `CARGO_TARGET_DIR` if
set. The interpreter that launches the runner needs the `build` package. Fresh
test interpreters need standard-library `venv` and `ensurepip` support.

Test one compiled ABI wheel on multiple CPython versions by repeating
`--python`:

```sh
python src-python/scripts/test.py --python python3.10 --python python3.12 --python python3.14
```

Interpreter paths are also accepted. All supplied interpreters must match the
wheel's operating system and architecture.

## Fast iteration against an installed extension

```sh
maturin develop --manifest-path src-python/Cargo.toml
python -I -X faulthandler src-python/scripts/run-tests.py
python -m unittest discover -s src-python/tests -p test_stream.py -v
ruff check src-python/python src-python/tests src-python/scripts
ruff format --check src-python/python src-python/tests src-python/scripts
cargo fmt --manifest-path src-python/Cargo.toml -- --check
cargo clippy --manifest-path src-python/Cargo.toml --all-targets -- -D warnings
cargo test -p insta360-rs --all-features
```

`unittest` does not compile Rust. Rebuild after changing native code, then start
a new interpreter. The packaging runner prevents accidental use of an old
extension or a wrapper imported directly from the source tree.

## Pre-commit hooks and CI

[`src-python/.pre-commit-config.yaml`](../.pre-commit-config.yaml) adapts the
general file checks, Ruff lint/format, Markdown formatting, and Bandit checks
from
[SpeleoDB's configuration](https://github.com/OpenSpeleo/SpeleoDB/blob/master/.pre-commit-config.yaml).
Ruff uses this package's `pyproject.toml` and Python 3.10 language target;
isolated hook tools use Python 3.14. Bandit scans the shipped wrapper and build
helpers. Python tests run separately from pre-commit, through the manual
commands above or the dedicated CI jobs.

Run the Python hooks without building or installing the extension:

```sh
prek -C src-python run --all-files
```

Root `prek run --all-files` also discovers this nested configuration. Hooks
execute relative to `src-python`, so the same command works from either project
directory. If prek cached discovery before this file existed, add `--refresh`.
Ruff and formatting hooks can rewrite files; review changes and rerun them.

`scripts/run-tests.py` requires an installed compiled extension, `ffmpeg` and
`ffprobe` on PATH, software HEVC support in the linked libraries, and a nonempty
test suite. Missing prerequisites fail instead of turning media coverage into
skips. Rebuild the extension after Rust changes before running tests.

CI's Full prek job runs both hook configurations. The common Tests matrix runs
`scripts/run-tests.py` against a repaired wheel on Python 3.14 for Linux, macOS
ARM64/x86_64 and Windows. Additional Python 3.10–3.13 jobs test the same Linux
wheel retained as `python-test-dist-linux-x86_64`, avoiding a duplicate 3.14
run. All test suites run in `ci.yml`. After a matching version tag passes CI,
`release.yml` builds fresh Linux, macOS, and Windows distributions. Release
performs build and repair checks without rerunning the runtime suites or
publishing CI's test wheel.

## Contract coverage

| Tests                   | What they verify                                                                                                                                                                             |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `test_public_api.py`    | Real extension import, installed version, exports, exception hierarchy, installed stubs/member/signature parity, every wrapper's arguments/defaults/return/error propagation.                |
| `test_config.py`        | All enum members, constructor/preset defaults, property mutation, dimensions and native type/range validation, capabilities and adapter fields.                                              |
| `test_probe.py`         | INSV framing/metadata, camera aliases, optional values, timing, trailer/track/calibration/motion fields, path discovery, defensive result copies and typed errors.                           |
| `test_extract.py`       | V2/V3 tails, records/streams/audio, decoded metadata and manifest, original bytes and packet hashes, paired inputs, destination safety and rollback.                                         |
| `test_stream.py`        | Descriptors, independent readers, packet payload/flags/side data against FFprobe, RGB colors, PTS/time bases, B-frame draining and seeks, EOF recovery, lifetime and concurrent access.      |
| `test_exports.py`       | Selection/interval validation, native errors, calibrated CPU PNG/JPEG/HEVC output, sorting/deduplication, scaling/quality/color/stabilization, underwater preset, atomic output and cleanup. |
| `test_jobs.py`          | Async frame/video success, progress/backend/warnings, result consumption, cancellation, simultaneous waiters, GIL release, configuration snapshots and GPU-unavailable fallback.             |
| `test_stabilization.py` | Camera-clock/exposure alignment, stabilization across seeks, rolling-shutter policies, and malformed motion metadata.                                                                        |

FFmpeg creates short two-track recordings with distinguishable colors/patterns,
AAC audio, and B-frames. Helpers append generated INSV metadata and calibration.
FFprobe independently checks stream/packet properties; image and video output is
decoded and inspected. Byte comparisons verify preservation and frame selection
without depending on encoder output being identical across FFmpeg releases.

Path assertions require absolute paths and use `Path.samefile()` to check file
or directory identity, including Unicode filenames. Rust's canonical Windows
paths can retain the `\\?\` prefix and expand short names, so their spelling
need not match Python's `Path.resolve()` output. Keep the native paths intact so
extended Windows paths remain usable.

Video success coverage needs a software HEVC encoder in the linked FFmpeg.
Audio-copy tests exercise both synchronous and asynchronous exports with default
and explicit `COPY`, compare compressed packet hashes and rational PTS/DTS with
FFprobe, and repeat clipped exports with sub-millisecond A/V offsets. They also
compare stored sample durations with MP4 edit lists disabled, verifying packet
hashes align with the normal playback view used for timestamp checks. This
avoids FFprobe 6.1's last-packet duration calculation subtracting the initial
audio delay from the packet duration; it does not relax the duration or A/V
offset checks. They cover ALAC, silent-source warnings, and unsupported AC3
rejection with `DROP` as an explicit fallback. Fixture generation therefore also
needs FFmpeg's native ALAC and AC3 encoders. Capability-dependent tests report
unavailable hardware explicitly. A full validation run must include software
HEVC success tests; a run with skips does not prove video export.
GPU-unavailable/fallback behavior is testable on CPU hosts. GPU and
hardware-encoder qualification also needs suitable devices and the platform
release matrix.

These fixtures establish Python packaging, FFI contracts, and deterministic API
behavior. They do not establish real-camera stitch quality, every codec's
behavior, cross-platform GPU correctness, or independence from the host's shared
libraries. See the broader [Rust test strategy](../../docs/testing.md) and
[wheel release qualification](../../docs/packaging.md#python-wheels).

## Housing and restoration coverage

Rebuild the extension after calibration or processor changes. The full suite
checks all housing/environment/accessory enum mappings, strict option controls,
read-only probe and export optical reports, and actual Legacy/AI image exports.
Each of four AI styles executes inference; independent selected-image exports
must reset temporal state, and zero restoration strength preserves exact output.
CI requires the AI engine on every platform through
`INSTA360_RS_REQUIRE_UNDERWATER_AI=1`. Linux (Python 3.10–3.14) and Windows
(Python 3.14) require a usable GPU through `INSTA360_RS_REQUIRE_GPU=1`; software
Vulkan and D3D12/WARP satisfy those checks. macOS runs GPU tests when Metal is
available. The runner reports adapter availability in the job summary, so a
successful macOS run without an adapter does not establish GPU execution.
Installation alone is not the matrix test.

Source and all-feature builds need the pinned MNN prefix in `MNN_ROOT`; see
[installation](installation.md). Plain Rust binding tests omit wheel-only
`extension-module`, as before.

On macOS, the Python crate's `build.rs` reserves Mach-O load-command space for
wheel repair. Delocate replaces short `@rpath` dependencies with longer paths to
the bundled libraries; without linker padding, the x86_64 wheel can fail repair
after compiling successfully. CI builds and repairs wheels on both macOS
architectures to check this packaging contract.
