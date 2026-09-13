# Video AI stitching model

The optional `ai-stitching` feature executes Studio's original video model 213
through the independently built, pinned MNN 3.6.1 CPU engine. It shares model
decoding, tensor ownership, validation and native error handling with
`underwater-ai`. Neither feature loads Insta360 executable code. A model session
belongs to one render planner and is reused; inference is single threaded and
uses high precision. GPU rendering does not change the model execution backend.

`seam_ai::SeamModel::new()` verifies the original and decoded model identities
before creating a session. `infer()` accepts four NCHW float32 tensors and
returns both directional flow tensors. It rejects malformed tensor lengths,
nonfinite inputs, grayscale outside 0–255 and masks outside 0–1. The native
adapter checks tensor types/shapes, transfers through reusable CAFFE-layout host
buffers, verifies all output values and publishes results only after both
outputs succeed. No model data is downloaded or discovered in the installed app
at runtime.

| Tensor               | Shape          | Meaning                                               |
| -------------------- | -------------- | ----------------------------------------------------- |
| `input_0`, `input_1` | `[1,3,544,64]` | Byte grayscale intensity replicated into three planes |
| `mask_0`, `mask_1`   | `[1,1,544,64]` | Source support, 0–1                                   |
| `flow_f`, `flow_b`   | `[1,2,136,16]` | Horizontal then vertical flow, in the output grid     |

Input preparation is part of stitching correctness. These are grayscale 0–255
tensors, with no RGB normalization or mean subtraction. The native model adapter
resizes incoming grayscale and mask belts with linear interpolation, scales
masks by 1/255, and copies each grayscale byte into all three planes. Its flow
postprocessor interleaves x/y with unit scale. The native higher layer resizes
flows and scales their horizontal and vertical components by the corresponding
grid-size ratios. A positive forward horizontal flow tracks an image feature
displaced right in the second image; the backward flow has the opposite
direction. Synthetic translations confirm both channel directions, while also
showing that estimated magnitude depends on texture.

## Native pipeline evidence and qualification boundary

The audit used the installed Insta360 Studio 5.9.10 macOS ARM64 image,
`Contents/Frameworks/libstudio_worker.dylib`, read only. Named symbols and
addresses below identify the evidence, rather than implying that the application
is required at runtime:

| Symbol                                                | ARM64 address            | Observed contract                                                                                  |
| ----------------------------------------------------- | ------------------------ | -------------------------------------------------------------------------------------------------- |
| `MNNAIFlowV2::EstimateWithMask` (four inputs)         | `0x20515e0`              | Linear resize to fixed input size; grayscale scale 1, mean 0; mask scale 1/255                     |
| `FlowProcess::PreprocessBelt` and worker              | `0x2048010`, `0x204ab90` | CV_8UC1 byte samples replicated across three NCHW planes                                           |
| `FlowProcess::PreprocessMask` and worker              | `0x20484ec`, `0x204ae68` | Grayscale mask bytes converted to floating coverage                                                |
| `MNNAIFlowV2::CalcFlows`                              | `0x2051150`              | Two directional outputs; unit postprocessor scales                                                 |
| `FlowProcess::PostprocessFlow` and worker             | `0x2048e6c`, `0x204b08c` | Planar x/y output copied without sign reversal                                                     |
| `SeamlessBlenderImpl::setImage` (pair)                | `0x143e3a0`              | Three-channel source converted by OpenCV BGR2GRAY (BT.601) before belt extraction                  |
| `SeamlessBlenderImpl::init`                           | `0x141f644`              | Vertical belt angular domain −110° through 290°; real-time modes can change working size and scale |
| `SeamlessBlenderImpl::getLeftLine2SphereMap` and body | `0x1427fd0`, `0x1445b9c` | Cylindrical cross-seam projection and periodic angular belt coordinate                             |
| `SeamlessBlenderImpl::setAIFlowMap`                   | `0x1432e88`              | Separate flow resizing/component scaling and further tracking/compositing                          |

The native cylindrical projection uses a horizontal coordinate proportional to
`−X / sqrt(Y² + Z²)` in the rotated belt frame and an angular vertical
coordinate derived from `atan2(−Y,Z)`. It is not a strip sampled uniformly in
colatitude. The base working dimensions are 180×3240, with speed levels scaling
those to 60×1080 or 90×1620. The 400° angular extent includes repeated coverage;
the model adapter itself does not append 16-row padding to a 512-row strip.

The portable video profile fixes the native speed-0 working dimensions at
60×1080 and scale 1. Its azimuth step is `400/1079` degrees and cylindrical
focal length is `1079/400 * 180/pi` pixels. This gives a cross-seam half-angle
`atan(29.5/focal)`, about 10.8 degrees, derived from the native projection
rather than an independent guessed angular width. Native speed-1 and speed-2
choices change working resolution and slightly change endpoint sampling, while
real-time speed flags can change angular scale as well. The portable profile
does not inherit those app runtime flags. A fixed analysis profile also keeps
thumbnail and final export geometry independent of requested output size.

The lens-0 orientation supplies a stable reference frame: local `[x,y,z]` maps
to cylindrical `[X,Y,Z]=[z,−y,x]`, so the lens optical axis is the cylinder
axis. This is the portable profile's orientation convention, not a claim that
every Studio camera profile chooses the same azimuth origin. The 400-degree
domain provides two model estimates in the repeated 40-degree sector;
edge-weighted averaging prevents an abrupt periodic cut. Inference proposals
need near-complete source support, local texture, forward/backward consistency
and photometric agreement. The photometric test requires improvement or accepts
a mean absolute error of at most two intensity levels across its nine samples,
allowing limited byte-rounding noise. Failed regions retain calibrated geometry.
Accepted proposals are converted to the shared spherical correction field, and
the common renderer validation rejects foldovers and excessive displacement.

Source RGB samples use BT.601 grayscale and byte rounding; non-RGB sources use
the same mask-aware color reconstruction first. This establishes one portable
color convention across source formats rather than depending on an app's
decoded-Y path. Native byte/resize rounding can differ by an intensity unit. The
implementation is an independently tested video stitching profile, not a
pixel-equivalence claim for all Studio runtime settings.

`seam_ai::unavailable_reason()` is the inexpensive authority used by capability
reports and bindings. A compiled engine is separate from completed stitching
qualification. The implemented video profile passes the tensor and rendered
geometry checks below. Missing features remain explicit capability errors
without selecting a different algorithm silently. Camera/image release
qualification still requires rendered comparisons in addition to the numerical
contracts.

## Original resources

The new `insta360-rs-data-ai-stitch-video` package holds eight original files:
model 213 and its complete seven-file CoreML group, including the two original
zero-length placeholders. Source release, original paths, stored lengths and
SHA-256 hashes are in the aggregate and subset `model-bundle.json` manifests.
The package adds 5,157,829 raw bytes. Keeping it separate preserves the strict
10,000,000-byte compressed archive limit of each publishable crate.

| Source relative to Studio `Contents/data/models/`                 | Purpose                        |
| ----------------------------------------------------------------- | ------------------------------ |
| `asqXMbz4bMndz3mzYzQUXIMQmM.ins`                                  | Original wrapped MNN model 213 |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/analytics/coremldata.bin` | Original CoreML analytics      |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/coremldata.bin`           | Original CoreML metadata       |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/ins_metadata.json`        | Original encoded metadata      |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/ins_model.mil`            | Original encoded model program |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/metadata.json`            | Original empty placeholder     |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/model.mil`                | Original empty placeholder     |
| `asqXMbz4bMndz3mzYzsn4VUzErnVN.mlmodelc/weights/weight.bin`       | Original weights               |

The MNN payload has SHA-256
`a855aa2106101edd5c28ed0922d21223c2e420c3520917cbd7d371b7437e0cce`; the verified
in-memory decoded model has SHA-256
`2fc715cb0b4a8de1b079d74079039cfa2f32e22b4db182e7cff0014d603257cf`. CoreML data
is preserved for reference qualification; it is not an implemented CoreML
inference provider. Its encoded `ins_` files cannot simply be handed to Apple's
loader as an ordinary compiled model.

Studio's selector distinguishes video model 213 from image model 214 and older
211/iOS V22 models. Extracting a JPEG from video still uses video-frame
geometry; model 214 is not required for that workflow and was not copied. The
pre-existing 51 payloads were checked against their recorded source bytes and
hashes without finding corruption. No standalone housing-exclusion mask or
per-camera calibration asset was missing from the repository: those resources
are generated from recording calibration and optical profiles. Adding AI model
files does not repair incorrect housing selection or mask projection.

## Verification

`tests/reference/seam-model-reference.cpp` invokes the pinned MNN Interpreter
directly without the Rust adapter or C shim. Its seven deterministic grayscale
cases cover zero motion, signed horizontal/vertical translations, diagonal
translation and unequal masks. The checked-in fixture contains every output
float. Rust compares all values with absolute and relative tolerances, verifies
session reuse, tests known flow directions, and exercises invalid tensors and
corrupt/unknown model identities. Existing underwater full-tensor references
also cover regression of the shared engine.

The AI producer tests independently check cylindrical coordinates, half-pixel
resize sampling, quarter-grid flow scaling, directional conversion, occlusion
rejection and a real model inference through confidence filtering into the
spherical field. Rendered synthetic spherical charts require more than 5%
accepted correction coverage and more than 10% geometric error reduction, with
deterministic repeated plans. Rotated and resized CPU/Metal output satisfies the
shared parity bound (mean absolute error below 0.2 and maximum at most 8 byte
levels). These checks passed on macOS ARM64. They cover the portable video
profile; cross-platform execution remains part of the release matrix.

See [reference provenance](../tests/reference/README.md) for the exact recipe,
fixture format and regeneration command. These numerical checks establish the
adapter's correctness independently of camera/rendering qualification and visual
comparison against Studio output.
