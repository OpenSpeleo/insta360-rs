# Packaging

## Rust crate and bundled resources

The core Rust library and CLI live under `src-rust/`; the root `Cargo.toml`
declares their paths explicitly. Cargo-based builds, tests, formatting, and CI
resolve these paths through the manifest. The Python extension lives in
`src-python/`, with its Rust glue in `src-python/src/`. These packages and both
data crates form one Cargo workspace with a shared root `Cargo.lock`.

The main crate contains source, tests, examples, documentation, the project
license, the notice file, and the combined asset manifest. Two unconditional
data dependencies contain all 41 licensed Insta360/Studio payloads:

| Crate                          | Repository directory | Contents                                                                                           | Files |
| ------------------------------ | -------------------- | -------------------------------------------------------------------------------------------------- | ----: |
| `insta360-rs-data-core`        | `data/core/`         | Three LUTs, camera settings, accessory classifiers, stitching models, catalog, sharpening settings |    36 |
| `insta360-rs-data-enhancement` | `data/enhancement/`  | ColorPlus, deflicker, two defringe variants, JPEG denoise                                          |     5 |

The split keeps each compressed `.crate` below the strict 10,000,000-byte
publication budget. Verified 0.1.0 candidates measured approximately **0.34 MB**
for the main crate, **6.58 MB** for core data, and **8.66 MB** for enhancement
data. The payloads still occupy 21,814,090 bytes (about 20.8 MiB) uncompressed;
splitting changes package boundaries, not total embedded data or implemented
capabilities. Supplied recordings, vendor executables, and vendor libraries
remain outside the packages.

The dependency-free `no_std` data crates embed original files with
`include_bytes!` and expose immutable `PAYLOADS` slices. `BundledAssetProvider`
searches both slices and reads the unchanged combined `model-bundle.json` with
`include_str!`. The embedded resources are available directly at runtime.
Applications using the bundled provider include these payloads in their binary;
loading an asset allocates only that requested payload. Existing directory and
in-memory providers remain available for application-controlled replacements.

The main crate inherits both `path` and exact `version` dependency requirements
from the root `[workspace.dependencies]` table. Local development resolves the
checked-in data crates; published consumers resolve the matching registry
versions. Exact pins bind the library's manifest to the expected payload set.
The data crates do not depend on the library. Their own subset manifests
preserve standalone provenance, and the library's tests require their union to
equal the public manifest with no duplicate payload paths.

Every data package includes its own license and notice. The data bytes remain
unchanged, including line endings and the complete eight-file CoreML group. This
move does not implement any additional inference pipeline.

`BundledAssetProvider::manifest()` validates descriptors and complete model
groups. The existing `load_verified` methods check payload length and SHA-256.
Runtime selection through `load_verified_for` and `load_verified_group_for`
continues to require explicit algorithm qualification; copying assets does not
qualify their preprocessing or inference.

The implemented color consumer uses the bundled X5 I-Log CUBE in image/video
exports and supports all three CUBEs through explicit RGB/GPU APIs. See
[runtime asset usage](asset-usage.md) for selection rules and the other
resources' remaining integration requirements.

Git ignores `.DS_Store` at every depth. Cargo's explicit `include` lists bypass
Git ignore rules, so each crate ends its list with `!**/.DS_Store`. Python
packaging excludes the same filename separately.

Every copied file preserves its original bytes, including text line endings.
Pre-commit excludes vendor payload directories from whitespace and EOF rewrites.
The vendor's `catalog/model_info.json` is encoded text despite its suffix.
Vendor payload directories are excluded from the common file hooks, including
JSON parsing; bundled-asset tests validate their integrity, and JSON consumers
validate the resources they use. The bundle manifest remains covered by JSON
checks. Git attributes alone do not stop formatters from changing working-tree
bytes. Source paths and releases are recorded in the manifest, and source/stored
hashes match. The [literal copy inventory](sdk-provenance.md) describes the
vendor and Studio sources. The project Apache-2.0 license covers
project-authored code; the original Insta360 data resources retain their vendor
licensing.

Before publishing:

```sh
cargo package --manifest-path data/core/Cargo.toml --list
cargo package --manifest-path data/enhancement/Cargo.toml --list
cargo package -p insta360-rs --list
python3 scripts/ci/check-packages.py --all-features
```

The verifier builds and tests extracted archives, including both data crates,
and rejects any archive at or above 10,000,000 bytes. See [releases](RELEASE.md)
for the initial unpublished-dependency check and publication ordering.

## Python wheels

The Python package uses PyO3's Python 3.10 stable ABI, so one repaired wheel per
OS/architecture supports Python 3.10 and newer. Releases build manylinux_2_28
x86_64, macOS ARM64, macOS x86_64, and Windows x86_64 wheels. All tests run on
Linux; macOS and Windows runners build and repair wheels only. See [CI](CI.md)
for checks and [releases](RELEASE.md) for registry configuration and
publication.

Maturin includes the library and both transitive data crates in each source
distribution. The Python distribution verifier checks all 41 payload hashes,
package manifests, and notices before rebuilding and testing an installed wheel.
The Rust package split does not reduce wheel size or require users to install
separate Python data packages. Maturin derives the Python version and author
from the inherited Cargo metadata; its source archives include the workspace
metadata needed to rebuild independently.

The Python extension enables both `insta360-rs/media` and `gpu`.
`ProcessingBackend.AUTO` therefore attempts the target platform's portable GPU
provider and reruns the complete operation on CPU after a typed GPU failure. Do
not label a wheel release-qualified for GPU until its Metal, D3D12, or Vulkan
path passes the corresponding real-X5 and clean GPU-less-host matrix.

Release wheels must bundle the same capability-pruned shared FFmpeg runtime used
at link time and repair loader paths with the platform-native wheel tool:
`delocate` on macOS, `delvewheel` on Windows, and `auditwheel` on Linux. A clean
container or VM must install each wheel, import `insta360_rs`, run
`capabilities()`, and probe a generated fixture before publication.

FFmpeg's license/configuration and transitive shared-library notices must ship
with wheel metadata. The Rust dependency includes the licensed resource bundle;
the Python API does not yet expose the asset provider or invoke model inference.
Vendor runtime libraries are not linked or bundled. Wheel packaging includes the
crate `NOTICE.md` or an equivalent notice carrying the same resource provenance
and vendor-license attribution.
