# Performance

## Historical version 0.1.0 benchmarks (2026-09-09)

These measurements predate the corrected Pro lens ID, sensor-crop mapping and
prepared radial masks. They describe the earlier implementation and are not a
performance or quality baseline for the current housing pipeline.

The repaired macOS ARM64 wheel was measured on an Apple M4 Pro with 24 GiB RAM,
using release Rust 1.97.1, bundled FFmpeg 8.1.2, and software libx265 at
quality 90. Both backends export the supplied X5 recording from source second
635.3 at 1920×960 with Direction Lock and rolling-shutter correction off. Each
backend received a one-second warmup followed by three five-second exports, run
sequentially after the test/build jobs completed. Wall time includes opening,
preflight, decoding, stitching, encoding, and final publication.

| Renderer            | Measured frames per run | Median wall time | Median throughput |
| ------------------- | ----------------------: | ---------------: | ----------------: |
| CPU + libx265       |                     150 |         19.345 s |          7.75 fps |
| Metal GPU + libx265 |                     150 |         10.826 s |         13.86 fps |

GPU throughput is 1.79 times CPU throughput for this workload. All eight
warmup/measured outputs fully decode as finalized HEVC MP4s with the expected
dimensions and frame counts. [The measurements](performance-0.1.0.json) preserve
each run, including frame counts, durations, and output sizes.

This measures one host and a short scene; it does not establish cross-platform
performance or vendor quality parity. The historical runs below use different
intervals and settings and are not a matched regression baseline.

## Probe path

Inspection reads ISO-BMFF headers, the `moov` hierarchy, the fixed trailer
header/index, and the small metadata record. It does not scan or map the video
payload. The supplied 13.4 GB X5 recording is therefore probed in essentially
constant I/O relative to recording duration.

Proprietary record reads are explicit and capped. The caller can inspect record
descriptors before choosing to allocate gyro, exposure, thumbnail, or other
payloads.

## Media path

The selected-frame exporter demuxes once, lets each FFmpeg decoder use frame
threading, retains at most 64 unmatched frames per track, and stitches output
rows with Rayon. Only requested synchronized frames enter the stitcher and
encoder. Timestamp selections seek both tracks with one demuxer seek.

At 5.7K, RGB decode buffers and one panorama require hundreds of megabytes at
peak. Callers should bound concurrent jobs based on memory, not only CPU count.
Multiple recordings can be processed concurrently, while one recording should
retain a single demux order to preserve deterministic track synchronization.

The CPU implementation evaluates projection math per output pixel. X5
radiometric correction adds two deterministic overlap prepasses sampled every
eight panorama rows, plus a 25-tap low-frequency sample only where both lenses
contribute.

The opt-in `wgpu` renderer now moves radiometric estimation, projection,
sampling, mask/seam evaluation, two-band blending, and video RGB-to-YUV420
conversion to GPU compute. It uploads decoded YUV420P directly; unsupported
decoded formats first pass through CPU `swscale` to RGB. Frame-size-dependent
GPU resources are cached and reused. Reapplying the same shared color LUT or
leaving it disabled preserves that cache. Hosts should retain preview renderers
across view changes rather than reconstructing their mask, motion and color
state for each frame.

Source housing masks are prepared at decoded lens resolution even for a small
panorama preview. The fixed-point distance transform uses separate forward and
reverse row scans with fixed interior neighbors, avoiding repeated coordinate
checks without changing contour or feather values. Border handling remains
clipped, and scratch storage stays proportional to the original image size,
including one-pixel-wide inputs. Random rectangular-mask equivalence tests and
an independent shortest-path reference verify the optimization. This removes CPU
overhead from initial preparation; subsequent requests reuse the prepared mask.
When both lenses need a housing mask, the caller prepares one while a single
scoped helper thread prepares the other, after checking the combined
64-megapixel limit. Bare and single-mask inputs stay serial; a helper startup
failure falls back to serial work. The shared cache lock permits only one
preparation, and the helper wait does not steal tasks from a caller's Rayon pool
(which could deadlock on the same cache lock). The temporary raster, distance
and output fields can overlap at up to nine bytes per combined source pixel (576
MiB at the limit); final cached masks use four bytes per pixel. For two 3840²
masks this can add about 70 MiB of peak scratch compared with serial
preparation. Failed preparation leaves no cached partial pair and preserves the
first lens's error. Downscaled previews still incur full-resolution source-mask
and upload costs.

GPU preparation validates source texture dimensions, storage-buffer sizes,
dispatch counts, and representable shader parameters. Allocation and binding
errors return typed GPU failures before caching resources, allowing callers to
apply their configured fallback. CPU panorama allocation is fallible as well;
arithmetic or address-space limits return an error. These checks do not replace
the caller's memory budget for concurrent exports.

The current GPU path blocks after every submission for readback. Video YUV is
copied from the mapped readback into an FFmpeg frame. Export decoding is
software; random-access previews have a separate hardware-decoding policy.
Native codec surface sharing is not implemented. These synchronization and
transfer costs can dominate smaller outputs, so adapter discovery or successful
shader tests do not establish an end-to-end speedup.

## Interactive host measurements (2026-09-10)

FrameForge measured the SDK changes together with its retained preview sessions
and deferred import checks on real X5 recordings on macOS ARM64. Runs used the
same unoptimized Rust test profile, with build and test jobs stopped. These are
host integration measurements, not standalone SDK or shipping-release timings.

| Operation                  |   Before |   After |
| -------------------------- | -------: | ------: |
| Cold panorama, median      | 10.590 s | 4.070 s |
| Return to panorama, median | 10.478 s | 0.878 s |
| Cold panorama with AI      | 12.387 s | 5.875 s |
| Return to panorama with AI | 12.488 s | 0.932 s |

The fixed-stencil optimization alone brought cold panorama preparation to 5.484
s; the scoped lens helper reduced that to 4.070 s. Its bounded temporary scratch
increase was accepted for this workload. The host's initial import returned in
15.2 ms for a single recording and 12.6 ms for a split recording, compared with
approximately 3.3 s and 4.9 s when full checks were eager.

Deferred source capability assessment still took 3.000 s initially and about
1.55 s on later checks. Only metadata-based processing selections were below 1
ms. Deferring telemetry and runtime preparation keeps source selection quick; it
does not eliminate that work or guarantee immediate first rendering. The results
cover these sources and host, not all cameras, resolutions or platforms.

## Stabilization preparation and readout cost

Enabled stabilization expands bounded BMFF timing tables and integrates the full
recording's IMU once per export attempt, including pre-roll. This makes a sought
frame's heading independent of the chosen export range. Memory and preparation
time grow with telemetry duration; the pose cap is five million and queries use
binary search. Short exports still pay this preparation cost.

Sensor correction uploads at most 129 unit quaternions per lens (4,128 bytes
combined as f32). GPU storage is reused between frames. Each lens projection
uses at most eight source-coordinate refinement iterations, with early exit at
0.05 source pixel movement. CPU/GPU radiometry uses the same sensor correction.
The disabled path keeps a single projection; there is no CPU panorama detour for
GPU readout correction.

The historical measurements below predate the exposure-clock, gravity-fusion,
and sensor-readout implementation and do not measure its performance. New costs
must use matched settings and builds rather than extrapolating those numbers.

### Current motion-path smoke measurement

On 2026-09-09, the same release build rendered source interval
`[657.85, 672.85)` at 1920×960, direction lock, quality 85, explicit Metal GPU,
and VideoToolbox HEVC on Apple M4 Pro. Jobs ran sequentially, with no competing
compilation or export. Both finalized 450-frame, 15.015-second BT.709-limited
YUV420 files at approximately 35.94 Mb/s; the encoded duration includes the last
selected frame's duration.

| Sensor correction         | Export elapsed | Throughput |
| ------------------------- | -------------: | ---------: |
| Required readout          |       12.346 s |  36.45 fps |
| Global stabilization only |       13.121 s |  34.30 fps |

These are functional single-run measurements, not isolated readout overhead or a
speedup claim. Their small timing reversal illustrates why repeated warm-run
stage measurements are needed before attributing costs to projection alone. Both
include full-file IMU preparation and exposure mapping.

## Initial matched X5 measurement

On 2026-09-08, release-build comparisons used the same 15-second midpoint
interval, 1920×960 output, X5 V6 calibration, invisible-dive-case underwater
profile, direction lock, and quality 85 on the same Apple Metal host:

| Path                         | Frames | Wall time | Throughput |
| ---------------------------- | -----: | --------: | ---------: |
| CPU iteration 6 + libx265    |    449 |  43.240 s |  10.38 fps |
| Portable wgpu + libx265      |    449 |  32.556 s |  13.79 fps |
| Portable wgpu + VideoToolbox |    449 |  11.132 s |  40.33 fps |

With the codec held constant, the portable-GPU result is 1.33× the end-to-end
CPU throughput and reduces wall time by 24.7%. Adding the platform hardware
encoder reaches 3.89× CPU throughput and reduces wall time by 74.3%. Both
generated MP4s are 14.982-second HEVC/YUV420P BT.709-limited files. The libx265
GPU midpoint measured 0.972 SSIM / 37.48 dB PSNR against the accepted CPU
midpoint after both paths' HEVC round trips. The corrected ~36 Mb/s VideoToolbox
output measured 0.960 SSIM / 36.33 dB PSNR and is visually geometrically
consistent with both.

This is an implementation measurement, not the formal release benchmark: it is
one run rather than three warm-run medians and does not isolate every stage. The
libx265 report accounted for 31.11 seconds of the 32.56-second GPU export, which
explains the large gain from VideoToolbox. Formal reporting must still use
matched thermal state, three-run medians, stage timings, and Metal/D3D12/Vulkan
results. Automatic GPU selection remains experimental until those cross-platform
gates pass.

### Full-resolution 5760x2880 rerun

The same source interval and settings were then rerun at the X5's full
equirectangular resolution. The three release jobs ran sequentially:

| Path                         | Frames | Wall time | Throughput | Output bitrate |
| ---------------------------- | -----: | --------: | ---------: | -------------: |
| CPU + libx265                |    449 | 351.175 s |   1.28 fps |    138.29 Mb/s |
| Portable wgpu + libx265      |    449 | 176.488 s |   2.54 fps |    138.40 Mb/s |
| Portable wgpu + VideoToolbox |    449 |  16.280 s |  27.58 fps |    140.48 Mb/s |

Holding libx265 constant, GPU stitching nearly doubles end-to-end throughput
(1.99x) and cuts wall time by 49.7%. The fully accelerated path is 21.57x the
CPU/libx265 throughput, cuts wall time by 95.4%, and is 10.84x faster than the
GPU/libx265 path. It is just below the 29.97-fps source rate without native
zero-copy or asynchronous frame slots.

All outputs are finalized 14.982-second HEVC/YUV420P MP4s. Against the CPU
midpoint after independent HEVC round trips, GPU/libx265 measured 0.971 SSIM /
38.51 dB PSNR and GPU/VideoToolbox measured 0.964 SSIM / 37.83 dB PSNR. The
hardware output uses the resolution-aware high-end photogrammetry curve; its
140.48 Mb/s bitrate and 263.1 MB file size closely match the 138.40 Mb/s, 259.2
MB libx265 result. These are single-run measurements and retain the formal
qualification limitations above.

## Exact native random-access previews

With `media`, `paired::PairedPreviewReader` is the random-access counterpart to
`PairedReader`'s continuous single-demux export path. It keeps one persistent
worker, demuxer and decoder per lens; seeks and decode proceed concurrently.
Capacity-one channels and one returned native frame per lens bound application
queues. Codec reference surfaces remain subject to the decoded-pixel bound. No
proxy pictures or intermediate video files are generated.

`PreviewAcceleration::Auto` selects supported FFmpeg hardware decoding and falls
back to software when device/format setup or codec decoding is unavailable;
`Software` provides a reference mode. Only requested hardware pictures transfer
to CPU memory. Consumers own both native frame handles and should retain them
for exact current-pair exports instead of re-seeking from rounded display time.

Random seeks preserve an extra indexed GOP for reordered streams. Streams that
declare no B pictures use the preceding indexed keyframe directly. Non-reference
pictures with packet PTS before the target may be discarded during preroll;
reference pictures and pictures at/after the target are retained. Final camera
pairing compares rational PTS and presentation origins exactly. Generated tests
cover reordered/non-reordered media, random/backwards/repeated/EOF seeks,
chapter transitions, reversed lens order, missing partners and cancellation.

In FrameForge on an Apple M4 Pro (24 GiB, shared FFmpeg 9.0), the original X5
HDR two-track 2880×2880 HEVC 60000/1001 recordings were measured across three
sessions and 16 requests/session. End-to-end native preview including two 640px
JPEGs improved non-adjacent seek p50/p95 from 2,641/3,604 ms to 303/559 ms
(8.7×/6.4×). Nearby forward steps stayed approximately 33 ms. Application-side
persistent parallel JPEG encoders and cached source inspection are included in
those numbers; they are not a standalone library throughput benchmark.

First-process hardware initialization cost 1,422 ms versus a 167 ms software
baseline. Subsequent fresh sessions opened in 71–79 ms. Results establish this
workload on one host, not universal codec/platform speedups. Independent
demuxers trade duplicated compressed reads for lens concurrency; OS caching
normally shares the reads, but slow uncached storage can affect that tradeoff.

Set `INSTA360_RS_PREVIEW_SAMPLE` to a real X5 original and run
`cargo test --locked --features media --test paired_preview` to compare `Auto`
preview decoding with serial-software native pixels. The test compares both
lenses' Y/U/V samples, dimensions, range, color space and rational pair
identities at five seeks, deinterleaving NV12 and excluding unspecified row
padding. `Auto` can fall back to software, so a passing test establishes parity
for the selected path; hardware qualification additionally requires independent
evidence that a hardware decoder was used.
