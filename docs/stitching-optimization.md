# Stitching optimization and source support

Stitching optimization is separate from physical optics, stabilization, color
restoration and execution backend. `SeamMode::Fixed` remains the default; the
application calls this setting **Off**. Off still performs calibrated
projection, housing exclusion, overlap color adjustment and fixed two-band
blending. It does not mean exporting unstitched lenses, and it is not a claim of
Studio pixel parity.

## Portable algorithms

`Dynamic` and `OpticalFlow` estimate correspondence between the current pair's
calibrated overlap images. They do not execute vendor libraries. Their names
identify capabilities, not a promise that their implementation is identical to
Insta360's proprietary algorithms.

| Mode         | Correspondence and processing                                                                                                                                                                                                        |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Fixed / Off  | Existing calibrated rays, static detail weights and source masks; no local warp                                                                                                                                                      |
| Dynamic      | Sparse pyramidal inverse-compositional Lucas–Kanade; mean-normalized patches up to 15×15 on a 16-pixel grid; at most four levels and 15 iterations per level                                                                         |
| Optical Flow | Dense inverse-search patches, overlapping patch aggregation and fixed-iteration variational smoothing; 7×7 patches on a 4-pixel grid, at most four levels and 16 inverse-search iterations per level                                 |
| AI           | Independently built MNN execution of the original Studio video model 213, native cylindrical belt preparation, bidirectional confidence filtering and conversion into the shared spherical correction field; requires `ai-stitching` |

The dense implementation follows the three-stage design described by
[Kroeger et al., Fast Optical Flow using Dense Inverse Search](https://arxiv.org/abs/1603.03590).
Its versioned settings and refinement are this library's implementation, not an
OpenCV or Studio compatibility profile. Unit tests use independent translated
textures and spherical charts rather than snapshots derived from the solver.

Both portable optimizers use an angular belt around the first calibrated optical
axis. The half-height derives from the registered lens FOV and departure of the
two optical axes from opposition. The azimuth has 1024 samples for Dynamic and
2048 for Optical Flow. Belt height follows the same angular sampling, rounded to
eight rows with a minimum of 32. Unsupported overlap wider than 90 degrees is
rejected. Synthetic, unregistered test lenses use an explicit 24-degree belt.

Sparse rows are anchored at the optical seam, so even the narrowest belt has a
central track row. Each Dynamic track chooses the largest fully supported 15×15,
11×11 or 7×7 patch before fitting. This lets narrow valid overlap retain
correspondence without relaxing texture or round-trip confidence thresholds. An
independent 12-row valid-strip regression reproduces the otherwise empty result
when a sparse grid starts only at the upper patch margin.

The source masks and sensor-readout motion also apply to analysis. Untextured
patches, invalid support and forward/backward inconsistency do not acquire a
guessed displacement. Proposals beyond 32 analysis pixels are rejected. Lens
zero remains the geometric reference; lens one is warped within the overlap.
Preparation bakes the latitude taper into the displacement vertices and sets
rejected proposals and the first/last latitude rows to zero. CPU and GPU then
interpolate these final displacements directly. They do not normalize by
confidence or apply a second taper: transitions to calibrated fallback must be
continuous, including across the periodic azimuth cut. Projection after
displacement checks the source masks again. The plan reports accepted
correspondence coverage.

Safety is checked on the composed bilinear map, after taper and confidence
rejection. Its Jacobian determinant is affine inside each cell, so checking all
four corners also bounds the cell interior. Cells below determinant 0.25 request
local contraction toward their mean displacement. This preserves each cell's
translation while reducing its derivatives. A vertex averages requests from its
incident unsafe cells in deterministic Jacobi passes; rejected vertices remain
pinned at zero. Cells containing a pinned vertex contract toward zero. Gathered
requests can only attenuate existing components, preventing increases or sign
reversals from propagating oscillations between cells. A local target of 0.5
leaves room for merging requests and pinning. Requests stay in the safe interval
connected to identity, found by a bounded 32-step bisection. A positive
determinant at both ends alone would miss an intermediate fold.

Combining incident requests can create another unsafe cell, so local contraction
is not itself treated as proof. At most 32 local passes are followed by exact
validation of the quantized vertices. When local repair succeeds, a distant safe
region retains its full correction. An unresolved field receives a final
conservative uniform scale with a rounding margin; unexpected numerical failure
yields an entirely calibrated field. This bounded safety repair can reduce
correction strength, but does not change solver resolutions, iterations or
correspondence thresholds. The quality regressions require both a positive final
map and preservation of a distant safe translation beside an unsafe confidence
boundary.

Independent regressions exercise a constant vertical proposal that folds only
after tapering, rejected-confidence transitions, wrapped azimuth cells and
source-coordinate round trips through the actual renderer mapping. They verify
that already-safe horizontal translations retain their full magnitude.

These modes can change reconstructed geometry. Keep Off for geometry-stable
photogrammetry unless a representative reconstruction comparison establishes
that a particular optimized workflow is acceptable. Better appearance of one
still does not establish better reconstruction or temporal quality.

## AI input and displacement contract

AI uses the original model bytes and an independent MNN CPU engine. It does not
load Studio or SDK runtime libraries. Asset integrity and full output tensors
are checked against a separate direct-MNN reference harness; execution alone is
not treated as proof of correct stitching.

Native producer evidence establishes a cylindrical belt rather than uniform
latitude rows or guessed periodic padding. The portable implementation uses the
normal export speed-zero geometry: 60 columns, 1080 rows, azimuth from -110 to
290 degrees, and focal scale `1079 / 400 * 180 / pi`. The extra 40 degrees
repeat the azimuth cut. A rigid basis places the first calibrated optical axis
normal to the seam plane. This basis is explicit and tested; it is not a claim
that Studio chooses the same azimuth origin for every camera.

Projected pixels become BT.601 byte grayscale. Half-pixel bilinear resizing with
border replication produces the model's 64×544 input; the same grayscale plane
is replicated into three channels without dividing intensities by 255. Coverage
is resized separately and normalized to 0–1. Model output has two planar flow
channels at 16×136: horizontal and vertical output-grid pixels, each scaled by
four when sampling input coordinates. Independent signed translations test the
direction, channel order and scale.

Forward/backward agreement, local texture, near-complete source support and a
photometric improvement test with a two-intensity-level mean error allowance
qualify each proposal. Repeated azimuth estimates are blended across the
40-degree overlap. Accepted cylindrical displacements become a periodic 512×64
spherical field, consumed by the same CPU/GPU compositor as the other
optimizers. Model initialization is reused within `StitchPlanner`; the current
source pair alone determines every inference input. Inference is a bounded,
synchronous stage; cancellation is checked before and after it.

This qualifies the implemented tensor and coordinate contracts. Real-camera
image quality, temporal stability, and equivalence to Studio remain separate
qualification tasks. See [AI model evidence](ai-stitching-model.md) for
provenance and native producer details.

## Ownership, determinism and performance

`StitchPlanner` prepares an immutable `PreparedStitchPlan` from the exact
current source pair. It never seeds a solve with the previous frame, uses a
delayed map, or changes analysis resolution according to the preview window.
Backend, projection size and global panorama orientation do not select another
solver. CPU and GPU consume the same correspondence field.

The owner retains the exact source-pair identity with the plan. Replaying a plan
checks calibration, source dimensions and sensor-readout poses; the application
also checks its retained frame identity and recipe. Save Frame reuses the
presented plan at the requested output size. A random seek or an intervening
render therefore cannot change that frame's geometry. Tests cover independent
recomputation, output resizing, stale calibration rejection and CPU/GPU parity.

`prepare_sources_controlled` checks cancellation before preparation, during
source analysis and within patch iterations. A cancelled plan is not published.
The media session shares one source-mask cache between its planner, RGB boundary
repair and selected renderer, including a GPU-to-CPU fallback. Standalone public
constructors keep independent ownership. This avoids a second full rasterization
and about 66 MB of duplicate masks for a pair of 2880-square sources; retained
frame plans do not own additional source masks. GPU buffers and RGB boundary
repair indices are also reused. Analysis uses borrowed RGB, YUV420, NV12 or
high-bit-depth planes and samples only the bounded belt, avoiding a
full-resolution RGB conversion on the direct GPU path. Flow workspaces are
bounded by the analysis dimensions; no recording-length history or search-volume
allocation is retained.

Dynamic and Optical Flow have different preparation costs. Benchmark cold
preparation separately from warm rendering, and include decoding, analysis,
uploads, readback and publication in end-to-end measurements. No source-rate
playback or cross-platform speed claim follows from unit tests. Per-frame solves
remain deterministic but can still exhibit visible temporal variation; moving
near-field scenes require separate qualification.

## Housing support before color reconstruction

Masking RGB samples after YUV conversion is insufficient. A supported luma pixel
can obtain chroma from an unsupported neighboring 4:2:0 texel. For centered
chroma, luma x=3 samples chroma coordinate 1.25. If support stops at x=4, the
old sampler admitted 25% of a chroma texel covering excluded columns 4–5. A
neutral BT.709 sample could consequently change from RGB (130,130,130) to
(130,124,197) when only that excluded U texel changed.

`StitchSource` reconstructs luma and chroma with separate support. A chroma
texel is valid only when its complete associated luma footprint is supported.
Valid bilinear taps are normalized; absent chroma support is neutral. The GPU
caches one support bit per chroma cell alongside its source masks. Integer GPU
storage preserves all bit patterns; only the original luma weights are read as
floats. This keeps two 3840-square source masks below the default 128 MiB
storage binding limit. Planar and NV12 paths use the same contract, including
centered/left chroma and odd dimensions.

CPU rendering keeps the existing SIMD color conversion for the image interior.
The shared projector uses a private borrowed source trait: full-frame RGB
rendering and color estimation compile to a direct RGB sampler, while overlap
analysis accepts decoded planes. This keeps format dispatch and large source
descriptor copies out of the CPU pixel loop without duplicating geometry. Both
color-estimation passes run independent columns in parallel, retaining ascending
row accumulation within each column so their statistics remain identical to the
serial traversal. `repair_rgb_support` reconstructs only supported boundary
pixels whose chroma footprint can touch exclusion. A separable seven-pixel
erosion prepares that boundary index list in linear time and caches it with the
source masks. Borrowed 16-bit planes retain bit depth, endianness and
significant-bit shift, so planar 10/12/16-bit formats and P010 receive the same
repair without quantizing their input before reconstruction.

Encoded chroma may already combine housing and scene detail. Masking cannot
unmix that sample or recover scenery hidden from both lenses. Conservative
support avoids admitting further excluded samples; it is not generative housing
removal. Poison-tap tests change excluded chroma while keeping supported samples
fixed, exercise actual GPU planar/NV12 rendering, and include a positive scene
color control. Separate real-source diagnostics must establish whether the
selected calibration and physical housing contour are correct.

## Qualification

Use the original recording and exact source frame pair, with a record of
effective optics, sensor crop, source dimensions, algorithm and software
version. Start with color restoration and stabilization disabled. Inspect each
unblended lens, source mask, overlap support and final image; compare matched
Studio output using the same physical setup and timestamp.

The supplied August 2026 X5 case records standard Invisible Dive Case underwater
and resolves lens 113 to 117. Historical Pro-to-117 mistakes in an earlier
fixture are not a diagnosis of this recording. Synthetic geometry, accurate MNN
tensors, asset hashes and CPU/GPU agreement each establish a narrower contract
than physical scene quality or Studio equivalence.
