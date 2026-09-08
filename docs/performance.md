# Performance

## Version 0.1.0 benchmarks (2026-09-09)

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
GPU resources are cached and reused.

GPU preparation validates source texture dimensions, storage-buffer sizes,
dispatch counts, and representable shader parameters. Allocation and binding
errors return typed GPU failures before caching resources, allowing callers to
apply their configured fallback. CPU panorama allocation is fallible as well;
arithmetic or address-space limits return an error. These checks do not replace
the caller's memory budget for concurrent exports.

The current GPU path blocks after every submission for readback. Video YUV is
copied from the mapped readback into an FFmpeg frame, and hardware decode/native
codec surface sharing are not implemented. These synchronization and transfer
costs can dominate smaller outputs, so adapter discovery or successful shader
tests do not establish an end-to-end speedup.

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
