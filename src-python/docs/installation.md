# Installation and native builds

## Requirements

- CPython 3.10 or newer; the extension targets the `cp310-abi3` stable ABI.
- A Rust toolchain satisfying `rust-version` in the Cargo manifests, a C/C++
  toolchain, and libclang for FFmpeg bindgen.
- FFmpeg development headers/libraries for `avcodec`, `avformat`, `avutil`, and
  `swscale`, discoverable by `pkg-config` or the ffmpeg-sys build environment.
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
maturin develop --manifest-path src-python/Cargo.toml
python -m unittest discover -s src-python/tests -v
```

On Windows, activate `src-python\.venv\Scripts\Activate.ps1` in PowerShell; the
remaining commands use the same paths with forward slashes. A virtual
environment must be active for `maturin develop`. Rebuild after editing Rust;
Python changes are immediately visible with a development install.

The binding Cargo manifest depends on the Rust crate at `..` and enables its
`media` and `gpu` features. Keep `src-python` adjacent to the core crate's
`src-rust` directory when building a checkout. The package and import names
remain `insta360-rs` and `insta360_rs` respectively.

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
