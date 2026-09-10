# Installation and native builds

## Requirements

- Standard, GIL-enabled CPython 3.10 or a later Python 3.x version; the
  extension targets the `cp310-abi3` stable ABI. See
  [wheel compatibility](#wheel-compatibility).
- A Rust toolchain satisfying `rust-version` in the Cargo manifests, a C/C++
  toolchain, and libclang for FFmpeg bindgen.
- FFmpeg development headers/libraries for `avcodec`, `avformat`, `avutil`, and
  `swscale`, discoverable by `pkg-config` or the ffmpeg-sys build environment.
- CMake and the pinned independent MNN CPU prefix for `underwater-ai`, enabled
  by the Python wheel configuration.
- `ffmpeg` and `ffprobe` executables for the integration tests. Their fixture
  generator needs lavfi, MPEG-4 video, AAC, and MP4 support. Software HEVC
  export tests require a `libx265` encoder in the FFmpeg libraries linked to
  Python.

The library links to FFmpeg directly. An `ffmpeg` executable alone does not
supply development headers or make a particular encoder available to the
extension. See the Rust [source installation notes](../../README.md#from-source)
for platform-specific FFmpeg prerequisites.

## Local development

From the repository root on macOS/Linux:

```sh
python3 -m venv src-python/.venv
. src-python/.venv/bin/activate
python -m pip install 'maturin>=1.9,<2' build ruff
python scripts/ci/build-mnn.py --output .cache/mnn
export MNN_ROOT="$PWD/.cache/mnn"
maturin develop --locked --manifest-path src-python/Cargo.toml
python -I -X faulthandler src-python/scripts/run-tests.py
```

On Windows, activate `src-python\.venv\Scripts\Activate.ps1` in PowerShell; the
remaining commands use the same paths with forward slashes. Set
`$env:MNN_ROOT = "$PWD/.cache/mnn"` instead of the shell export command. A
virtual environment must be active for `maturin develop`. Rebuild after editing
Rust; Python changes are immediately visible with a development install.

The binding Cargo manifest depends on the Rust crate at `..` and enables its
`media` and `gpu` features. Maturin also enables `underwater-ai`, which links
the independently compiled MNN engine and includes its notices in release
wheels. For a checkout without AI, explicitly override Maturin features or build
a Rust media/GPU consumer without `underwater-ai`. Keep `src-python` adjacent to
the core crate's `src-rust` directory when building a checkout. The package and
import names remain `insta360-rs` and `insta360_rs` respectively.

## Build installable artifacts

```sh
python -m build --sdist --wheel --outdir src-python/dist src-python
python -m pip install --force-reinstall src-python/dist/<wheel-filename>.whl
```

PEP 517 build isolation installs maturin from `build-system.requires`. The
source archive contains the core Rust source, embedded resources, Python source
and stubs, tests, and documentation. Maturin rewrites path dependencies so an
extracted source archive can build independently of the original checkout. The
wheel contains `_native`, the Python wrapper, `__init__.pyi`, `py.typed`, and
the license/notice metadata. It has no Python runtime dependencies.

Use the [complete test runner](testing.md) to inspect both artifacts, rebuild
the source archive, and exercise installed FFI on multiple interpreters.

## Wheel compatibility

For example, `insta360_rs-0.1.0-cp310-abi3-win_amd64.whl` targets standard,
GIL-enabled CPython 3.10 and later Python 3.x versions on Windows x86-64.

| Filename tag | Meaning                                                                                  |
| ------------ | ---------------------------------------------------------------------------------------- |
| `cp310`      | Minimum CPython version: 3.10 when combined with `abi3`.                                 |
| `abi3`       | CPython's stable ABI, allowing the same wheel to work across later CPython 3.x versions. |
| `win_amd64`  | Windows with 64-bit x86 Python, on Intel or AMD processors.                              |

`cp310` does not restrict this wheel to Python 3.10: `abi3` makes 3.10 the
minimum compatible version. The same wheel can therefore be installed on
standard CPython 3.11, 3.12, 3.13, 3.14, and subsequent Python 3.x versions
without building a separate wheel for each interpreter version. See the
[wheel tag specification](https://packaging.python.org/en/latest/specifications/platform-compatibility-tags/)
and
[PyO3's minimum ABI version explanation](https://pyo3.rs/v0.29.0/building-and-distribution.html#minimum-python-version-for-abi3-and-abi3t-builds).

This is intentional: [`Cargo.toml`](../Cargo.toml) enables PyO3's `abi3-py310`
feature, and [`pyproject.toml`](../pyproject.toml) declares
`requires-python = ">=3.10"`. The stable ABI supplies compatibility across
CPython versions; it does not remove operating-system, architecture, or
interpreter-implementation requirements.

This particular Windows wheel does not target:

- macOS or Linux, which require their own platform wheels;
- 32-bit Python or native ARM64 Python;
- PyPy or other non-CPython interpreters;
- free-threaded CPython builds, which cannot load `abi3` wheels and require a
  compatible build with different ABI tags. See
  [PyO3's ABI compatibility details](https://pyo3.rs/v0.29.0/building-and-distribution.html#py_limited_apiabi3abi3t).

The architecture must match the Python interpreter, not merely the machine's
processor. ABI compatibility also does not establish runtime test coverage for
every platform/version combination; see
[testing](testing.md#pre-commit-hooks-and-ci) for the current validation matrix.

## Runtime portability

A local development wheel can depend on the host's FFmpeg shared libraries.
Successful installation in a fresh virtual environment proves Python packaging
and native loading on that host; it does not prove that libraries are bundled.
Before distributing wheels to other machines, repair their shared-library
dependencies and test on clean hosts as described in
[packaging](../../docs/packaging.md#python-wheels). Target platforms are macOS
ARM64/x86_64, Windows x86_64, and manylinux x86_64.

Project code is Apache-2.0. Bundled resources retain their original licensing;
see [NOTICE](../NOTICE.md) and the
[asset inventory](../../docs/sdk-provenance.md). Redistributed FFmpeg libraries
also need their corresponding third-party notices.

## Troubleshooting

| Symptom                                    | Action                                                                                                               |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- |
| `No module named insta360_rs`              | Install into the interpreter running the tests; use `python -m pip` and print `sys.executable`.                      |
| `_native` fails to load a shared library   | Check FFmpeg discovery at build time and loader paths at runtime. Rebuild or repair the wheel for the intended host. |
| `pkg-config` cannot find FFmpeg            | Install development libraries and expose their `.pc` files with `PKG_CONFIG_PATH`.                                   |
| `MissingCapabilityError` on stitched video | Use `audio=AudioPolicy.DROP`, inspect `capabilities().hevc_encoders`, and review the error message.                  |
| `GpuUnavailableError`                      | Select `ProcessingBackend.CPU` or install a compatible GPU driver; explicit GPU mode does not fall back.             |
| Rust changes seem ignored                  | Rebuild and reinstall the extension, then start a fresh Python process.                                              |
