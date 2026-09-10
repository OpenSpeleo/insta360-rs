# Recording sequence video export

`Exporter::from_sequence` accepts a validated `RecordingSequence` and produces
one finalized HEVC MP4. `Exporter::new` retains its single-input contract and
uses the same video engine. Exports process available validated original
chapters; raw group submedia counts do not establish whole-recording coverage.
Discovery rules are described in [recording sequences](recording-sequences.md).

Each chapter must have a validated dual-track, two-file or metadata-proven
packed panorama layout, compatible source properties, established camera A/B
ordering, and usable per-recording calibration. Unsupported camera, layout,
accessory, color, or motion combinations return a capability or validation
error. Stitched still export currently accepts one chapter; direct fisheye frame
access uses `PairedReader` across the selected sequence without intermediate
videos.

## Decoded source layouts

Image and video export use the same `PairedReader` adapters:

| Layout                           | Required source identity                                                                                                                                                                                                                                                                                                      |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| One file, two video tracks       | Recorded A/B track order; any stream/projection declaration must agree with separate full fisheye tracks.                                                                                                                                                                                                                     |
| Two files, one video track each  | Matching `_00_` and `_10_` filenames and recording identity; compatible timing, dimensions, geometry and color metadata. Caller path order does not change A/B ownership.                                                                                                                                                     |
| One file, one packed video track | `VID_` filename; explicit `DoubleFisheyePanorama`, `SingleStreamFile` and zero rotation; a 2:1 encoded canvas with width divisible by four and even height; current two-lens calibration on a 2:1 canvas with A's center in the left half and B's center in the right half. Split-file/multi-track declarations are rejected. |

Packed pictures are decoded once, then copied into two owning native-format
frames with correct plane strides. They retain sample depth and timestamps.
These copies prevent a cropped frame view from exposing memory beyond its last
row. Packed preview uses one software decoder per request; separate-lens preview
retains its two worker sessions. Neither dimensions nor filenames alone enable
packed decoding. Half-fisheye, single-lens, rectilinear and already stitched
sources are unsupported. Explicit nonzero or unknown recorded rotations fail
until a corresponding lens transform is established.

Rendering requires an established camera/lens profile and the recording's own
calibration. `Stabilization::Off` permits the broader decoded camera families;
motion processing still requires a qualified motion profile. No X5 IMU
convention is inferred for another camera. Sensor-window normalization and
housing masks remain separate calibration operations.

Native paired access retains 10-bit SDR and HDR samples. Stitched images and
video currently produce 8-bit output: 10-bit SDR is converted accordingly, while
Dolby, PQ and HLG fail because a verified HDR tone mapper is unavailable.
`ColorConversion::Preserve` does not bypass this restriction. Encoded extraction
continues to preserve the original streams without these rendering constraints.

## Preflight and options

Call `exporter.preflight_video(&options)` off the UI thread before presenting a
confirmed export. It resolves calibration, color and motion for every chapter,
validates declared camera A/B track ordering, the projection and selected time
interval, checks the selected encoder policy, and checks copied audio
compatibility. `VideoPreflight` contains the effective projection, duration,
frame rate, chapter count, audio track count, eligible encoder names, and
warnings. It does not create output files. Encoder candidates describe
availability; opening a hardware encoder can still fail at export time.

All existing `StitchConfig` and `VideoExportOptions` fields apply. Output is
8-bit YUV420 HEVC in MP4 with an even 2:1 equirectangular projection. Quality is
1–100. Start/duration select a half-open interval on the selected recording
timeline, and the first included video frame becomes output time zero.

`ExportEvent::BackendSelected` reports the renderer actually opened, and
`EncoderSelected(EncoderReport)` reports the successfully opened encoder name,
hardware flag, and copied audio count. Stitch GPU selection and encoder
acceleration remain independent. Export decoding is software; GPU frames still
use synchronous readback. Progress is emitted at most every 100 ms while
stitching, with the global media time and an elapsed-work ETA, plus phase
events.

## One continuous processing session

The sequence engine holds one strict `PairedReader`, one renderer, and one HEVC
writer per attempt. It validates exact rational A/B timestamps before processing
a frame; it never selects a neighboring frame to repair a missing partner.
Chapter-local presentation times are normalized by the shared video stream
origin and added to the chapter's recording start. It does not stitch separate
chapter files and concatenate them afterward.

At each chapter boundary, calibration and color settings are refreshed. Motion
continues on the recorded camera clock from the preceding pose, unwrapped
heading, and residual bias. Repeated telemetry must match the preceding sensor
samples exactly, and each lens's frame capture clock must advance beyond the
preceding chapter. Matching repeated gyro data alone cannot qualify a reset
capture clock. A small non-overlapping tail is bridged using the last sensor
measurement, subject to the normal maximum telemetry gap. Clock resets,
conflicting overlaps, or missing coverage fail explicitly; no arbitrary clock
offset is inferred. Gravity and Direction Lock orientation are initialized only
for the first chapter. A trimmed export prepares earlier chapter motion to keep
that same world frame.

Optional underwater restoration runs after stitching and I-Log conversion. Each
video attempt owns its restoration session and resets temporal history at
chapter boundaries; each selected still starts with fresh history. Enabling
restoration routes GPU video through RGB output before encoding. Resolved
optical settings remain available in `ExportResult.optics` independently of
color settings.

Only the current and preceding chapter's bounded telemetry are retained while
preparing the next chapter. Frame queues, audio buffers, GPU resources and the
writer remain bounded independently of recording length. Automatic GPU failure
restarts the complete attempt on CPU; explicit GPU requests remain strict.

## Original audio

`AudioPolicy::Copy` remuxes AAC or ALAC compressed packets into the same MP4
writer. Payload bytes, track metadata and dispositions are copied; no audio
decoder or encoder is opened. All chapters must agree on track count, codec,
sample format/rate, channel layout, profile and codec configuration. Other audio
formats return a capability error with the option to disable original audio. A
recording without audio succeeds with a warning and a silent video.

Audio uses the same video origin and chapter offsets, preserving its relative
timing instead of moving its first packet independently to zero. The MP4 movie
clock uses microsecond resolution so edit lists retain sub-millisecond audio
offsets, including after clipping, instead of rounding them to milliseconds.
Audio packets retain their sample-based time base.

For two-file sources, audio from both inputs is preserved. The public
`PairedReader::take_audio_packets` stream index is a chapter-wide namespace: the
original stream index plus the total number of streams in preceding input files.
Packet PTS, DTS, duration, time base and payload are unchanged. Native A/B
pairing compares rational timestamps and shared presentation origins exactly;
rounded recording microseconds are for display and selection. Saving a preview
with `open_at_pair` uses its exact native identity.

Only complete compressed packets inside the selected recording/chapter interval
are copied. A cut can omit a partial compressed packet on each side of a chapter
boundary, so the combined gap can span two packet durations. Packet
timestamps/durations must be present and valid, and output DTS must increase.
The reader drains the final audio tail without further video decoding when a
clip ends. Missing timing, corrupt packets and overlapping output timestamps
fail rather than being guessed.

## Publication and verification

Cancellation and failure remove only attempt-owned temporary outputs. Successful
publication flushes the encoder, writes the MP4 trailer, checks I/O errors, and
atomically publishes without overwriting an existing destination.

`tests/sequence_export.rs` generates unsplit and split dual-track recordings and
compares every decoded output pixel, checks global clipping and one encoder
selection, verifies original audio packet identity and A/V offsets, and tests
preflight, missing A/B ordering, silent audio, incompatible audio layouts,
cancellation and no-clobber behavior. Mixed AAC/ALAC tests verify multiple
tracks, metadata, dispositions and a clip that begins between video frames.
Fusion tests compare continued pose/heading with an uninterrupted track and
reject conflicting or reset clocks. These synthetic checks establish the stated
contracts; real X5 split recordings and each physical GPU/encoder platform still
need release qualification.

`tests/decoded_layouts.rs` adds generated fixtures across the registered bare
camera families, all three layouts, differing rational clocks and GOPs, nonzero
origins, repeated seeks/cancellation, chapter boundaries, both files' audio
payloads, and a 4,096-frame legacy stream. It checks SDR/HDR behavior and
complete Legacy/AI image/video restoration, including GPU restoration when an
adapter is required. These fixtures validate adapters and contracts; they do not
qualify every physical camera, housing or hardware decoder.
