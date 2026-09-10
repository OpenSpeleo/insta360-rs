# Recording chapters and exact lens pairs

`RecordingSequence` represents one camera recording as ordered temporal
`RecordingChapter` values. Each chapter contains an existing `InputSet`: one
multi-track file or the simultaneous `_00_`/`_10_` lens pair. Separating these
levels prevents a continuation file from being mistaken for the other camera.

`RecordingSequence::discover(path)` examines siblings only when metadata field
88 explicitly declares temporal splitting (`2`). It matches the field-26 group
identity and capture subtype, then validates camera serial/model, source track
geometry/codec/rate, recorded lens order, crop, and color compatibility.
Metadata identities on files explicitly marked not split do not trigger
grouping. A proxy never substitutes for a missing original lens.

The field-26 protobuf schema and field-88 enum were checked against supplied SDK
metadata. Field-26 `type` is the capture subtype (`VIDEO_HDR=6`,
`VIDEO_PURE=20`); it is independent of the split flag. The iOS SDK's
`INSExtraMetadata.h` identifies that field as `INSSubMediaType`. The Android
SDK's split path in `UrlAsset.fillFiles()` orders inputs by raw member index
without using a declared total to establish completeness.

**Group members are not original video chapters.** A real X5 HDR recording from
firmware `v1.11.10_build1` has original indices `0, 2` and matching LRV preview
indices `1, 3`, all sharing a group identity. Its group total is `0`. Thus
neither an index gap nor a filename counter proves an original chapter is
absent. A nonzero group total is submedia metadata without a verified mapping to
the number of original video chapters. Do not assume an index origin,
consecutive original indices, or a fixed stride such as dividing every index by
two. Absolute indices may be any `u32`; allocation and the 4096-chapter limit
apply to actual inputs, never metadata positions.

Discovery validates and orders available originals without claiming complete
capture coverage. Split sequences have `complete = false` and no warning solely
because that coverage is unknown. Export operations process those validated
chapters; `require_complete()` remains a stricter API for callers that require
verified whole-recording coverage and reports that coverage is unknown. Nonsplit
recordings and explicit `single(inputs)` selections have `complete = true` for
their selected scope. Missing paths and simultaneous lens partners still fail
normal input validation. Duplicate member indices, conflicting nonzero group
totals, incompatible properties, and conflicting or malformed declarations
remain errors. Both recognized fields remain in the raw metadata collection,
including unknown nested fields.

Malformed or conflicting declarations on the selected input fail `discover()`
and `new()` before they can be mistaken for a nonsplit recording. Applications
can deliberately choose `single(inputs)` to open only that selected chapter.
Discovery accepts bare filenames relative to the current working directory.

Applications can assess positive continuity evidence without interpreting index
gaps. `has_capture_gap()` reports a substantial uncovered interval between
originals on the recorded raw-gyro microsecond clock. It excludes unknown clocks
and time-lapse and allows one second for frame rounding and boundary priming.
`lacks_preview_footage(&inspection)` checks whether an associated preview's
capture interval is covered by the available originals, using the same
allowance. It first verifies the camera serial/model, group identity/subtype,
split marker, and known clocks. A preview beginning at the last original's end
still represents missing footage when its interval extends beyond the available
recording.

Neither method establishes whole-recording completeness; absent final chapters
without surviving evidence, unqualified clocks, and very short discontinuities
can remain unknown. Optional previews are never required and their metadata
indices are not compared with originals. Discovery continues to return validated
available inputs. The application chooses its fallback policy and retains the
user's selected original before chronological sorting; `single(inputs)` opens
that original when a complete sequence is unavailable.

Chapter durations come from container track timing and are accumulated as
checked `Duration` values. `chapter_at()` uses half-open intervals, so a time
exactly on a join belongs to the following chapter. Camera/exposure clock
continuity is a separate stabilization preflight responsibility; a camera group
identity alone does not prove that motion can be extrapolated across a telemetry
gap.

With the `media` feature, `PairedReader` opens the original file directly and
uses one demuxer with two decoders. `next_pair()` returns FFmpeg-owned frame
buffers without RGB conversion, along with chapter identity, original PTS/time
bases, and recording-relative microseconds for presentation. Pair identity
compares rational PTS using FFmpeg's exact timestamp comparator. Equal rounded
microseconds are insufficient. Missing, duplicated, nonmonotonic, corrupt, or
unmatched frames fail; there is no ordinal or nearest-frame fallback. Recorded
lens order is required. Current decoded support is one file with exactly two
video tracks; legacy pairs and packed fisheye frames remain available to
lossless unpack.

Seeking uses FFmpeg's microsecond seek convention and retains a preceding
indexed GOP for each lens: MP4 decode timestamps can otherwise skip dependencies
of a requested B frame. If an index cannot establish that preroll, reading
starts at the chapter beginning. Preroll frames are decoded and excluded from
output using exact stream-relative timing, including fractional nonzero source
origins; rounding is confined to seek and display timestamps. Delayed frames are
drained before advancing chapters. Video queues are bounded by frame count and
retained bytes, and cancellation is checked during demux and decode. No
intermediate video is created. Optional audio forwarding retains original
packets; consumers drain audio batches after each returned pair and after EOF.
`finish_audio()` reads the short packet tail at a clipped video boundary without
decoding further video. `FramePair::identity()` retains both native timestamps
and time bases, and `open_at_pair()` can recover that exact pair without
rounding a displayed time. Applications may instead retain the displayed
original frame handles for a current-pair export, avoiding another seek or
decode.

Interactive applications can instead use `paired::PairedPreviewReader` with
`PreviewAcceleration::Auto` or `Software`. It retains an independent demuxer and
decoder worker for each lens and returns the same exact native pair contract.
Random seeks reuse those resources, use hardware decoding when supported, and
avoid unnecessary non-reference preroll. Continuous exports retain the serial
`PairedReader` so they do not read the container twice. See
[performance](performance.md) for measured behavior, bounded ownership and
hardware/software qualification.

A preview request after a chapter's last presented frame advances to the first
available pair in following chapters. Both lenses must reach EOF together;
unmatched frames and decode errors remain failures.

Tests generate independent ISO-BMFF/ExtraInfo fixtures for metadata discovery
and small MPEG-4 dual-track recordings with delayed frames for decode, chapter
joins, seek, and cancellation. Native rational tests include distinct timestamps
that round to the same microsecond. Chapter fixtures cover the observed HDR
original/preview index pattern, arbitrary origins and filenames, unknown and
nonzero submedia totals, invalid associations, and extreme indices. Continuous
export tests compare decoded output with the unsplit original. These establish
specific algorithmic behavior. The real X5 HDR files establish the observed
grouping semantics; other camera/firmware combinations and exact Studio boundary
behavior still require qualification.
