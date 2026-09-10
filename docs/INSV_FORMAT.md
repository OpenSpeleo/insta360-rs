# Insta360 INSV format specification

## Scope and conventions

This document defines the INSV recording layouts, ISO Base Media File Format
(ISO-BMFF) media structure, ExtraInfo tail versions 2 and 3, metadata fields,
calibration encodings, and telemetry layouts used by this project. It covers
ordinary 360-video recordings from ONE X, ONE X2, X3, X4, X5, and X6, with X4
Air included for layout compatibility. Low-resolution `.lrv` proxies and related
`.mp4` media are described where they affect recording discovery.

**MUST**, **MUST NOT**, **SHOULD**, and **MAY** specify reader and writer
requirements within the profiles defined here. A payload marked **opaque** has
known framing but no complete semantic encoding defined in this document.
Preserving an opaque payload does not imply support for processing its contents.
The final implementation-support section distinguishes these requirements from
what `insta360-rs` currently implements.

Byte offsets are zero-based. Ranges use an exclusive end. Integer widths are
exact: `u8`, `u16`, `u32`, and `u64` are unsigned; `i32` and `i64` are signed.
`f32` and `f64` are IEEE-754 binary floating-point values. **BE** means
big-endian; **LE** means little-endian. Binary structures have no implicit
alignment or padding. Text signatures contain exactly the stated bytes, without
a terminating NUL.

The following version spaces are independent:

| Identifier             | Meaning                                             | Values described here           |
| ---------------------- | --------------------------------------------------- | ------------------------------- |
| Recording layout       | Distribution of lens images across files and tracks | Packed, split-file, multi-track |
| ExtraInfo version      | Tail framing                                        | 2, 3                            |
| Record format          | Encoding of one tail payload                        | Interpreted per record ID       |
| Calibration generation | Lens parameter serialization and projection family  | V1, V2, V3, V6                  |
| Camera generation      | Camera model                                        | ONE X through X6                |

A camera model or file extension MUST NOT be used to infer any of the other
version spaces without inspecting the file.

## Recording layouts and format capabilities

An INSV file contains encoded media tracks and camera-specific ExtraInfo data.
Lens images are encoded samples within tracks; they are not nested MP4 files.
The media and ExtraInfo regions have separate validity: damaged or absent
ExtraInfo can leave the media playable while preventing calibrated stitching or
stabilization.

| Layout                | Main files per segment | Lens-image organization                                              | Processing requirement                                                  |
| --------------------- | ---------------------: | -------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| Packed dual fisheye   |                      1 | Two fisheye views in one decoded pixel buffer, commonly side by side | Identify each lens region and apply the correct crop/calibration        |
| Split-file pair       |                      2 | One lens stream in each of the `_00_` and `_10_` files               | Open both files, obtain primary metadata, and synchronize their samples |
| Multi-track           |                      1 | Two video tracks, one per lens                                       | Decode both tracks and apply recorded track order                       |
| Single-lens or planar |         Mode-dependent | One selected lens or already processed view                          | Inspect projection/category; do not assume two-lens stitching           |

```text
Packed                         Split-file                       Multi-track
recording.insv                 VID_..._00_....insv               recording.insv
  video: [lens A | lens B]        video: lens 00                   video track 0
  optional audio/data            primary metadata                 video track 1
  ExtraInfo                    VID_..._10_....insv                 optional audio/data
                                 video: lens 10                   ExtraInfo
                                 companion metadata
```

Packed frames describe decoded image organization. They do not establish the
track count of every lower-resolution or special recording mode.

| File kind    | Purpose                                             | Media organization                     | ExtraInfo handling                                                    |
| ------------ | --------------------------------------------------- | -------------------------------------- | --------------------------------------------------------------------- |
| `.insv` main | Original camera video                               | Any applicable recording layout above  | Required when processing depends on embedded calibration or telemetry |
| `.lrv` proxy | Optional low-resolution preview of a main recording | Can contain both fisheyes in one frame | Probe independently; do not assume it duplicates the main tail        |
| `.mp4`       | Generic media or rendered export                    | Inspect tracks and projection          | Extension alone does not indicate whether camera data survives        |

Renaming `.insv` to `.mp4` changes no bytes and performs no stitching. A player
may show only one video track of a multi-track recording. A generic remux can
retain encoded media while dropping ExtraInfo.

### Camera and mode compatibility

The table describes ordinary 360-video capture. Capture-resolution labels refer
to a recording mode or intended output, not necessarily the coded dimensions of
a stored lens track.

| Camera      | Ordinary main-file organization                                        | Relevant format variations                                                       |
| ----------- | ---------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| ONE X (X1)  | At 5.7K and above, `_00_`/`_10_` split pair; below 5.7K, one main file | Packed lower-resolution frames; optional second thumbnail and frame-time records |
| ONE X2 (X2) | At 5.7K and above, split pair; below 5.7K, one main file               | Current/original V2 and V3 calibration can coexist; inspect actual fields        |
| X3          | At 5.7K and above, split pair; below 5.7K, one main file               | Camera-specific inertial coordinates and lens-guard states                       |
| X4          | One file with two lens video tracks                                    | High-resolution modes, including 8K; accessory-specific calibration              |
| X4 Air      | One file with two lens video tracks                                    | Shares the modern multi-track layout                                             |
| X5          | One file with two lens video tracks                                    | Multiple simultaneous calibration generations; guard, dive-case, and ND states   |
| X6          | One file with two lens video tracks                                    | V6 calibration and 10-bit media paths; inspect actual bit depth and codec        |

Bullet Time, TimeShift, timelapse, HDR, high-frame-rate, webcam, single-lens,
loop, and pre-record modes can change grouping, tracks, timing, proxies, and
telemetry. Neither a universal codec nor a universal tail or calibration version
is assigned to any model by this specification.

### Layout detection and lens order

A reader SHOULD determine the recording layout as follows:

1. Read metadata fields `131` (stream type) and `79` (panorama record type).
2. Read field `80` (multi-track order) and inspect actual video tracks.
3. If a second lens belongs to another file, locate the corresponding
   `_00_`/`_10_` sibling and validate its recording identity.
4. Validate the decoded image category and dimensions before treating a single
   video frame as packed dual fisheye.
5. Use camera name and capture resolution only as fallback hints.

Track indices here are zero-based indices among video tracks, not ISO-BMFF
`track_ID` values and not necessarily global demuxer stream indices. Field `80`
value `1` identifies track 0 as stream `10`; value `2` identifies it as stream
`00`. An explicit field `80` takes precedence over the reversal hint in field
`131`. Contradictory layout declarations and actual tracks MUST be reported; a
stitcher MUST NOT invent an absent lens stream.

For split files, `_00_` is the primary input and `_10_` is the companion.
Primary metadata can contain calibration needed by both lenses. Preserve both
files and their association. Pair lens samples using presentation time,
accounting for track time bases and edits; equal frame indices alone do not
establish temporal alignment.

## ISO-BMFF media structure

### Box framing

Each box begins with the following header:

| Offset | Size | Encoding              | Meaning                                             |
| -----: | ---: | --------------------- | --------------------------------------------------- |
|      0 |    4 | `u32` BE              | Total box size, including its header                |
|      4 |    4 | Four bytes            | Box type, such as `ftyp`, `moov`, `mdat`, or `inst` |
|      8 |    8 | `u64` BE, conditional | Extended total size when the first size word is `1` |

An ordinary header is eight bytes. A size word of `1` selects a sixteen-byte
header. A size word of `0` extends the box to the end of its enclosing region;
for a top-level box, that is EOF. A parser MUST validate the minimum header
length and the complete box range before reading its payload. Unknown boxes can
be skipped using their lengths. An unknown `uuid` box also carries a
sixteen-byte user type after the size/type header; its contents remain opaque
here.

Container fields are BE. ExtraInfo fields are LE where specified below. Readers
MUST switch byte order at the format boundary.

### Movie and track organization

The V3 profile has the following top-level form. The media boxes need not occur
in the order shown, but `inst` is terminal and `ftyp` starts the file in the
profile accepted by the project reader.

```text
ftyp                         file type and compatible brands
moov                         movie and track descriptions
  mvhd                       movie time base
  trak                       repeated for each track
    tkhd                     track identifier and presentation geometry
    edts/elst                optional edits into the movie timeline
    mdia
      mdhd                   track time base and duration
      hdlr                   handler type: vide, soun, or another type
      minf/stbl
        stsd                 sample descriptions and codec configuration
        stts                 decoding-time deltas
        ctts                 optional composition-time offsets
        stsc                 sample-to-chunk mapping
        stsz or stz2         sample sizes
        stco or co64         absolute chunk byte offsets
        stss                 optional sync-sample table
mdat                         encoded samples; may be interleaved across tracks
other boxes                  optional padding or extensions
inst                         V3 ExtraInfo, including terminal header
EOF
```

`ftyp` begins with a four-byte major brand, a BE `u32` minor version, and zero
or more four-byte compatible brands. No single brand is required for every
recording. The box type `inst` identifies the ExtraInfo container; its bytes are
not encoded video samples.

The useful track-header fields below are relative to each box's payload, unless
explicitly stated otherwise. Full-box payloads begin with one version byte and
three flag bytes.

| Structure                    | Offset and encoding                                | Interpretation                                                           |
| ---------------------------- | -------------------------------------------------- | ------------------------------------------------------------------------ |
| `hdlr`                       | Offset 8, four bytes                               | Handler type; `vide` identifies video                                    |
| `mdhd` version 0             | Offset 12: `u32` BE; offset 16: `u32` BE           | Timescale and duration in track ticks                                    |
| `mdhd` version 1             | Offset 20: `u32` BE; offset 24: `u64` BE           | Timescale and duration in track ticks                                    |
| `stsd`                       | Offset 4: `u32` BE; entries start at offset 8      | Sample-description count and boxed sample entries                        |
| Ordinary visual sample entry | Offsets 32 and 34 from the entry's start: `u16` BE | Coded width and height                                                   |
| `stts`                       | Offset 4: `u32` BE; entries at offset 8            | Count followed by pairs of `sample_count`, `sample_delta`, both `u32` BE |

For `stts` entries `(n_i, d_i)`, total sample count is `sum(n_i)` and total
sample ticks are `sum(n_i * d_i)`. Their ratio to the track timescale determines
average frame rate; it does not prove constant frame timing. Presentation time
also includes `ctts` and applicable edit-list mapping. A zero timescale cannot
be used to convert ticks into seconds.

A demuxer locates each sample through the chunk map and sample-size table, then
uses that sample's selected `stsd` description to decode it. Chunk offsets are
absolute file offsets. Rewriting box placement can therefore require updating
`stco`/`co64` even when encoded samples remain unchanged. Fragmented media
(`moof`/`traf`/`trun` timing and sample addressing) is outside the movie-table
profile specified here.

### Codecs, dimensions, and color

Camera-video codec values include H.264/AVC, H.265/HEVC, and MJPEG. Common
sample entries include `avc1`, `hvc1`, and `hev1`; the actual sample entry and
its codec configuration determine decoding. Codec bitstreams are carried as
opaque sample payloads at the container-parser layer.

A reader MUST inspect each track's codec, coded dimensions, pixel format, bit
depth, color properties, and time base. The two lens tracks need not have
identical headers. Metadata's intended capture/output dimensions MUST NOT
replace the coded dimensions. A 10-bit input requires a decoder and processing
path that retain that depth when lossless depth preservation is requested.

Audio, timecode, and other data tracks are optional. AAC audio and timecode can
coexist with two HEVC video tracks, but neither is a mandatory INSV component.

## ExtraInfo version compatibility

| Property         | Version 2                                 | Version 3 indexed                      | Version 3 sequential               |
| ---------------- | ----------------------------------------- | -------------------------------------- | ---------------------------------- |
| Outer framing    | Raw regions appended after ordinary media | Terminal `inst` box                    | Terminal `inst` box                |
| Terminal header  | 40 bytes                                  | 72 bytes                               | 72 bytes                           |
| Metadata         | JSON payload                              | Record `0x01`; format selects encoding | Same record encoding as indexed V3 |
| Gyro             | Optional separate protobuf wrapper        | Record `0x03`                          | Record `0x03`                      |
| Record footers   | None                                      | Six bytes per record                   | Six bytes per record               |
| Directory        | None                                      | Final record `(format=0, id=0)`        | None; walk record footers backward |
| Other record IDs | No general record framing defined         | Extensible ID catalogue                | Same catalogue                     |

V3 sequential framing is not V2 framing. A common terminal signature does not
make their preceding structures interchangeable.

### Tail detection

Let `F` be the complete file length. The final 32 bytes of both defined tail
versions are the ASCII signature:

```text
8db42d694ccc418790edff439fe026bf
```

The LE `u32` immediately before this signature is the version. A bounded reader
SHOULD read the final 40 bytes first, check the signature, then dispatch by
version. Version `3` requires at least the complete 72-byte terminal header and
a valid `inst` box. Version `2` uses the final 40-byte header directly. An
unknown version MUST remain unsupported; it MUST NOT be interpreted as V3 merely
because the signature matches.

If a recognized version has invalid sizes or framing, report a damaged tail of
that version. Do not retry it as another layout. Probe media validity separately
where partial inspection is required. For V2, determine the appended-region
boundary before walking top-level media boxes, because the V2 tail is not a box.

## ExtraInfo version 3

### Terminal header and enclosing box

The final 72 bytes are:

| Offset within terminal header | Size | Encoding     | Meaning                                                                              |
| ----------------------------: | ---: | ------------ | ------------------------------------------------------------------------------------ |
|                             0 |   32 | Opaque bytes | Reserved terminal data; preserve existing values                                     |
|                            32 |    4 | `u32` LE     | ExtraInfo size `E`, including this header but excluding the eight-byte `inst` header |
|                            36 |    4 | `u32` LE     | Version, equal to `3`                                                                |
|                            40 |   32 | ASCII        | Common terminal signature                                                            |

For the eight-byte `inst` header profile:

```text
inst_start       = F - E - 8
payload_start    = inst_start + 8 = F - E
terminal_start   = F - 72
record_region    = [payload_start, terminal_start)
inst_total_size  = E + 8
```

Every subtraction and addition MUST use checked arithmetic. `E` MUST be at
least 72. The computed start MUST coincide with a top-level `inst` box whose BE
size is `E + 8` and whose end is EOF. The enclosing ordinary size word must be
able to represent that total. An extended-size `inst` variant is not defined by
these formulas and MUST NOT be accepted by silently changing the payload origin.

The reserved bytes have no defined processing meaning. A new tail may zero them;
a preserving rewrite SHOULD retain them. The ASCII signature is an identifier,
not a cryptographic checksum.

### Record footer

A record is a payload of `L` bytes immediately followed by a six-byte footer:

| Offset within footer | Size | Encoding | Meaning                              |
| -------------------: | ---: | -------- | ------------------------------------ |
|                    0 |    1 | `u8`     | Record format                        |
|                    1 |    1 | `u8`     | Record ID                            |
|                    2 |    4 | `u32` LE | Payload length `L`, excluding footer |

There is no implicit record alignment. A record's physical range includes its
footer. Formats are scoped to their record ID; format `1` does not mean that
every kind of record contains protobuf.

### Indexed records

The directory is the final record before the terminal header. Its footer has
`format=0`, `id=0`, and payload length `D`. `D` MUST be a multiple of ten. An
empty directory has `D=0` and still occupies a six-byte footer.

```text
directory_footer = terminal_start - 6
directory_start  = directory_footer - D
```

Directory entries have this packed layout:

| Offset within entry | Size | Encoding | Meaning                                        |
| ------------------: | ---: | -------- | ---------------------------------------------- |
|                   0 |    1 | `u8`     | Record ID                                      |
|                   1 |    1 | `u8`     | Record format                                  |
|                   2 |    4 | `u32` LE | Record payload length `L`                      |
|                   6 |    4 | `u32` LE | Payload offset `R` relative to `payload_start` |

For each entry, compute `record_start = payload_start + R` and
`record_end = record_start + L + 6`. Require:

```text
payload_start <= record_start
record_end <= directory_start
directory_start >= payload_start
```

The footer at `record_start + L` MUST match the entry's format, ID, and length.
Reject overlapping record ranges and duplicate entries referring to the same
bytes. Repeated IDs at different, non-overlapping locations are permitted; do
not equate record ID with a globally unique key. ID `0` is reserved for
directory control and does not designate a typed media payload.

Directory order need not equal physical order or ID order. A preserving writer
SHOULD retain both the physical record order and the original directory order.
An entry containing ten zero bytes is an unused directory slot and has no
payload or record footer. Readers MUST skip it while preserving the original
directory bytes. A nonempty entry with reserved ID zero is not a typed record.
Offsets are measured from the beginning of the `inst` payload, not from EOF or
from the beginning of the file.

### Sequential records

If the footer immediately before the terminal header is not the directory marker
`(format=0, id=0)`, interpret the record region as a backward chain:

1. Set `cursor = terminal_start`.
2. Read the footer at `cursor - 6` within the record region.
3. Decode `L`, then calculate `record_start = cursor - 6 - L`.
4. Validate and retain the record. Set `cursor = record_start`.
5. Repeat until `cursor == payload_start`.

A partial footer, leftover prefix bytes, underflow, or an out-of-region record
is malformed. A header-only empty region can be represented structurally, but
has no metadata or media-processing records. Readers supporting only indexed V3
MAY reject sequential or empty tails explicitly.

### Minimal indexed-tail example

A metadata payload containing only camera name `X5` is the protobuf byte
sequence `12 02 58 35`. This example is a structural test vector, not a complete
movie or usable lens calibration:

```text
00 00 00 6a 69 6e 73 74       inst: total size 106, type "inst"
12 02 58 35                   metadata payload: field 2, length 2, "X5"
01 01 04 00 00 00             record footer: format 1, ID 1, length 4
01 01 04 00 00 00 00 00 00 00 directory entry: ID 1, format 1, length 4, offset 0
00 00 0a 00 00 00             directory footer: format 0, ID 0, length 10
[32 zero bytes]               reserved terminal bytes
62 00 00 00                   E = 98
03 00 00 00                   ExtraInfo version 3
[32 ASCII signature bytes]    8db42d694ccc418790edff439fe026bf
```

The directory begins ten bytes into the payload. The metadata record including
its footer ends exactly at that position.

## ExtraInfo version 2

V2 appends the following raw regions to the media:

```text
[ordinary media]
[optional gyro payload]
[44-byte gyro header, if gyro is present]
[JSON metadata payload]
[40-byte metadata header]
EOF
```

The final metadata header is:

| Offset | Size | Encoding | Meaning                     |
| -----: | ---: | -------- | --------------------------- |
|      0 |    4 | `u32` LE | Metadata payload length `M` |
|      4 |    4 | `u32` LE | Version, equal to `2`       |
|      8 |   32 | ASCII    | Common terminal signature   |

Compute `metadata_start = F - 40 - M`. The metadata region is
`[metadata_start, F - 40)`. Its JSON key schema is outside the typed metadata
profile below; it MUST NOT be decoded using protobuf field numbers.

When present, the 44 bytes immediately before `metadata_start` are:

| Offset within gyro header | Size | Encoding | Meaning                                                    |
| ------------------------: | ---: | -------- | ---------------------------------------------------------- |
|                         0 |    4 | `u32` LE | Gyro payload length `G`                                    |
|                         4 |    8 | Reserved | Zero in newly constructed headers; preserve existing bytes |
|                        12 |   32 | ASCII    | `9c792b1ac55c40418d36ffb0d1d16b58`                         |

Compute `gyro_start = metadata_start - 44 - G` only after checking the gyro
signature and header bounds. With gyro present, ordinary media ends at
`gyro_start`; otherwise it ends at `metadata_start`. A recognized gyro header
with an impossible length is damaged gyro data, not a usable empty gyro record.

The gyro payload is a protobuf envelope. Its field `1`, wire type `2`, contains
a gyro message with these nested fields:

| Field | Wire/type           | Meaning                                               |
| ----: | ------------------- | ----------------------------------------------------- |
|     1 | 0, protobuf `int64` | Time offset; time unit is not defined here            |
|     2 | 0, bool             | Application-state flag                                |
|     3 | 0, protobuf `int32` | Declared sample count                                 |
|     4 | 0, enum             | Camera family: `0` Nano, `1` Nano2, `2` Air, `3` Air2 |
|     7 | 2, bytes            | Encapsulated gyro samples                             |

Fields `5` and `6` have no definition in this profile. The nested sample bytes
remain opaque: their encoding is not established by the envelope. Do not pass
the complete wrapper or assume its nested bytes can be passed to a V3 flat gyro
decoder. V2 has no six-byte record footers or ten-byte directory entries.

## ExtraInfo record catalogue

The following IDs apply to V3 records. Presence is optional at the file level; a
requested operation can require particular records. **Typed** means a payload
layout is defined later in this document. **Opaque** means that only the framing
and broad purpose are defined.

| ID     | Record               | Payload definition or role                                              |
| ------ | -------------------- | ----------------------------------------------------------------------- |
| `0x00` | Directory            | Ten-byte entries; control record, not media data                        |
| `0x01` | Metadata             | Typed protobuf subset for format 1; legacy JSON is a separate encoding  |
| `0x02` | Thumbnail            | Opaque embedded preview image                                           |
| `0x03` | Primary gyro         | Typed common or packed raw inertial samples                             |
| `0x04` | Primary exposure     | Typed timestamp/shutter pairs                                           |
| `0x05` | Extended thumbnail   | Opaque additional preview, including legacy 5.7K recordings             |
| `0x06` | Frame PTS            | Typed timestamp sequence for frame-time mapping                         |
| `0x07` | GPS                  | Opaque positioning data                                                 |
| `0x08` | GPS reception        | Opaque satellite/signal-quality samples                                 |
| `0x09` | AAA                  | Opaque automatic exposure/ISO analysis                                  |
| `0x0A` | Anchors              | Opaque highlights or capture markers                                    |
| `0x0B` | AAA simulation       | Opaque processing data                                                  |
| `0x0C` | Secondary exposure   | Same flat sample shape as primary exposure; second image stream         |
| `0x0D` | Magnetic             | Opaque magnetometer samples                                             |
| `0x0E` | Euler                | Opaque orientation samples                                              |
| `0x0F` | Secondary gyro       | Inertial data for a second sensor/stream; verify its own layout context |
| `0x10` | Speed                | Opaque speed-change/time-control data                                   |
| `0x11` | TBox                 | Opaque telemetry-box data                                               |
| `0x12` | Editor               | Opaque editor state                                                     |
| `0x13` | Heart rate           | Opaque heart-rate samples                                               |
| `0x14` | Forward direction    | Opaque chosen forward-view direction                                    |
| `0x15` | Up view              | Opaque chosen up direction                                              |
| `0x16` | Shell recognition    | Opaque detected housing/accessory state                                 |
| `0x17` | Position             | Opaque positional samples                                               |
| `0x18` | Timelapse quaternion | Opaque timelapse orientation                                            |
| `0x19` | Quaternion           | Opaque orientation samples                                              |
| `0x1A` | APEI                 | Opaque processing data                                                  |
| `0x1B` | Dynamic ISP          | Opaque dynamic image-processing parameters                              |
| `0x1C` | Static ISP           | Opaque static image-processing parameters                               |
| `0x1D` | AE flicker           | Opaque exposure-flicker data                                            |
| `0x1E` | Vehicle info         | Opaque vehicle-specific telemetry                                       |
| `0x80` | Time map             | Opaque edited-to-original time mapping                                  |

Unknown IDs and formats are extension points. A reader MAY expose their payload
bytes without decoding them. A preserving writer SHOULD retain unknown records
and MUST NOT replace them with an empty or guessed interpretation. Historical
variants can assign different semantics to IDs `0x11` and `0x12`; these records
remain opaque without an identified encoding profile.

## Metadata record `0x01`

### Protobuf encoding

Record format `1` identifies protobuf metadata in this profile. Format `2` is
associated with legacy JSON metadata. Other values are unsupported unless an
encoding is explicitly identified; they MUST NOT be assumed to mean JSON or
protobuf. Presence of a metadata record does not require every field below.

A protobuf key is the unsigned varint `(field_number << 3) | wire_type`. Varints
store seven value bits per byte, least-significant group first, with bit 7
indicating continuation. Field numbers range from `1` to `2^29 - 1`. The
supported wire primitives are:

| Wire type | Encoding                                                                         |
| --------: | -------------------------------------------------------------------------------- |
|         0 | Unsigned varint; interpreted as integer, bool, or enum according to field        |
|         1 | Eight bytes LE; fixed64 or `f64`                                                 |
|         2 | Varint byte length followed by that many bytes; string, bytes, or nested message |
|         5 | Four bytes LE; fixed32 or `f32`                                                  |

Strings are UTF-8 without a terminator. Nested messages are length-delimited;
the outer field length bounds all nested reads. Signed protobuf `int32` values
use two's-complement varints, not ZigZag `sint32` encoding. A negative `int32`
can therefore occupy ten bytes on the wire. Unknown fields MUST remain skippable
by their actual wire type. An unexpected wire type for a known field MUST NOT be
coerced into the expected type.

### Field registry

This registry defines the metadata subset covered by this specification.
Implementation coverage is identified below. Unlisted fields remain opaque.
Numeric values and field presence MUST be retained separately; an absent field
is not necessarily equivalent to the enum's `Unknown` value.

| Field | Wire        | Meaning                                                                                                                                                      |
| ----: | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
|     1 | 2, string   | Camera serial number                                                                                                                                         |
|     2 | 2, string   | Camera name                                                                                                                                                  |
|     3 | 2, string   | Firmware version                                                                                                                                             |
|     5 | 2, string   | Current V1 calibration                                                                                                                                       |
|    17 | 2, string   | Original V1 calibration                                                                                                                                      |
|    19 | 2, vector   | File/image dimensions                                                                                                                                        |
|    20 | 0, `int32`  | Nominal frame rate; actual track timing takes precedence                                                                                                     |
|    24 | 0, `int64`  | First-frame camera timestamp; apply the recording's time unit                                                                                                |
|    25 | 1, `f64`    | Rolling-shutter readout value; X5 stores milliseconds for the recorded resolution                                                                            |
|    26 | 2, message  | Recording group information, defined below                                                                                                                   |
|    27 | 2, message  | Crop window, defined below                                                                                                                                   |
|    28 | 1, `f64`    | Gyro timestamp adjustment; interpreted as milliseconds by the project                                                                                        |
|    29 | 0, bool     | Whether gyro timestamp adjustment is declared                                                                                                                |
|    30 | 0, `u32`    | Legacy timelapse interval; field 59 selects milliseconds versus seconds                                                                                      |
|    31 | 2, bytes    | Recorded IMU calibration; observed X5 layout is six little-endian f64 values followed by u64 (56 bytes); bias semantics and timestamp unit remain unverified |
|    44 | 2, vector   | Intended rendered/exported dimensions                                                                                                                        |
|    53 | 2, string   | Current V2 calibration                                                                                                                                       |
|    54 | 2, string   | Current V3 calibration                                                                                                                                       |
|    55 | 2, string   | Original V2 calibration                                                                                                                                      |
|    56 | 2, string   | Original V3 calibration                                                                                                                                      |
|    59 | 0, bool     | Legacy interval is in milliseconds when true, seconds when false                                                                                             |
|    62 | 0, bool     | Raw inertial sample layout flag                                                                                                                              |
|    64 | 0, enum     | Gyro/video presentation-time mapping strategy                                                                                                                |
|    65 | 2, message  | Inertial measurement ranges, defined below                                                                                                                   |
|    68 | 0, enum     | Selected optical/accessory state                                                                                                                             |
|    78 | 2, message  | Rational frame rate: numerator and denominator                                                                                                               |
|    79 | 0, enum     | Panorama recording organization                                                                                                                              |
|    80 | 0, enum     | Multi-track lens order                                                                                                                                       |
|   102 | 0, bool     | Presence flag for the additional resolution vector                                                                                                           |
|   103 | 2, vector   | Additional resolution vector; processing meaning unspecified                                                                                                 |
|   104 | 0, enum     | Automatic guard-detection result                                                                                                                             |
|   111 | 2, string   | Current V6 calibration                                                                                                                                       |
|   112 | 2, string   | Original V6 calibration                                                                                                                                      |
|   127 | 0, `int32`  | Milliseconds trimmed from playback duration to align unequal streams                                                                                         |
|   128 | 0, `int32`  | Full stitch blend angle in degrees                                                                                                                           |
|   129 | 0, enum     | File image/projection category                                                                                                                               |
|   130 | 0, enum     | Encoded image rotation                                                                                                                                       |
|   131 | 0, enum     | Physical stream layout                                                                                                                                       |
|   132 | 0, enum     | Video codec hint                                                                                                                                             |
|   133 | 0, `uint64` | Direct timelapse interval in milliseconds                                                                                                                    |
|   134 | 0, bool     | Pre-record capture mode                                                                                                                                      |
|   135 | 0, `uint64` | Expected bitrate in bits per second                                                                                                                          |
|   136 | 0, enum     | Capture calibration generation                                                                                                                               |
|   145 | 2, bytes    | Extension containing candidate embedded optical-profile messages; preserve raw bytes                                                                         |
|   148 | 2, vector   | Expected panorama aspect ratio; not coded pixel dimensions                                                                                                   |
|   186 | 0, enum     | Lens-accessory mapping recognized by the project                                                                                                             |
|   193 | 0, bool     | X5 P3 correction/fallback flag recognized by the project                                                                                                     |

Field `30` is defined here as an integer. The current parser instead interprets
a wire-5 float at this field number and retains the wire-0 integer as an unknown
field. Float acceptance is an implementation behavior whose applicability to
recorded files is not established by this specification. Field `133`, when
present, takes precedence over a derived legacy interval. Fields `145`, `186`,
and `193` are extension interpretations and MUST NOT be required for general
metadata decoding. Field `145` is not an authoritative accessory-selection field
merely because profile-like messages occur within it.

### Layout and processing enums

All values are decimal. Unlisted numeric values remain unknown values of their
respective enum; they MUST NOT select the first known variant by default.

| Field | Values                                                                                                                                           |
| ----: | ------------------------------------------------------------------------------------------------------------------------------------------------ |
|    64 | `0` unknown; `1` decoder timing with first-frame timestamp; `2` mapping carried by exposure data                                                 |
|    79 | `0` unknown; `1` split-file panorama; `2` multi-track panorama                                                                                   |
|    80 | `0` unknown; `1` track 0 is stream `10`; `2` track 0 is stream `00`                                                                              |
|   129 | `0` unknown; `1` plane; `2` double-fisheye panorama; `3` fisheye; `4` wide angle; `5` double-half-fisheye panorama; `6` equirectangular panorama |
|   130 | `0` unknown; `1` 0 degrees; `2` 90 degrees; `3` 180 degrees; `4` 270 degrees                                                                     |
|   131 | `0` unknown; `1` single-stream file; `2` two streams in separate files; `3` two tracks in one file; `4` two tracks in reversed order             |
|   132 | `0` unknown; `1` H.264; `2` H.265; `3` MJPEG; `4` H.264 intra; `5` H.265 intra                                                                   |
|   136 | `0` unknown; `1` V1; `2` V2; `3` V3; `4` V6                                                                                                      |

The value `4` in field `136` means calibration V6, not calibration V4. The
project currently maps codec field `132` differently: `1` unknown, `2` H.264,
`3` H.265, and `4` MJPEG. This mapping conflicts with the wire enum above.
Preserve the raw number and determine the encoded codec from the actual track
sample entry; do not dispatch a decoder from the project's metadata enum alone.

Fields exposed by an API as `videoTrackCount` and `reverseVideoTrackOrder` can
be derived from actual tracks and fields `80`/`131`; they are not additional
protobuf fields assigned by this table.

### Optical/accessory enums

These values specify capture state; they do not contain the optical calibration
needed to render that state.

| Field 68 value | Optical state                               |
| -------------: | ------------------------------------------- |
|              0 | Common/bare configuration                   |
|              1 | Spherical protector                         |
|              2 | Dive case underwater                        |
|              3 | 2023 dive case underwater                   |
|              4 | X4 plastic lens guard                       |
|              5 | X4 glass lens guard                         |
|              6 | Automatic; interpret guard-detection result |
|              7 | ND16                                        |
|              8 | ND32                                        |
|              9 | ND64                                        |
|             10 | Dive Case Pro underwater                    |
|             11 | Dive Case Pro above water                   |
|             12 | ND128                                       |

| Field | Values                                                                                                                                                                                                                                                    |
| ----: | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
|   104 | `0` unknown; `1` plastic; `2` glass; `3` off; `4` averaged plastic/glass; `5` ND16; `6` ND32; `7` ND64; `8` ND128                                                                                                                                         |
|   186 | `0` unknown; `1` standard; `2` black-mist filter; `3` wide-angle lens; `4` adjustable macro; `5` anamorphic; `6` star filter; `7` ND filter; `8` micro lens; `9` 5 cm micro lens; `10` 11.6 cm micro lens; `11` 21.5 cm micro lens; `12` 38 cm micro lens |

Camera identity is part of accessory interpretation. The same numeric lens ID
can have different camera-family meanings. A missing or unsupported accessory
conversion MUST NOT silently use bare-air calibration.

### Nested crop and inertial configuration

Field `27` contains this crop message. All six fields use wire type `0`:

| Nested field | Type             | Meaning                                          |
| -----------: | ---------------- | ------------------------------------------------ |
|            1 | `u32`            | Source/sensor width                              |
|            2 | `u32`            | Source/sensor height                             |
|            3 | `u32`            | Destination/encoded width                        |
|            4 | `u32`            | Destination/encoded height                       |
|            5 | Protobuf `int32` | Signed horizontal crop offset; absent means zero |
|            6 | Protobuf `int32` | Signed vertical crop offset; absent means zero   |

All four dimensions must be present and positive for a usable crop. A negative
crop offset is a signed coordinate, not an enormous unsigned pixel position.
Keep the crop's source and destination geometry separate from each track's coded
dimensions until their relationship has been validated.

Field `65` contains two wire-0 `u32` fields: nested field `1` is the positive
accelerometer full-scale range in g, and nested field `2` is the positive
gyroscope full-scale range in degrees per second. These ranges scale packed raw
samples; they do not change the sample stride.

### Dimensions, frame rate, and grouping

Vector fields `19`, `44`, `103`, and `148` contain nested wire-0 protobuf
`int32` fields `1` (X/width) and `2` (Y/height). Validate positive dimensions or
ratio components before using them. The additional field-103 vector has no
defined priority over actual sample-entry dimensions. Field `148` is a
width/height ratio and MUST NOT be interpreted as a pixel size.

Field `78` contains wire-0 `u32` fields `1` (numerator) and `2` (denominator). A
denominator of zero is invalid for division. Rational frame rate can preserve
rates such as `30000/1001`; integer nominal frame rate cannot express that ratio
exactly. Track timestamps still determine sample presentation time.

Field `26` contains a recording-group message:

| Nested field | Wire/type | Meaning                                                    |
| -----------: | --------- | ---------------------------------------------------------- |
|            1 | 0, enum   | Capture/group type                                         |
|            2 | 0, `u32`  | Index within the group; numbering origin is not fixed here |
|            3 | 2, string | Group identity                                             |
|            4 | 0, `u32`  | Declared total group members                               |

Group-type values are `0` normal video, `1` Bullet Time, `2` timelapse video,
`3` normal photo, `4` HDR photo, `5` interval photo, `6` HDR video, `7` burst
photo, `8` static timelapse video, `9` TimeShift, `10` AEB night photo, `11`
super-normal video, `12` loop recording, `13` Starlapse photo, `14` panorama
photo, `15` FPV video, `16` movie video, `17` slow-motion video, `18` selfie
video, `19` PureShot Plus photo, `20` Pure video, `21` Starlapse video, `22`
Startrail photo, `23` dashcam video, and `24` virtual-PTZ video. Photo values
share the metadata namespace; they do not make still-image payloads part of this
INSV video specification. Unlisted values remain unknown.

Grouping, dimension-vector, and rational-frame-rate messages are currently
retained as top-level unknown values by the project rather than interpreted.

## Lens calibration encoding

### String grammar and generation selection

A calibration offset is an underscore-delimited ASCII numeric string. It is not
a byte offset. Token 0 is the lens count; the two-lens profile requires `2`.
Each subsequent token is a finite decimal number. Integer-valued fields must
represent nonnegative `u32` values exactly; dimensions must also be positive.
Readers may trim surrounding whitespace and NULs but MUST NOT discard tokens
inside the string.

| Generation | Total tokens for two lenses | Tokens per lens | Trailing global tokens     | Projection family                                  |
| ---------- | --------------------------: | --------------: | -------------------------- | -------------------------------------------------- |
| V1         |                          16 |               6 | Width, height, packed word | Polynomial pinhole V1; parse-only in project       |
| V2         |                          34 |              16 | Packed word                | Polynomial pinhole V2                              |
| V3         |                          40 |              19 | Packed word                | Unified omnidirectional radial/tangential          |
| V6         |                          56 |              27 | Packed word                | Extended unified omnidirectional radial/tangential |

The token sequence is always:

```text
lens_count _ [lens 0 values] _ [lens 1 values] _ [global values]
```

Current calibration can already incorporate an optical accessory. Original
calibration is the factory copy. A processor MUST make that choice explicit and
MUST NOT substitute current for original, or original for current, when the
requested form is absent. Several generations can coexist in either form.

The project prefers V6, V3, V2, then V1 within the requested form. This is a
selection policy, not a statement that later camera models contain only later
calibration generations. A supported token count alone is insufficient: validate
the packed generation, lens identifiers, finite values, and geometry.

### Ordered lens fields

The following lists give the exact order within each lens block. Indices are
relative to the start of that block.

| Generation | Ordered fields                                                                                                             |
| ---------- | -------------------------------------------------------------------------------------------------------------------------- |
| V1         | `radius, cx, cy, ex, ey, ez`                                                                                               |
| V2         | `radius, cx, cy, ex, ey, ez, tx, ty, tz, c0, c1, c2, c3, width, height, lens_type`                                         |
| V3         | `xi, fx, fy, cx, cy, ex, ey, ez, tx, ty, tz, k1, k2, k3, p1, p2, width, height, lens_type`                                 |
| V6         | `xi, fx, fy, cx, cy, ex, ey, ez, tx, ty, tz, k1, k2, k3, k4, k5, p1, p2, p3, p4, s1, s2, s3, s4, width, height, lens_type` |

`cx, cy` are principal-point coordinates and `fx, fy` are focal scales in the
calibration canvas. `radius` supplies the common focal scale in the project's
V1/V2 representation. `ex, ey, ez` are Euler angles in degrees. `tx, ty, tz` are
translation values whose physical unit is not established by this profile;
preserve them without assuming metres or pixels. `xi` is the unified-camera
model parameter. Focal scales `fx` and `fy`, or `radius` for V1/V2, MUST be
strictly positive. The distortion coefficients MUST remain in their native
order.

V1 shares global width, height, and lens type across both lens blocks. V2/V3/V6
carry width, height, and lens type in each block; the project requires matching
canvas dimensions and matching lens types for optical-setup resolution across
the two lenses. The calibration canvas is the geometry for the two lens views
and must be related to packed or separate decoded images before sampling pixels.

V3 has three radial and two tangential coefficients. V6 has five radial
coefficients, four tangential coefficients, and four thin-prism coefficients.
Treating V6 as a three-coefficient pinhole model loses defined parameters and
changes the lens projection. V2's four polynomial coefficients have their own
model-specific interpretation and MUST NOT be relabeled as V3 coefficients.

### Packed word

The last token is interpreted as a `u32`:

```text
V1:
  lens_type = packed & 0x03ff
  flags     = packed & ~0x03ff

V2, V3, V6:
  generation = packed >> 16
  flags      = packed & 0xffff
```

For V2/V3/V6, the high sixteen bits MUST equal `2`, `3`, or `6`, respectively.
The low flags are preserved without assigning unspecified meanings. For example,
`0x00060400` identifies V6 with flags `0x0400`; it does not identify the camera
as X6. In V1, `0x0471` encodes lens type `113` and flags `0x0400`.

### Orientation and optical setup

The project's calibration-to-camera rotation uses column vectors and the
following degree-based composition:

```text
R = Rz(ez) * Ry(ey + 90 degrees) * Rx(ex)
lens 0: R
lens 1: Ry(180 degrees) * R
```

The optical axis in camera space is positive Z. This basis conversion is part of
interpreting the calibration for the project renderer; the gyro axes require
separate camera-specific treatment and MUST NOT be assumed to share this basis.
Full rendering equations and accessory conversion are processing algorithms, not
additional fields in the calibration string.

Lens IDs are camera-scoped. For the X5 profile, examples include `113` for bare
air, `114` for bare underwater, `117` for the invisible dive case underwater,
and `118` for that case above water. A numeric ID without the corresponding
camera/optical interpretation does not supply a usable projection.

Field `128` supplies a full stitch blend angle in degrees. The project treats
zero as unspecified. A nonzero angle must lie within 180–360 degrees and not
exceed the resolved lens field of view. Exactly 180 degrees means zero angular
overlap and requires a finite hard seam. An accessory conversion can invalidate
the old blend-angle override.

### Embedded optical-profile messages

The field-145 extension can contain nested profile descriptors. Recognition is
limited to a matching message structure and name; the entire enclosing extension
must remain available as raw bytes if a lossless rewrite is needed. A descriptor
has one of these two forms:

| Nested field | Polynomial profile               | Classifier profile          |
| -----------: | -------------------------------- | --------------------------- |
|            1 | Wire 2: UTF-8 profile name       | Wire 2: UTF-8 profile name  |
|            2 | Six repeated wire-1 `f64` values | One wire-0 unsigned integer |

Do not combine classifier and polynomial values in one interpreted descriptor.
The six finite coefficients are ordered constant term first:

```text
physical_radius(theta_degrees) = c0 + c1*theta + c2*theta^2
                              + c3*theta^3 + c4*theta^4 + c5*theta^5
```

This polynomial has maximum degree five. Its independent variable is angle in
degrees, not radians. Classifier-only profiles cannot supply this curve. Packed
wire-2 coefficient arrays are outside the typed descriptor profile above.

Recognized names include `bare`, `BareUnderwater`, `ProtectorA`, `ProtectorS`,
`ProtectorAS`, `InvisibleDiveWater`, `InvisibleDiveAir`, `ND16`, `ND32`, `ND64`,
`invisibleDive`, `heat_bare`, and `heat_protector`. The mere presence of a name
does not prove that the accessory was installed; explicit recorded capture state
or a deliberate caller selection determines the requested setup.

The project can convert X5 V6 calibration to bare air, dive-case water, or
dive-case air when both required polynomial profiles are available. Other
configurations require an already suitable calibration or an explicit
unsupported result. A parsed profile is not sufficient evidence that every
conversion is implemented.

## Telemetry payloads and time domains

### Common inertial samples

In the common V3 gyro profile, record `0x03` contains repeated 56-byte samples:

| Sample offset |   Size | Encoding | Meaning                          |
| ------------: | -----: | -------- | -------------------------------- |
|             0 |      8 | `i64` LE | Camera timestamp in milliseconds |
|     8, 16, 24 | 8 each | `f64` LE | Acceleration X, Y, Z             |
|    32, 40, 48 | 8 each | `f64` LE | Angular velocity X, Y, Z         |

All floating-point values must be finite. The project passes common axis values
through and interprets angular velocity as radians per second. Do not rescale
common values using the raw-sample full-scale ranges. Acceleration normalization
and camera-axis conventions must be established for a profile before using these
values for gravity estimation.

### Packed raw inertial samples

With field `62` true, record `0x03` uses repeated 20-byte samples:

| Sample offset |   Size | Encoding | Meaning                          |
| ------------: | -----: | -------- | -------------------------------- |
|             0 |      8 | `i64` LE | Camera timestamp in microseconds |
|     8, 10, 12 | 2 each | `u16` LE | Acceleration X, Y, Z             |
|    14, 16, 18 | 2 each | `u16` LE | Angular velocity X, Y, Z         |

Axes use offset-binary encoding centered at `32768`. They are not signed 16-bit
two's-complement values. Let `A` be the accelerometer range in g and `G` the
gyro range in degrees per second:

```text
acceleration_g = (raw_accel - 32768) * A / 32768
angular_rate_rad_per_s = (raw_gyro - 32768) * G / 32768 * pi / 180
```

Subtract in a signed or floating-point type. Use field `65` ranges when valid.
The project's missing-range defaults are `A=8` and `G=2000`; those defaults are
implementation choices, not ranges required of every camera.

A typed decoder MUST choose the sample layout from the raw flag and the
identified recording profile. Payload divisibility alone is ambiguous because
some lengths are divisible by both 20 and 56. Reject a payload not divisible by
the selected stride. Secondary gyro record `0x0F` needs its own sensor and
timing context before it can use either interpretation.

### Exposure samples

Records `0x04` and `0x0C` contain repeated 16-byte samples in the flat V3
profile:

| Sample offset | Size | Encoding | Meaning                     |
| ------------: | ---: | -------- | --------------------------- |
|             0 |    8 | `i64` LE | Camera timestamp            |
|             8 |    8 | `f64` LE | Shutter duration in seconds |

Shutter duration must be finite, nonnegative, and representable in the output
time type. Primary and secondary exposure records refer to different image
streams. Do not substitute one for the other solely because they share a stride.

For the project's raw profile, exposure timestamps are microseconds; for its
common profile, they are milliseconds. The shutter value remains seconds in both
profiles. General exposure-to-video mapping can require additional mapping data;
the timestamp/shutter pair alone does not implement field-64 strategy `2`.

### Frame timestamps, GPS, and edited time

Frame-PTS record `0x06` contains consecutive LE `i64` values without per-value
footers. Its payload length must be divisible by eight. A value identifies a
frame's camera time; the applicable millisecond/microsecond mode must be known
before interpreting it. Timelapse and TimeShift can require this mapping instead
of extrapolating capture time from the encoded frame rate.

GPS and reception records are optional. Their logical data can include time,
validity, latitude/longitude, hemispheres, speed, course, altitude, and
reception quality. A complete packed GPS byte schema is outside this
specification. Readers MUST NOT infer that schema from a host-language
structure's field order or padding. Preserve the raw records when not decoded.

Time-map records describe relationships between edited/jump-cut time and
original capture time. Speed records can modify that relationship piecewise.
Their binary entry layouts are opaque here. Processing that depends on those
mappings must supply a supported decoder or reject the operation; it cannot
assume an identity time map after editing.

### Project timestamp normalization

The following formulas describe the current typed decoders, not a universal
clock rule for every INSV record. Let `T` be a sample's camera timestamp, `B`
the first-frame timestamp from field `24`, and `k=1` for raw microseconds or
`k=1000` for common milliseconds. When `B` is absent, the decoders use the first
record sample's timestamp as the base.

```text
exposure_relative_us = k * (T - B)
gyro_relative_us     = k * (T - B) - round(1000 * adjustment_ms)
```

A positive gyro adjustment moves the gyro sample earlier on the video timeline.
The project applies a finite field-28 value when present, independently of the
field-29 boolean. Samples before time zero are omitted. Retained samples must
have strictly increasing timestamps and fit an unsigned 64-bit microsecond
duration. Use wider checked intermediate arithmetic to avoid signed overflow.

Motion decoding requires the raw flag to be present. Exposure decoding currently
uses milliseconds if the flag is absent. The base-time fallback provides a
relative series but cannot establish absolute alignment between independently
based records.

Field `64` values absent, `0`, and `1` use this first-frame-based motion path in
the project. Value `2`, and unknown nonzero strategies, produce an unsupported
capability error. Rolling-shutter value `25`, track presentation times, camera
clock timestamps, frame maps, and edited times remain distinct domains; never
apply one global unit to every integer timestamp in the file.

## File naming, recording sets, and proxies

The conventional filename grammar is:

```text
<prefix>_<YYYYMMDD>_<HHMMSS>_<direction><role>_<sequence>.<extension>
```

| Component     | Interpretation                                                     |
| ------------- | ------------------------------------------------------------------ |
| Prefix        | Media kind, commonly `VID` for main video or `LRV` for a proxy     |
| Date/time     | Capture-name timestamp; not reliable evidence of camera generation |
| Direction `0` | Lens facing away from the screen                                   |
| Direction `1` | Screen/selfie-side lens                                            |
| Role `0`      | Main/original file                                                 |
| Role `1`      | Low-resolution proxy                                               |
| Sequence      | Recording/segment sequence identifier                              |

`_00_` and `_10_` identify the conventional split-file main pair. `_01_` or
`_11_` can identify an associated proxy. A multi-track file may still have
`_00_` in its filename even though both lenses are inside it. Names are
discovery hints; the metadata and actual media structure remain authoritative.

An LRV is a video proxy, not a metadata sidecar and not the second
full-resolution lens input. Its creation is mode-dependent. Applications MUST
tolerate a missing proxy and MUST NOT replace a missing main lens with the proxy
silently.

Long captures may have several continuation segments. Validate the sequence and
available grouping metadata for each segment, and preserve lens pairing within
each segment. Names with matching timestamps alone do not prove membership.
Renaming or moving a file requires maintaining an explicit association if the
conventional sibling-name lookup can no longer find it.

## Processing and preservation requirements

### Required data by operation

| Operation                          | Minimum relevant data                                                                    |
| ---------------------------------- | ---------------------------------------------------------------------------------------- |
| Generic playback                   | Valid media tracks and supported codec                                                   |
| Lens-track extraction              | Track selection and valid media sample tables                                            |
| Two-lens stitching                 | Both lens views, time alignment, track/file order, usable calibration, and optical setup |
| Gyro stabilization                 | Stitching inputs plus decoded inertial samples and a valid video-time mapping            |
| Rolling-shutter correction         | Readout interpretation and synchronized motion beyond frame-level orientation            |
| Timelapse or edited-time telemetry | Applicable frame/source-time mapping and supported time-control records                  |
| Lossless tail preservation         | Original bytes for all recognized and unrecognized tail material                         |

A full-sphere equirectangular output normally has a 2:1 aspect ratio. Decoding a
packed frame or extracting one fisheye track does not produce that panorama.
Stitching projects calibrated lens views into a common sphere, resolves optical
validity and overlap, and blends corresponding imagery. The file structure does
not prescribe one stabilization, seam-blending, or optical-flow algorithm.

### Rewriting and remuxing

A preserving rewrite MUST retain unknown boxes, records, enum values, protobuf
fields, and reserved bytes unless the caller explicitly requests their removal.
Keeping only parsed semantic fields is insufficient for byte-exact preservation.
The recording-set association and primary calibration must also survive.

When changing the media or tail, recalculate every affected length, pointer, and
time mapping. This includes BMFF chunk offsets when sample placement changes, V3
directory offsets relative to the new payload origin, record lengths, ExtraInfo
size, and `inst` size. If trimming or retiming changes camera-to-video
alignment, copying the old tail unchanged does not restore that alignment. A
pure relocation that leaves relative record positions unchanged does not require
changing their relative directory offsets.

Work on a copy when rewriting originals. A generic media remux normally does not
preserve the full tail contract. The project currently provides inspection and
payload reads, not a complete INSV tail-writing API.

### Inspection commands

These commands inspect the media portion without renaming the file:

```sh
ffprobe -v error \
  -show_entries format=format_name,duration,bit_rate \
  -show_entries stream=index,codec_type,codec_name,codec_tag_string,width,height,r_frame_rate,time_base \
  -of json recording.insv
```

For a two-video-track recording, the following extract encoded lens media:

```sh
ffmpeg -i recording.insv -map 0:v:0 -c copy lens-0.mp4
ffmpeg -i recording.insv -map 0:v:1 -c copy lens-1.mp4
```

The outputs are unstitched lens tracks. These commands do not transfer the full
ExtraInfo tail to the resulting MP4 files.

## Validation requirements

A conforming reader for a declared profile MUST:

- Validate all byte ranges with checked arithmetic before reading or allocating.
- Bound every box by its parent, handle ordinary and extended headers, and
  account for size-to-end semantics without swallowing a required terminal box.
- Verify tail signature, version, length, and outer framing together.
- For indexed V3, check both directory-marker bytes, directory divisibility,
  payload origin, record bounds before the directory, matching footers, and
  non-overlapping record ranges.
- For sequential V3, consume the complete record region without partial footers
  or leftover bytes.
- For V2, distinguish absent gyro from a recognized but damaged gyro wrapper.
- Bound record counts, payload sizes, protobuf field counts, recursion depth,
  and sample counts before allocating.
- Decode each metadata field according to its incoming wire type and preserve
  unsupported values without inventing defaults that change interpretation.
- Validate calibration token counts, generation bits, finite values, dimensions,
  and camera-specific lens semantics before rendering.
- Validate each typed telemetry payload's stride, numerical values, units, and
  time ordering before interpolation.
- Reject requested processing that lacks required lens, calibration, timing, or
  supported opaque-record interpretation.

Unknown extensions are different from malformed known structures. An inspection
interface SHOULD distinguish playable media with unsupported or damaged
ExtraInfo from a completely invalid media container. A processor must also
separate readable metadata from a recording that it can actually stitch.

## Current project implementation support

This section records the present behavior of `insta360-rs`; it does not weaken
the format requirements above. Structural inspection, typed payload decoding,
and end-to-end media export are separate support levels.

| Capability                                     | Current support                                                                                               |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Input discovery                                | One or two existing files; conventional `_00_`/`_10_` pairing; primary ordered first                          |
| Camera identification                          | ONE X, ONE X2, X3, X4, X5, X6 and registered aliases; unknown names retained                                  |
| V3 indexed inspection                          | Supported within parser limits, including empty directories and record regions                                |
| V3 sequential inspection                       | Backward-chain parsing supported; empty/header-only tails are not a supported input                           |
| Genuine V2 tail                                | Extraction preserves JSON metadata and gyro envelope; bounded `InsvReader` inspection is not implemented      |
| JSON metadata                                  | Readable JSON and raw bytes during extraction; bounded inspector remains protobuf-only                        |
| Protobuf metadata                              | Typed subset plus top-level unknown values; some known encodings remain undecoded                             |
| Media-header inspection                        | Video handlers, first visual sample description, track duration, average frame timing                         |
| Complete BMFF demuxing                         | Delegated to the media decoding path; the bounded inspector is not a sample-table validator                   |
| Calibration parsing                            | Two-lens V1, V2, V3, V6                                                                                       |
| CPU/GPU projection                             | V2, V3, V6 subject to registered lens geometry; V1 parse-only                                                 |
| Accessory conversion                           | X5 V6 with required embedded polynomial profiles and supported target setup                                   |
| Primary gyro                                   | Common and packed raw V3 samples; explicit raw flag required                                                  |
| Exposure                                       | Flat V3 primary and secondary payload decoders                                                                |
| Frame PTS, GPS, secondary IMU, other telemetry | Raw record access; no complete typed processing path                                                          |
| Calibrated panorama image/video export         | X5 single-file recordings with exactly two video tracks; other stitched input layouts are not implemented     |
| Stabilization                                  | X5 compact-raw normalization, gravity fusion, exposure/PTS alignment, and supported sensor readout correction |
| Audio in video export                          | Optional AAC/ALAC packet copy preserving video-relative timing, with complete-packet cuts                     |
| Tail rewriting                                 | No complete preserving writer                                                                                 |

Registered camera/lens geometry does not establish that every mode of that
camera can pass the media export pipeline. In particular, X6 10-bit format
capability must not be read as a claim of current X6 end-to-end export support.

### Component extraction and direct stream access

With the `media` feature, the `extract` API and CLI preserve every demuxed
stream as original encoded packets with timing, codec configuration, side data,
and metadata. Video/audio also receive playable container copies where
supported. These copies use remuxing without re-encoding. Non-media boxes,
complete raw ExtraInfo tails, repeated and unknown records, thumbnails, and
calibration material are extracted into indexed files with a JSON manifest.
Unused bytes inside `mdat` are not a component of the extraction.

The extraction parser independently validates V2 and V3 framing, including
sequential and empty indexed V3 tails. Malformed or unsupported interpretations
are reported while bounded raw bytes are preserved. The bounded `InsvReader`
limitations below still apply to that separate inspection API.

`MediaSource` and `MediaStream` provide file-backed stream selection, encoded
packet readers, and seekable video-frame decoding without creating intermediate
files. They do not require camera-specific calibration. Packet access preserves
encoded bit depth; decoded RGB24 frames are explicitly an 8-bit unstitched
representation. Direct stream readers and component extraction do not perform
calibrated panorama rendering.

### Parser limits

These are resource limits of the current implementation, not maximum sizes of
the INSV format:

| Resource                                     |                                       Limit |
| -------------------------------------------- | ------------------------------------------: |
| Boxes per parsed level                       |                                       4,096 |
| Directory payload                            |                                       1 MiB |
| Nonzero indexed records / sequential records |                                         256 |
| Metadata payload                             |                                       8 MiB |
| Individual payload read                      |             Caller limit, capped at 512 MiB |
| Protobuf fields per parsed message           |                                      65,536 |
| `stts` entries used for average frame rate   | 4,096; larger tables suppress computed rate |
| Calibration string                           |                                65,536 bytes |
| Typed optical-profile descriptor             |                                 4,096 bytes |
| Motion samples                               |                                   5,000,000 |
| Exposure samples                             |                                  10,000,000 |
| Gyro payload loaded by media export          |                                     128 MiB |

### Conformance gaps and maintenance boundaries

The bounded reader currently requires `ftyp` first and the terminal tail to
match a top-level `inst` box. It does not implement V2 dispatch or independent
media-only success when tail inspection fails. Actual inspected video-track
count supersedes the count inferred from metadata.

The typed reader requires version 3 and protobuf metadata format 1; unsupported
versions or metadata encodings return `MissingCapability`. Indexed records and
their footers must end before the directory payload, with no overlapping or
duplicate ranges. Both directory-marker bytes are checked, empty tails and
directories are supported, and sequential parsing must consume the complete
record region. Signed first-frame timestamps preserve protobuf `int64` values.
These checks use bounded descriptors and reads, without scanning media payloads.
Tests include nested-container mutations and independently constructed malformed
directory and sequential-record layouts.

The current reader also has the following limitations relevant to maintenance:

- Ordinary inspection reads the first sample description per video track. The
  explicit presentation-timestamp reader validates bounded `stts`/`ctts` tables
  and simple edit origins; full chunk/fragment demuxing remains FFmpeg's job.
- Returned records are sorted by ID. Parsed unknown protobuf values retain
  content but do not preserve original varint encoding, wrapper bytes, or every
  nested unknown field. Inspection results cannot reconstruct the original tail
  byte-for-byte.
- The media pipeline normalizes X5 primary compact-raw IMU and uses primary/
  secondary exposures with actual BMFF presentation timestamps for tag-64
  mode 2. It applies gravity fusion and supported sensor readout correction.
  Arbitrary edited-time maps, secondary IMU fusion, and other camera mounting
  profiles remain unsupported. See [stabilization](stabilization.md) for
  implementation choices, static evidence, and physical limits.

Known structures with missing typed support remain valid format extensions or
implementation gaps. They must not be relabeled as malformed solely because the
current processing pipeline cannot consume them.
