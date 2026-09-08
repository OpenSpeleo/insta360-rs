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
continuation segments are separate inputs; extraction does not recursively
search unrelated captures.

The destination must be absent or an empty directory. Symlink destinations and
nonempty directories are rejected. Extraction runs in an owned sibling staging
directory; errors remove that directory, and successful completion publishes the
complete result. `ExtractionReport` contains absolute output paths,
input/stream/ record counts, generated files, and warnings.

## Output structure

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
