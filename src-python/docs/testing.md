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

1. Requires Cargo, `ffmpeg`, and `ffprobe`, then builds a wheel and source
   archive using PEP 517 build isolation.
2. Verifies stable ABI tags, distribution metadata, license/notice files, native
   module, type stubs, and `py.typed`; rejects leaked development artifacts.
3. Checks that the source archive includes docs, tests, build scripts, core Rust
   sources, and byte-identical embedded assets.
4. Installs the wheel without dependencies in a fresh virtual environment and
   runs all tests from outside the checkout with Python isolated mode (`-I`).
5. Rebuilds a wheel from the extracted source archive and repeats the isolated
   installation and suite using the tests shipped in that archive.

Build artifacts and test environments use temporary directories; Cargo output is
cached in `src-python/target/python-tests`, or your `CARGO_TARGET_DIR` if set.
The interpreter that launches the runner needs the `build` package. Fresh test
interpreters need standard-library `venv` and `ensurepip` support.

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

CI's Full prek job runs both hook configurations. The dedicated Python 3.10–3.14
jobs use `scripts/run-tests.py` against the repaired distribution wheel. These
checks and runtime tests run on Linux; macOS/Windows jobs build wheels.

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

Video success coverage needs a software HEVC encoder in the linked FFmpeg.
Capability-dependent tests report unavailable hardware explicitly. A full
validation run must include software HEVC success tests; a run with skips does
not prove video export. GPU-unavailable/fallback behavior is testable on CPU
hosts. GPU and hardware-encoder qualification also needs suitable devices and
the platform release matrix.

These fixtures establish Python packaging, FFI contracts, and deterministic API
behavior. They do not establish real-camera stitch quality, every codec's
behavior, cross-platform GPU correctness, or independence from the host's shared
libraries. See the broader [Rust test strategy](../../docs/testing.md) and
[wheel release qualification](../../docs/packaging.md#python-wheels).
