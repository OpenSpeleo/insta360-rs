# Stream access and complete extraction

The `media` feature provides two ways to use the original recording: read its
streams directly, or extract its components into a folder. Neither operation
requires stitching, calibration, stabilization, a supported camera registry
entry, or an encoder. Encoded video and audio are copied without re-encoding.

## Direct stream access

`MediaSource` describes the streams in an `InputSet`. A `MediaStream` identifies
one stream in one original file. It opens an independent packet or frame reader,
so playback and background frame extraction can seek without sharing a cursor.
No intermediate MP4, packet dump, or image file is created.

```rust,no_run
use std::time::Duration;
use insta360_rs::{InputSet, MediaSource};

fn main() -> insta360_rs::Result<()> {
    let source = MediaSource::open(InputSet::discover("recording.insv")?)?;
    let video = source.video_streams().next().ok_or_else(|| {
        insta360_rs::Error::InvalidMedia("recording has no video stream".into())
    })?;

    let mut frames = video.open_video()?;
    frames.seek(Duration::from_secs(10))?;
    if let Some(frame) = frames.read_frame()? {
        // Tightly packed RGB24 bytes for a native preview or image encoder.
        assert_eq!(frame.data.len(), frame.width as usize * frame.height as usize * 3);
    }

    let mut packets = video.open_packets()?;
    while let Some(packet) = packets.read_packet()? {
        // Original encoded payload, codec time base, PTS/DTS, and side data.
        // Feed a compatible decoder or muxer without writing an intermediate file.
        let _ = packet;
    }
    Ok(())
}
```

`StreamInfo` reports input and per-file stream indices, kind, codec, codec
configuration, time base, dimensions, and declared timing. A stream index counts
all demuxed streams, including audio/data; it is not a lens number. Video
streams remain in file/track order. Apply recorded lens order when combining
them into a panorama.

Packet reads retain original signed timestamps and codec configuration. Packet
seeking can return an earlier keyframe, allowing the consumer's decoder to
reconstruct the requested position. Decoded-frame seeking flushes the decoder
and discards preroll before returning a frame at or after the requested time.
Frame times are relative to the selected stream's start.

Frame decoding is explicit and returns an unstitched view of the selected track.
RGB24 is an 8-bit preview/image representation; encoded packet access preserves
the original bit depth. Preview conversion supports the common BT.601, BT.709,
FCC, SMPTE 240M, and BT.2020 non-constant-luminance matrices and uses BT.601
when the matrix is unspecified. Other declared matrices return an unsupported
error; encoded packet access remains available. A packed-fisheye track remains
packed, while a dual-track recording exposes each lens separately. This API
supplies data for native playback; it is not an HTTP MP4 endpoint or a browser
`MediaStream` object.

## Folder extraction

Rust:

```rust,no_run
use insta360_rs::{extract, InputSet};

fn main() -> insta360_rs::Result<()> {
    let report = extract(&InputSet::discover("recording.insv")?, "extracted")?;
    println!("{}", report.manifest_path.display());
    Ok(())
}
```

CLI:

```sh
insta360-rs extract recording.insv extracted
insta360-rs extract VID_20260101_120000_00_001.insv VID_20260101_120000_10_001.insv extracted
insta360-rs extract recording.insv extracted --json
```

Python:

```python
import insta360_rs

report = insta360_rs.extract("recording.insv", "extracted")
print(report.manifest_path)
```

A single CLI/Python input discovers its conventional `_00_`/`_10_` sibling. Rust
callers use `InputSet::discover` for discovery or `InputSet::new` for an
explicit set. Each file has its own output directory. Optional proxies and
continuation segments are separate inputs to `extract`; that API does not
recursively search unrelated captures. Use a verified `RecordingSequence` and
`extract_sequence` for camera-split recordings.

### Controlled and sequence extraction

`extract_controlled(inputs, target, cancelled, callback)` retains the ordinary
output structure while adding cooperative cancellation through an `AtomicBool`
and throttled `ExtractionProgress` callbacks. Container copying checks between
64 KiB buffers and packet archiving checks between packets. Progress counts
original container and encoded packet bytes actually copied, with an estimated
total because preserved metadata can appear in several artifacts. Phase changes
and successful publication are always reported; intermediate events are limited
to one every 100 ms. `completed` becomes true only after publication.

`extract_sequence(sequence, target, cancelled, callback)` processes validated
available chapters without claiming complete recording coverage. Split sequences
retain `complete = false` in the manifest. Compatible video/audio streams get
one continuous playable file at the root. Declared lens ordering yields
`camera_A.mp4` and `camera_B.mp4`; unknown lens ordering uses neutral
`video_001` names. Audio copies use `audio_001.m4a` or a compatible Matroska
extension.

All original components are consolidated into five categories under `metadata/`.
There are no published `parts/` or per-input directories:

```text
extracted/
  manifest.json
  camera_A.mp4
  camera_B.mp4
  audio_001.m4a                   when present and compatible
  metadata/
    calibration/
      payload.bin
      artifacts.jsonl
    container/
      payload.bin
      artifacts.jsonl
    extra-info/
      payload.bin
      artifacts.jsonl
      records.jsonl
    metadata/
      payload.bin
      artifacts.jsonl
      recording.json
      motion.jsonl
      exposure.jsonl
    streams/
      payload.bin
      artifacts.jsonl
```

Every category exists, even when it has no source artifacts. Each `payload.bin`
contains the original artifact bytes from every source input. Each
`artifacts.jsonl` entry records `original_path`, `chapter_index`, `input_index`,
`timeline_start_ns`, `duration_ns`, and `slice` with `path`, `offset` and
`size`. Container boxes, proprietary tails and calibration profiles may differ
between chapters; a shared payload is an indexed bundle, not a new standalone
container or calibration file. No original artifact is overwritten by a later
chapter.

The sequence manifest uses schema version 3. Its input descriptions retain the
source identity, timeline start, container and media properties, but artifact
references are objects with `path`, `offset` and `size`. Paths are relative to
the export root; offsets and sizes identify bytes within that category's shared
payload. The manifest also describes the category indexes and continuous stream
mapping. Consumers should use these references instead of inferring filenames.

Packet indexes remain byte-for-byte original artifacts. Their packet and
side-data offsets are local to the corresponding packet-payload or side-data
artifact slice, not absolute offsets into the shared bundle. Likewise, original
path strings inside archived JSON resolve by chapter/input and original relative
name through the category indexes. Manifest and recording metadata references
already use explicit slices; they never depend on a removed source directory.

### Recording-wide metadata

The directly readable recording view is under `metadata/metadata/`:

- `recording.json` contains recording duration/completeness, continuous stream
  mappings, common settings, and time-ranged settings for every source input.
  Apply interval settings over `common` to recover each input's values. A
  setting that changes between parts is not promoted to a common value.
- `motion.jsonl` and `exposure.jsonl` contain supported samples from the entire
  recording. Rows retain chapter/input/record/sample provenance, original camera
  timestamps, and signed `timeline_timestamp_ns` values on the continuous export
  timeline. Nanosecond timestamps are decimal strings to preserve precision.
- `metadata/extra-info/records.jsonl`, relative to the export root, indexes the
  proprietary records, including repeated and unknown IDs. Each record's
  `raw_path` is a slice reference into that category's `payload.bin`; no
  separate duplicate record payload is written.

The recording schema is version 2, independent of the sequence manifest. This is
readable recording-wide metadata, not a rewritten proprietary INSV trailer or an
embedded metadata track in the camera MP4s. Negative preroll samples and overlap
at chapter boundaries remain explicit rather than being discarded or
deduplicated. Rows are in source traversal order; consumers must use timestamps
and provenance rather than assume strict global timestamp ordering.

Telemetry alignment requires a supported declared clock mapping. Unknown or
unsupported mappings retain original records and available camera-clock samples,
with null timeline timestamps and warnings rather than fabricated video
alignment. This includes exposure-file and timelapse mappings until those
transformations are qualified for this export. Metadata interpretation does not
require camera calibration, supported stitching profiles, video decoding or
re-encoding.

All raw packet PTS/DTS values remain unchanged. Continuous files use one
video-based time origin shared with audio per input, then add the chapter's
timeline start using rational timestamp rescaling. Repeated audio priming before
subsequent chapters and audio packets beginning beyond the chapter's video
duration are omitted from playable copies and counted in the manifest; complete
packets remain in the shared archive. No packet is decoded or re-encoded.
Boundary trimming is packet-granular, so the last retained audio packet may
extend beyond the video boundary by less than one packet duration.

Codec-configuration changes, missing timestamps/streams, and non-increasing
decode timestamps make that continuous copy unavailable. Extraction records a
warning and preserves all original artifacts in the category bundles. A
compatible copy never silently changes codecs to force a join. Camera-split
discovery and exact frame pairing have their own validation contracts;
convenience remuxing is not evidence of physical camera synchronization.

Sequence extraction demuxes each input once into temporary original artifacts
and continuous copies, then appends the artifacts to the category bundles with
bounded reads and buffered writes. Only one input's temporary artifacts exist at
a time; its staging-only directory is removed before the next input. Muxer
fragments and telemetry sample processing are bounded, and no decoded frames are
allocated. Bundling adds a sequential read/write pass over the original
artifacts. Cancellation checks apply throughout and remove the entire job-owned
staging directory, including incomplete bundles and continuous files, while
leaving sources and any pre-existing empty destination intact.

Generated fixtures recover original artifact bytes from indexed slices, verify
all published references and the five-category layout, and cover independent
camera epochs, changing settings, opaque records, unsupported mappings,
continuous-copy fallback and cancellation. Tests must not merely assert the
absence of `parts/`: every original artifact must remain recoverable.

The destination must be absent or an empty directory. Symlink destinations and
nonempty directories are rejected. Extraction runs in an owned sibling staging
directory; errors remove that directory, and successful completion publishes the
complete result. `ExtractionReport` contains absolute output paths,
input/stream/ record counts, generated files, and warnings.

## Single-input output structure

`extract` and `extract_controlled` retain their existing per-input layout:

```text
extracted/
  manifest.json
  input-00/
    container/                    original box bytes; mdat headers only
    extra-info/
      tail.bin                    complete original tail, including reserved bytes
      directory.bin               V3 directory when present
      ...                         every record payload, including unknown records
    metadata/                     readable metadata when its encoding is supported
    calibration/                  extracted calibration material when present
    streams/
      000/
        metadata.json             stream properties, tags, paths, and codec parameters
        packets.bin               concatenated original encoded packet payloads
        packets.jsonl             one record per packet, including boundaries and timing
        side_data.bin             original stream/packet side-data bytes
        extradata.bin             original codec configuration
        media.mp4                 additional playable video copy when supported
      001/...
      002/.../media.m4a            example AAC audio copy
  input-01/...                    companion file, if supplied
```

The manifest's `schema_version` is `1`. Each `inputs` entry has an absolute
`source`, original byte `size`, an input `directory` relative to the target, and
`container` and `media` descriptions. Artifact paths inside a component
description are relative to that input directory. Use manifest paths rather than
inferring record filenames from their IDs; repeated IDs have distinct files. The
report's `files` list includes every generated artifact.

Container descriptions identify original byte offsets, sizes, preserved boxes,
tail framing, and record encodings. V2 JSON and gyro envelopes, V3 indexed and
sequential tails, and empty V3 directories are supported by the extractor.
Metadata whose interpretation is unavailable remains available as original
bytes. Invalid tail structures produce explicit warnings and bounded raw
preservation where possible; they are never treated as valid decoded records.

Media descriptions include every video, audio, data, subtitle, attachment, and
unknown stream exposed by the demuxer, plus file tags and chapters. Tag keys and
values use either `text` or `bytes_hex` so invalid UTF-8 is not silently
changed. Packet indexes preserve original PTS, DTS, duration, flags, source
position, payload boundaries, demux sequence, and side-data references.
Timestamps use the recorded stream time base; missing timestamps remain null.

Video/audio tracks additionally get a compatible MP4/M4A or Matroska copy when
available. MP4 copies are fragmented to bound sample-table memory; Matroska
copies use live mode to avoid retaining a whole-file cue index. A muxer can
normalize container timestamps, but the raw packet index retains their original
values. Unsupported playable copies produce warnings; their raw packets remain
available.

Raw packet files and playable copies deliberately duplicate encoded video/audio
payloads. Plan for roughly twice the media size plus metadata. Data streams need
only their packet artifacts. Original non-media boxes and the complete tail are
preserved, but unused bytes inside `mdat` are not copied: this is component
extraction, not a byte-exact backup of the complete input file.

## Verification

Generated fixtures exercise dual video, AAC audio, timecode/data tracks, opaque
repeated records, V2/V3 tails, and destination ownership. Tests compare original
packet boundaries and payload bytes with both raw artifacts and playable copies.
Direct-reader tests cover independent cursors, decoder draining, seeking, and
frame access without output files. No camera-specific rendering result is needed
to qualify encoded stream copying.

### Continuous stream timestamp preservation

Continuous MP4 copies delay their initial movie header until the first fragment
so the muxer can represent edit lists for AAC priming and reordered video.
Automatic negative timestamp rebasing is disabled: independent camera and audio
files retain the same source origin instead of receiving different shifts.
Fragmentation still bounds sample-table memory. Regression tests compare native
packet PTS/DTS and encoded payloads on both sides of a chapter boundary.

Camera A/B names require an explicit recorded lens order and exactly two video
tracks, or the declared two-file lens layout with one video track per file.
Additional proxy tracks retain neutral stream names; all tracks are archived.
