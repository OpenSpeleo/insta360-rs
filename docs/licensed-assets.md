# Licensed asset architecture

`insta360-rs` separates recording geometry from reusable licensed resources.
This boundary keeps the portable template stitcher functional without a vendor
runtime while providing original Insta360 and Studio assets directly from the
crate. Applications can also supply their own qualified bundles.

## Implemented runtime consumer

The X5 I-Log CUBE now runs in CPU/GPU image and video exports through
`StitchConfig::color_conversion`. Auto selection recognizes explicit recorded
I-Log metadata; Preserve retains the recorded encoding. All three CUBEs also
support direct RGB/GPU use. See [runtime asset usage](asset-usage.md) for
selection, validation, and the assessment of every bundled resource class.

## Authority and precedence

The current/original offset embedded in an INSV is the authority for the
physical camera that recorded it. Intrinsics, distortion, principal points,
extrinsics, crop/layout metadata, and an already-converted accessory lens type
must not be replaced by a generic camera calibration.

Optical setup selection follows this order:

1. explicit caller selection;
2. explicit recorded accessory/offset state;
3. the current offset's encoded lens type;
4. an ambiguity error. Image classifiers are not run during optical selection.

Registry data supplies documented fallbacks such as lens-family FOV and blend
angles. Recorded values take precedence whenever the media provides them.

## Bundle boundary

Licensed files are checked in under `data/*/assets/`, embedded by five data
crates, and served by `BundledAssetProvider`. Applications can also supply
external bundles through `DirectoryAssetProvider` or `InMemoryAssetProvider`. A
bundle manifest records:

- its schema version and stable bundle identifier;
- the `insta360-rs` asset-manifest schema version;
- every asset's logical identifier, role, relative path, byte length, and
  SHA-256 digest;
- its exact raw-source digest, source release/path, and any canonicalization;
- complete multi-file model groups; and
- explicit qualification plus optional camera, lens, and target tags.

Qualification is default-deny. A bundled payload is `Unqualified` even when its
camera family is known. An application may mark a complete model group
`Qualified` only after its preprocessing, execution, and outputs pass the
declared camera/lens/target matrix. Empty compatibility dimensions are wildcards
only after that explicit gate. Explicit underwater restoration verifies its
fixed resource group without claiming camera/platform qualification or automatic
model selection; it runs only after the caller selects Legacy or AI.

The application constructs an asset provider and selects an explicit policy:

- `Disabled`: never load the asset;
- `Automatic`: use a qualified, compatible asset when present; absence or
  incompatibility retains the deterministic fallback;
- `Required`: return a typed error when it is absent, incompatible, or corrupt.

Both policies fail closed for an invalid manifest, I/O failure, length mismatch,
or digest mismatch. `Automatic` never hides corruption by falling back.

Directory providers resolve paths below the configured bundle root. All
providers reject absolute paths and parent directory traversal. The complete
payload is length- and digest-checked before a consumer receives it.

Hashes bind payloads to the manifest and detect corruption; they do not
authenticate a manifest that an attacker can also replace. Applications must
ship the manifest inside their trusted installation or authenticate it through
their own signed-update/package boundary.

`ModelBundle::load_verified_for` additionally enforces exact camera, lens ID,
and Rust target compatibility before it touches the provider.
Vendor-release/firmware version bounds remain audit metadata because vendor
versions are opaque, not a SemVer ordering contract.

## Direct bundled access

`src-rust/assets/model-bundle.json` describes 51 stored payloads from 50
original resources copied from MediaSDK 3.1.5, CameraSDK 2.1.8, iOS SDK 1.10.4,
and Insta360 Studio 5.9.10. All whole-file payloads are copied byte-for-byte
with identity normalization and equal source/stored hashes. Model 197 is split
into contiguous original-byte parts with both part and whole-source hashes
verified. Identical platform copies and Studio classifiers already present in
the vendor distributions are stored once. Sample recordings and optional Android
regression images are outside this runtime bundle.

`data/.gitattributes` disables Git newline conversion for every data crate's
`assets/` tree, including text payloads such as the underwater style manifest.
Keep new data crates under this rule: converting a vendor file's LF newlines to
CRLF changes its length and digest and makes the bundled resource unusable.
`scripts/ci/test_asset_checkout.py` checks every manifest payload after a
temporary Git checkout with `core.autocrlf=true`, so this Windows checkout
contract is also tested on Linux and macOS.

```rust
use insta360_rs::assets::{AssetPolicy, BundledAssetProvider, OpenCvLinearSvm};

let bundle = BundledAssetProvider::manifest()?;
let asset = bundle.load_verified(
    &BundledAssetProvider,
    "camera-accessory-svm-0db3a7a0-xml",
    AssetPolicy::Required,
)?.ok_or("missing bundled model")?;
let svm = OpenCvLinearSvm::parse_xml(&asset.bytes)?;
```

`BUNDLED_MANIFEST` also exposes the manifest JSON. Assets are embedded in the
library and accessed directly. Consumers retain explicit asset policies and
compatibility checks; model files alone do not change stitching behavior or
advertise inference support.

Studio contributes three I-Log-to-Rec.709 `.cube` LUTs (Ace Pro 2, Luna, and
X5), ISO/FOV sharpening parameters, and two additional OpenCV SVM payloads.
Their original filenames and source paths are recorded in the manifest. The
sharpening parameters use the `Other` asset kind; their consumer is not yet
implemented.

## Supplied resource classes

### Camera and optical profiles

The vendor distributions do not contain a standalone factory-calibration
database or a set of ready-made warp maps. Per-unit calibration stays in the
INSV. Camera aliases, lens identifiers, fallback FOV/blend angles, supported
offset generations, optical-setup mappings, and mask recipes are normalized into
a reviewed crate-owned registry with provenance. Source layout, crop, rotation,
and codec remain per-recording metadata.

CameraSDK `camera_conf_*.json` files describe capture capabilities and setting
dependencies. They can seed mode validation but must not choose stitch geometry.

### Accessory classifiers

The `cameraaccessory/*.xml` and `coolingshell/*.xml` payloads are OpenCV linear
SVM/SVR models. Their numeric model representation is portable and can be parsed
without OpenCV. A model is not a usable classifier by itself: camera- specific
ROI selection, resizing, color conversion, HOG/feature ordering, and
normalization must also match the vendor implementation. Until that
preprocessing has a labeled golden corpus, the crate exposes parsed model data
and the exact linear `decision_value` for caller-supplied feature vectors, but
no successful image accessory-detection capability.

### AI seam flow

The desktop `ai_stitcher.ins` is an opaque vendor container. The iOS
distribution also contains an inspectable compiled CoreML/Espresso graph whose
inputs are two RGB seam strips and two masks and whose outputs are
forward/backward flow. The bundle includes all eight compiled-model constituents
and records one ordered `AssetGroupDescriptor`.
`ModelBundle::load_verified_group_for` returns the group only after every member
passes integrity checks; partial models are never returned.

AI seam flow is separate from optical calibration and masking. It cannot repair
an incorrect lens projection. Because dynamic flow may change feature ownership
between frames, it remains opt-in and requires photogrammetry stability metrics.

### Radiometric and restoration models

ColorPlus, defringe, deflicker, and denoise are optional image-processing
stages. They must declare their changes independently from stitch geometry.
Underwater workflows should default them off until matching, temporal
consistency, and reprojection tests prove they do not damage reconstruction.

## Qualification

Each stored asset records its raw-source digest, stored digest, source release,
source path, and canonicalization. Release qualification compares intermediate
outputs, not only the final encoded frame:

- camera/accessory selection against labeled recordings;
- AI forward/backward flow tensors against a vendor/CoreML oracle;
- CPU and wgpu LUT results within one 8-bit code value;
- defringe prediction maps before compositing;
- CPU/GPU projected source coordinates, masks, and seam weights;
- straight-line curvature, overlap reprojection error, temporal angular
  stability, feature-match retention, and reconstruction reprojection error.

Possession of a readable asset is not reported as algorithm support. A feature
becomes available only when its loader, preprocessing, execution, and golden
qualification are all present.
