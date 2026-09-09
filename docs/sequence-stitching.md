# Recording sequence video export

`Exporter::from_sequence` accepts a validated `RecordingSequence` and produces
one finalized HEVC MP4. `Exporter::new` retains its single-input contract and
uses the same video engine. Sequence discovery and completeness rules are
described in [recording sequences](recording-sequences.md).

Each chapter must have the supported X5 single-file, two-video-track layout,
compatible source properties, established camera A/B ordering, and usable
per-recording calibration. Unsupported camera, layout, accessory, color, or
motion combinations return a capability or validation error. Stitched still
export currently accepts one chapter; direct fisheye frame access uses
`PairedReader` across the complete sequence without intermediate videos.

## Preflight and options

Call `exporter.preflight_video(&options)` off the UI thread before presenting a
confirmed export. It resolves calibration, color and motion for every chapter,
validates the projection and selected time interval, checks the selected encoder
policy, and checks copied audio compatibility. `VideoPreflight` contains the
effective projection, duration, frame rate, chapter count, audio track count,
eligible encoder names, and warnings. It does not create output files. Encoder
candidates describe availability; opening a hardware encoder can still fail at
export time.

All existing `StitchConfig` and `VideoExportOptions` fields apply. Output is
8-bit YUV420 HEVC in MP4 with an even 2:1 equirectangular projection. Quality is
1–100. Start/duration select a half-open interval on the complete recording
timeline, and the first included video frame becomes output time zero.

`ExportEvent::BackendSelected` reports the renderer actually opened, and
`EncoderSelected(EncoderReport)` reports the successfully opened encoder name,
hardware flag, and copied audio count. Stitch GPU selection and encoder
acceleration remain independent. Video decoding is software; GPU frames still
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
samples exactly. A small non-overlapping tail is bridged using the last sensor
measurement, subject to the normal maximum telemetry gap. Clock resets,
conflicting overlaps, or missing coverage fail explicitly; no arbitrary clock
offset is inferred. Gravity and Direction Lock orientation are initialized only
for the first chapter. A trimmed export prepares earlier chapter motion to keep
that same world frame.

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
timing instead of moving its first packet independently to zero. Only complete
compressed packets inside the selected recording/chapter interval are copied.
Thus cuts can leave a gap of up to one compressed packet at a boundary. Packet
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
preflight, silent audio, cancellation and no-clobber behavior. Fusion tests
compare continued pose/heading with an uninterrupted track and reject
conflicting or reset clocks. These synthetic checks establish the stated
contracts; real X5 split recordings and each physical GPU/encoder platform still
need release qualification.
