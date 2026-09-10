//! Bounded readers for the ISO-BMFF portion and proprietary trailer of INSV files.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::error::io_error;
use crate::profile::camera_profile_for_name;
use crate::{CameraModel, Error, MediaInfo, Result, TrailerInfo, VideoTrackInfo};

const INSTA360_MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";
const TRAILER_HEADER_SIZE: u64 = 72;
const RECORD_FOOTER_SIZE: u64 = 6;
const OFFSET_ENTRY_SIZE: usize = 10;
const MAX_TOP_LEVEL_BOXES: usize = 4_096;
const MAX_INDEX_SIZE: u64 = 1024 * 1024;
const MAX_INDEX_RECORDS: usize = 256;
const MAX_METADATA_SIZE: u64 = 8 * 1024 * 1024;
const MAX_RECORD_READ_SIZE: u64 = 512 * 1024 * 1024;
const MAX_STTS_ENTRIES: u32 = 4_096;
const MAX_TIMING_SAMPLES: usize = 5_000_000;
const MAX_PROTOBUF_FIELDS: usize = 65_536;
const MAX_PROTOBUF_FIELD_NUMBER: u64 = (1 << 29) - 1;

struct TimingEdit {
    media_start: i64,
    empty_duration: u64,
    duration: Option<u64>,
    movie_timescale: u32,
}

fn unique_timing_box<'a>(boxes: &'a [BoxHeader], kind: &[u8; 4]) -> Result<&'a BoxHeader> {
    let mut matching = boxes.iter().filter(|item| &item.kind == kind);
    let header = matching.next().ok_or_else(|| {
        Error::InvalidMedia(format!(
            "missing `{}` box for timestamp mapping",
            String::from_utf8_lossy(kind)
        ))
    })?;
    if matching.next().is_some() {
        return Err(Error::InvalidMedia(format!(
            "duplicate `{}` boxes make timestamp mapping ambiguous",
            String::from_utf8_lossy(kind)
        )));
    }
    Ok(header)
}

fn round_timing_ratio(numerator: i128, denominator: i128) -> i128 {
    if numerator < 0 {
        -((-numerator + denominator / 2) / denominator)
    } else {
        (numerator + denominator / 2) / denominator
    }
}

/// A validated set of one or two files belonging to one Insta360 recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSet {
    paths: Vec<PathBuf>,
}

impl InputSet {
    /// Creates an input set, validating file existence and paired-file naming.
    pub fn new(paths: Vec<PathBuf>) -> Result<Self> {
        if paths.is_empty() || paths.len() > 2 {
            return Err(Error::InvalidMedia(
                "an Insta360 input set must contain one or two files".into(),
            ));
        }

        for path in &paths {
            validate_input_path(path)?;
        }

        if paths.len() == 1 {
            return Ok(Self { paths });
        }

        let first = pair_identity(&paths[0]);
        let second = pair_identity(&paths[1]);
        let (first_kind, first_identity) = first.ok_or_else(|| {
            Error::InvalidMedia("paired inputs must contain `_00_` and `_10_` markers".into())
        })?;
        let (second_kind, second_identity) = second.ok_or_else(|| {
            Error::InvalidMedia("paired inputs must contain `_00_` and `_10_` markers".into())
        })?;

        if first_identity != second_identity || first_kind == second_kind {
            return Err(Error::InvalidMedia(
                "paired inputs do not belong to the same `_00_`/`_10_` recording".into(),
            ));
        }

        let mut ordered = paths;
        if first_kind == PairKind::Secondary {
            ordered.swap(0, 1);
        }
        Ok(Self { paths: ordered })
    }

    /// Discovers the primary file when given a legacy secondary file and includes
    /// a legacy secondary file when one exists beside the primary.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        validate_input_path(&path)?;

        match pair_identity(&path) {
            Some((PairKind::Secondary, _)) => {
                let primary = replace_pair_marker(&path, "_10_", "_00_").ok_or_else(|| {
                    Error::InvalidMedia("could not derive the primary `_00_` filename".into())
                })?;
                if !primary.is_file() {
                    return Err(Error::InvalidMedia(format!(
                        "legacy secondary input requires primary file {}",
                        primary.display()
                    )));
                }
                Self::new(vec![primary, path])
            }
            Some((PairKind::Primary, _)) => {
                let secondary = replace_pair_marker(&path, "_00_", "_10_").ok_or_else(|| {
                    Error::InvalidMedia("could not derive the secondary `_10_` filename".into())
                })?;
                if secondary.is_file() {
                    Self::new(vec![path, secondary])
                } else {
                    Self::new(vec![path])
                }
            }
            None => Self::new(vec![path]),
        }
    }

    /// Returns the paths in processing order. For legacy pairs, `_00_` is first.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

/// An ISO-BMFF top-level box discovered without reading its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoxInfo {
    /// Four-character box type.
    pub kind: [u8; 4],
    /// Absolute byte offset of the box header.
    pub offset: u64,
    /// Total box size including its header.
    pub size: u64,
}

/// A proprietary INSV record described by the trailer index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordInfo {
    /// Insta360 record identifier.
    pub id: u8,
    /// Record payload encoding identifier.
    pub format: u8,
    /// Absolute byte offset of the record payload.
    pub offset: u64,
    /// Record payload size.
    pub size: u64,
}

/// A factory calibration string embedded in an INSV recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EmbeddedOffset {
    /// Insta360 offset schema generation.
    pub version: u8,
    /// Whether this is the unmodified factory copy of the offset.
    pub original: bool,
    /// Opaque offset value. Geometry modules may parse supported generations.
    pub value: String,
}

/// An embedded optical-profile descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EmbeddedProfile {
    /// Profile name used by Insta360 media and vendor libraries.
    pub name: String,
    /// Original protobuf submessage for forward-compatible interpretation.
    pub payload: Vec<u8>,
}

/// Full-scale ranges used to decode the compact raw IMU record.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ImuRange {
    /// Accelerometer full scale in multiples of standard gravity.
    pub accelerometer_g: f64,
    /// Gyroscope full scale in degrees per second.
    pub gyroscope_degrees_per_second: f64,
}

macro_rules! protobuf_enum {
    ($(#[$attribute:meta])* pub enum $name:ident { $($variant:ident = $value:literal),+ $(,)? }) => {
        $(#[$attribute])*
        #[non_exhaustive]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
        pub enum $name {
            $($variant,)+
            /// A value introduced by newer firmware or not described by this metadata schema.
            Other(u32),
        }

        impl $name {
            /// Converts the integer stored by the camera into a forward-compatible value.
            pub fn from_raw(value: u32) -> Self {
                match value {
                    $($value => Self::$variant,)+
                    other => Self::Other(other),
                }
            }

            /// Returns the exact integer representation used in protobuf metadata.
            pub fn raw_value(self) -> u32 {
                match self {
                    $(Self::$variant => $value,)+
                    Self::Other(value) => value,
                }
            }
        }
    };
}

protobuf_enum! {
    /// Projection/category recorded in `INSPBExtraMetadata.fileCategory` (tag 129).
    pub enum FileCategory {
        Unknown = 0,
        Plane = 1,
        DoubleFisheyePanorama = 2,
        Fisheye = 3,
        WideAngle = 4,
        DoubleHalfFisheyePanorama = 5,
        EquirectangularPanorama = 6,
    }
}

protobuf_enum! {
    /// Encoded image rotation recorded in `INSPBExtraMetadata.fileRotate` (tag 130).
    pub enum FileRotation {
        Unknown = 0,
        Degrees0 = 1,
        Degrees90 = 2,
        Degrees180 = 3,
        Degrees270 = 4,
    }
}

protobuf_enum! {
    /// Physical stream/file layout recorded in `INSPBExtraMetadata.streamType` (tag 131).
    pub enum StreamType {
        Unknown = 0,
        SingleStreamFile = 1,
        DualStreamFiles = 2,
        DualStreamTracks = 3,
        DualStreamTracksReversed = 4,
    }
}

protobuf_enum! {
    /// Video codec recorded in `INSPBExtraMetadata.codecType` (tag 132).
    pub enum CodecType {
        Unknown = 1,
        H264 = 2,
        H265 = 3,
        Mjpeg = 4,
    }
}

protobuf_enum! {
    /// Offset generation selected when the recording was captured (tag 136).
    pub enum CaptureOffsetVersion {
        Unknown = 0,
        V1 = 1,
        V2 = 2,
        V3 = 3,
        V6 = 4,
    }
}

protobuf_enum! {
    /// Capture color mode in `ExtraMetadata.shooting_param_info.color_mode` (212/8).
    ///
    /// This enum differs from the separate `ExtraRecipe.color_mode` numbering.
    /// Legacy `gamma_mode = "log"` does not identify this I-Log transfer curve.
    pub enum RecordedColorMode {
        Unknown = 0,
        Standard = 1,
        ILog = 2,
        Dolby = 3,
    }
}

protobuf_enum! {
    /// Firmware-selected gyro/video timestamp mapping strategy (tag 64).
    pub enum VideoPtsMapType {
        Unknown = 0,
        DecoderWithFirstFrameTimestamp = 1,
        ReadingInExposureFile = 2,
    }
}

protobuf_enum! {
    /// Lens housing/filter selection recorded in `offsetStates` (tag 68).
    pub enum OffsetState {
        Common = 0,
        SphereProtector = 1,
        DiveCaseUnderwater = 2,
        DiveCase2023Underwater = 3,
        X4PlasticLensGuard = 4,
        X4GlassLensGuard = 5,
        Automatic = 6,
        Nd16 = 7,
        Nd32 = 8,
        Nd64 = 9,
        DiveCaseProUnderwater = 10,
        DiveCaseProAboveWater = 11,
        Nd128 = 12,
    }
}

protobuf_enum! {
    /// Automatic lens-guard detection result used with [`OffsetState::Automatic`] (tag 104).
    pub enum GuardDetectedType {
        Unknown = 0,
        Plastic = 1,
        Glass = 2,
        Off = 3,
        AveragePlasticGlass = 4,
        Nd16 = 5,
        Nd32 = 6,
        Nd64 = 7,
        Nd128 = 8,
    }
}

protobuf_enum! {
    /// Panorama recording organization recorded in tag 79.
    pub enum PanoRecordType {
        Unknown = 0,
        SplitFile = 1,
        MultiTrack = 2,
    }
}

protobuf_enum! {
    /// Meaning of track zero for a multi-track panorama recording (tag 80).
    pub enum PanoRecordTrackOrder {
        Unknown = 0,
        Track0IsStream10 = 1,
        Track0IsStream00 = 2,
    }
}

protobuf_enum! {
    /// Lens accessory recorded by recent cameras in tag 186.
    pub enum LensAccessoryType {
        Unknown = 0,
        Standard = 1,
        BlackMistFilter = 2,
        WideAngleLens = 3,
        AdjustableMacroLens = 4,
        AnamorphicLens = 5,
        StarFilter = 6,
        NdFilter = 7,
        MicroLens = 8,
        MicroLens5Cm = 9,
        MicroLens11_6Cm = 10,
        MicroLens21_5Cm = 11,
        MicroLens38Cm = 12,
    }
}

/// A validated sensor-to-encoded-frame crop from the nested tag-27 message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CropWindow {
    /// Sensor/source width before the firmware crop.
    pub source_width: u32,
    /// Sensor/source height before the firmware crop.
    pub source_height: u32,
    /// Encoded destination width.
    pub destination_width: u32,
    /// Encoded destination height.
    pub destination_height: u32,
    /// Horizontal crop offset; firmware may encode a negative value.
    pub x_offset: i32,
    /// Vertical crop offset; firmware may encode a negative value.
    pub y_offset: i32,
    /// Unrecognized fields retained from the nested crop message.
    pub unknown_fields: Vec<UnknownMetadataField>,
}

/// An unrecognized protobuf value retained without interpreting its schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum UnknownMetadataValue {
    /// Wire type 0.
    Varint(u64),
    /// Wire type 1, stored as its exact little-endian bits.
    Fixed64(u64),
    /// Wire type 2, stored without its key and length prefix.
    LengthDelimited(Vec<u8>),
    /// Wire type 5, stored as its exact little-endian bits.
    Fixed32(u32),
}

/// A protobuf field not understood by this version of the metadata parser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnknownMetadataField {
    /// Protobuf field number.
    pub number: u32,
    /// Exact wire value, suitable for later schema-aware interpretation.
    pub value: UnknownMetadataValue,
}

/// Camera recording identity and raw submedia association, including previews.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RecordingGroup {
    /// Raw capture subtype, independent of physical lens organization.
    pub capture_type: u32,
    /// Raw submedia ordering value; preview files may occupy intervening positions.
    pub index: u32,
    /// Camera-provided recording identity; empty means unavailable.
    pub identity: String,
    /// Declared submedia count, not an original-chapter count; zero is unspecified.
    pub total: u32,
}

/// Temporal splitting flag, distinct from panorama lens/file organization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum FileSplitType {
    Unknown,
    NotSplit,
    Split,
    Other(u32),
}

/// Metadata required by calibration and motion processing.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct InsvMetadata {
    /// Recording identity and raw submedia position from protobuf field 26.
    pub recording_group: Option<RecordingGroup>,
    /// Temporal split declaration from protobuf field 88.
    pub file_split_type: Option<FileSplitType>,
    /// Group or split declarations were malformed or contradictory.
    pub sequence_metadata_invalid: bool,
    /// Camera serial number.
    pub serial: Option<String>,
    /// Camera model string written by the firmware.
    pub camera_name: Option<String>,
    /// Camera firmware version.
    pub firmware: Option<String>,
    /// Raw gamma label (tag 22). Studio identifies I-Log by the exact `"I_Log"`
    /// value; legacy `"log"` alone does not identify that transfer curve.
    /// Conflicting duplicates or invalid values leave this unset.
    pub gamma_mode: Option<String>,
    /// Explicit capture color mode (tag 212, nested tag 8), when unambiguous.
    /// Missing, malformed, and conflicting declarations leave this unset;
    /// unknown enum integers remain [`RecordedColorMode::Other`]. Original
    /// gamma and shooting-parameter fields are retained in [`Self::unknown_fields`].
    pub recorded_color_mode: Option<RecordedColorMode>,
    /// Capture color metadata was present but malformed, used the wrong wire
    /// type, overflowed its enum, or contained conflicting declarations.
    /// Consumers must not treat this as an absent mode and fall back to gamma
    /// metadata for a color conversion. Valid empty messages leave this false.
    pub recorded_color_mode_invalid: bool,
    /// Embedded base and original calibration offsets.
    pub offsets: Vec<EmbeddedOffset>,
    /// Embedded optical/accessory profiles.
    pub profiles: Vec<EmbeddedProfile>,
    /// Whether the gyro record uses the compact raw 20-byte sample layout.
    pub is_raw_gyro: Option<bool>,
    /// Firmware-selected gyro/video timestamp mapping strategy (tag 64).
    pub video_pts_map_type: Option<VideoPtsMapType>,
    /// Camera-system timestamp of the first video frame. Raw-gyro recordings
    /// store microseconds; legacy recordings store milliseconds.
    pub first_frame_timestamp: Option<i64>,
    /// Firmware-provided gyro/video alignment correction in milliseconds.
    pub gyro_timestamp_adjust_ms: Option<f64>,
    /// Dynamic IMU ranges used by compact raw records. When absent, the
    /// documented firmware defaults are 8 g and 2000 degrees/second.
    pub imu_range: Option<ImuRange>,
    /// Number of gyro samples when the record layout is known exactly.
    pub gyro_sample_count: u64,
    /// Number of primary and secondary exposure samples.
    pub exposure_sample_count: u64,
    /// Integer nominal frame rate from metadata, when present.
    pub nominal_frame_rate: Option<u32>,
    /// Sensor-to-encoded-frame crop. Invalid zero-sized crop messages are retained
    /// in [`Self::unknown_fields`] instead of being applied.
    pub crop_window: Option<CropWindow>,
    /// Rolling-shutter readout value recorded by the firmware (tag 25).
    pub rolling_shutter_time: Option<f64>,
    /// Whether the recording declares a gyro timestamp adjustment (tag 29).
    pub has_gyro_timestamp_adjust: Option<bool>,
    /// Legacy timelapse frame interval from tag 30, in seconds unless
    /// [`Self::timelapse_interval_is_milliseconds`] is true.
    pub timelapse_interval: Option<f64>,
    /// Firmware marker indicating that the legacy timelapse interval uses
    /// milliseconds instead of seconds (tag 59).
    pub timelapse_interval_is_milliseconds: Option<bool>,
    /// Direct millisecond timelapse interval used by recent cameras (tag 133).
    pub timelapse_interval_ms: Option<u64>,
    /// Milliseconds to remove from playback duration to align unequal streams (tag 127).
    pub file_duration_tailor_time_ms: Option<i32>,
    /// Lens blend angle recorded at capture time (tag 128).
    pub blend_angle: Option<i32>,
    /// Recorded projection/category (tag 129).
    pub file_category: Option<FileCategory>,
    /// Recorded frame rotation (tag 130).
    pub file_rotation: Option<FileRotation>,
    /// Recorded physical stream/file layout (tag 131).
    pub stream_type: Option<StreamType>,
    /// Recorded video codec (tag 132).
    pub codec_type: Option<CodecType>,
    /// Offset schema selected at capture time (tag 136).
    pub capture_offset_version: Option<CaptureOffsetVersion>,
    /// Lens housing/filter selection (tag 68).
    pub offset_state: Option<OffsetState>,
    /// Automatic lens-guard detection result (tag 104).
    pub guard_detected_type: Option<GuardDetectedType>,
    /// Panorama recording organization (tag 79).
    pub pano_record_type: Option<PanoRecordType>,
    /// Multi-track stream order (tag 80).
    pub pano_record_track_order: Option<PanoRecordTrackOrder>,
    /// Actual track count after full inspection, or a count inferred from the
    /// declared stream layout when only trailer metadata has been parsed.
    pub video_track_count: Option<u32>,
    /// Whether track zero represents stream `_10_`, when declared by metadata.
    pub reverse_video_track_order: Option<bool>,
    /// Expected encoded bitrate in bits per second (tag 135).
    pub expected_bitrate: Option<u64>,
    /// Whether this file was produced by pre-record mode (tag 134).
    pub is_pre_recorded: Option<bool>,
    /// Lens accessory selected for cameras that support interchangeable optics (tag 186).
    pub lens_accessory_type: Option<LensAccessoryType>,
    /// X5 ND128 seam-correction fallback flag (tag 193).
    pub p3_fake_on: Option<bool>,
    /// Top-level protobuf fields not understood, partially inspected, or not safely decoded.
    pub unknown_fields: Vec<UnknownMetadataField>,
}

/// Structural and metadata inspection result for one INSV file.
#[derive(Clone, Debug, PartialEq)]
pub struct InsvInspection {
    /// Top-level ISO-BMFF boxes.
    pub boxes: Vec<BoxInfo>,
    /// Proprietary trailer location and version.
    pub trailer: TrailerInfo,
    /// Indexed proprietary records.
    pub records: Vec<RecordInfo>,
    /// Parsed metadata and opaque calibration values.
    pub metadata: InsvMetadata,
    /// Video tracks found in the ISO-BMFF movie box.
    pub video_tracks: Vec<VideoTrackInfo>,
    /// Longest video-track duration.
    pub duration: Option<Duration>,
    /// Frame rate derived from sample timing.
    pub fps: Option<f64>,
}

/// A bounded `Read + Seek` INSV parser.
pub struct InsvReader<R> {
    inner: R,
    file_size: u64,
}

impl<R: Read + Seek> InsvReader<R> {
    /// Creates a reader and records its seekable length.
    pub fn new(mut inner: R) -> Result<Self> {
        let file_size = inner
            .seek(SeekFrom::End(0))
            .map_err(|error| invalid_io("determining input size", error))?;
        inner
            .seek(SeekFrom::Start(0))
            .map_err(|error| invalid_io("rewinding input", error))?;
        Ok(Self { inner, file_size })
    }

    /// Parses the container, trailer index, metadata, and video-track headers.
    pub fn inspect(&mut self) -> Result<InsvInspection> {
        let boxes = self.scan_top_level_boxes()?;
        if boxes.first().map(|item| item.kind) != Some(*b"ftyp") {
            return Err(Error::InvalidMedia(
                "INSV input does not begin with an ISO-BMFF `ftyp` box".into(),
            ));
        }

        let (trailer, records) = self.read_trailer_index()?;
        if !boxes.iter().any(|item| {
            item.kind == *b"inst" && item.offset == trailer.offset && item.size == trailer.size
        }) {
            return Err(Error::InvalidMedia(
                "proprietary trailer is not a valid top-level `inst` box".into(),
            ));
        }

        let mut metadata = self.read_metadata(&records)?;
        populate_sample_counts(&mut metadata, &records);
        let movie = self.read_movie_info(&boxes)?;
        if !movie.video_tracks.is_empty() {
            metadata.video_track_count = u32::try_from(movie.video_tracks.len()).ok();
        }
        let fps = movie.fps.or(metadata.nominal_frame_rate.map(f64::from));

        Ok(InsvInspection {
            boxes,
            trailer,
            records,
            metadata,
            video_tracks: movie.video_tracks,
            duration: movie.duration,
            fps,
        })
    }

    /// Returns the proprietary trailer descriptor.
    pub fn trailer_info(&mut self) -> Result<TrailerInfo> {
        self.read_trailer_index().map(|(trailer, _)| trailer)
    }

    /// Returns indexed proprietary record descriptors without loading payloads.
    pub fn records(&mut self) -> Result<Vec<RecordInfo>> {
        self.read_trailer_index().map(|(_, records)| records)
    }

    /// Returns parsed metadata and embedded calibration material.
    pub fn metadata(&mut self) -> Result<InsvMetadata> {
        let (_, records) = self.read_trailer_index()?;
        let mut metadata = self.read_metadata(&records)?;
        populate_sample_counts(&mut metadata, &records);
        Ok(metadata)
    }

    /// Reads actual presentation timestamps for each video track, in microseconds.
    ///
    /// Track order matches [`InsvInspection::video_tracks`]. Sample durations and
    /// composition offsets are read from `moov`; compressed media is not read.
    /// A single rate-one edit, optionally preceded by an empty edit, is supported
    /// when it preserves every sample. Trims require an original-frame ordinal
    /// mapper to keep camera exposures aligned and are rejected.
    /// Fragmented movies and other edit layouts require a separate time mapper.
    /// At most five million samples across all video tracks are expanded.
    pub fn video_presentation_timestamps(&mut self) -> Result<Vec<Vec<i64>>> {
        let boxes = self.scan_top_level_boxes()?;
        if boxes.iter().any(|item| item.kind == *b"moof") {
            return Err(Error::MissingCapability(
                "fragmented video timestamp mapping is unsupported".into(),
            ));
        }
        let mut movies = boxes.iter().filter(|item| item.kind == *b"moov");
        let movie = movies
            .next()
            .ok_or_else(|| Error::InvalidMedia("missing movie box for timestamp mapping".into()))?;
        if movies.next().is_some() {
            return Err(Error::InvalidMedia(
                "multiple movie boxes make timestamp mapping ambiguous".into(),
            ));
        }
        let header = self.read_box_header(movie.offset, movie.offset + movie.size)?;
        let children = self.child_boxes(header.payload_offset(), header.end())?;
        if children.iter().any(|item| item.kind == *b"mvex") {
            return Err(Error::MissingCapability(
                "fragmented video timestamp mapping is unsupported".into(),
            ));
        }
        let movie_timescale = children
            .iter()
            .find(|item| item.kind == *b"mvhd")
            .map(|header| self.read_media_header(header))
            .transpose()?
            .flatten()
            .map(|timing| timing.0);
        let mut result = Vec::new();
        let mut remaining = MAX_TIMING_SAMPLES;
        for track in children.iter().filter(|item| item.kind == *b"trak") {
            if let Some((timestamps, sample_count)) =
                self.read_track_presentation_timestamps(track, movie_timescale, remaining)?
            {
                remaining -= sample_count;
                result.push(timestamps);
            }
        }
        if result.is_empty() {
            return Err(Error::InvalidMedia(
                "no video tracks for timestamp mapping".into(),
            ));
        }
        Ok(result)
    }

    fn read_track_presentation_timestamps(
        &mut self,
        track: &BoxHeader,
        movie_timescale: Option<u32>,
        maximum_samples: usize,
    ) -> Result<Option<(Vec<i64>, usize)>> {
        let children = self.child_boxes(track.payload_offset(), track.end())?;
        let mdia = unique_timing_box(&children, b"mdia")?;
        let media_children = self.child_boxes(mdia.payload_offset(), mdia.end())?;
        if self.read_handler(unique_timing_box(&media_children, b"hdlr")?)? != *b"vide" {
            return Ok(None);
        }
        let (timescale, _) = self
            .read_media_header(unique_timing_box(&media_children, b"mdhd")?)?
            .ok_or_else(|| Error::InvalidMedia("unsupported or zero video timescale".into()))?;
        let minf = unique_timing_box(&media_children, b"minf")?;
        let minf_children = self.child_boxes(minf.payload_offset(), minf.end())?;
        let table = unique_timing_box(&minf_children, b"stbl")?;
        let tables = self.child_boxes(table.payload_offset(), table.end())?;
        let sample_count = self.read_timing_sample_count(&tables)?;
        if sample_count == 0 || sample_count > maximum_samples {
            return Err(Error::InvalidMedia(
                "video timestamp sample count is empty or exceeds the parser limit".into(),
            ));
        }
        let stts = self.read_timing_runs(unique_timing_box(&tables, b"stts")?)?;
        if stts[0] != 0 {
            return Err(Error::MissingCapability("unsupported stts version".into()));
        }
        let mut timestamps = Vec::with_capacity(sample_count);
        let mut decode_time = 0_i64;
        for run in stts[8..].chunks_exact(8) {
            let count = be_u32(&run[..4]) as usize;
            let delta = i64::from(be_u32(&run[4..]));
            if count > sample_count - timestamps.len() || count == 0 || delta == 0 {
                return Err(Error::InvalidMedia("invalid stts count or duration".into()));
            }
            for _ in 0..count {
                timestamps.push(decode_time);
                decode_time = decode_time
                    .checked_add(delta)
                    .ok_or_else(|| Error::InvalidMedia("video decode time overflows".into()))?;
            }
        }
        if timestamps.len() != sample_count {
            return Err(Error::InvalidMedia(
                "stts and sample-size counts differ".into(),
            ));
        }
        if tables.iter().any(|item| item.kind == *b"ctts") {
            let ctts = self.read_timing_runs(unique_timing_box(&tables, b"ctts")?)?;
            if ctts[0] > 1 {
                return Err(Error::MissingCapability("unsupported ctts version".into()));
            }
            let mut next = 0;
            for run in ctts[8..].chunks_exact(8) {
                let count = be_u32(&run[..4]) as usize;
                let offset = if ctts[0] == 1 {
                    i64::from(i32::from_be_bytes(
                        run[4..8].try_into().expect("offset width"),
                    ))
                } else {
                    i64::from(be_u32(&run[4..]))
                };
                if count == 0 || count > sample_count - next {
                    return Err(Error::InvalidMedia(
                        "ctts and sample-size counts differ".into(),
                    ));
                }
                for timestamp in &mut timestamps[next..next + count] {
                    *timestamp = timestamp.checked_add(offset).ok_or_else(|| {
                        Error::InvalidMedia("video presentation time overflows".into())
                    })?;
                }
                next += count;
            }
            if next != sample_count {
                return Err(Error::InvalidMedia(
                    "ctts and sample-size counts differ".into(),
                ));
            }
        }
        let edit = self.read_timing_edit(&children, movie_timescale)?;
        let mut result = Vec::with_capacity(sample_count);
        for timestamp in timestamps {
            let relative = i128::from(timestamp) - i128::from(edit.media_start);
            if edit.duration.is_some_and(|duration| {
                relative < 0
                    || relative * i128::from(edit.movie_timescale)
                        >= i128::from(duration) * i128::from(timescale)
            }) {
                return Err(Error::MissingCapability(
                    "video edits that discard samples require an original-frame exposure mapper"
                        .into(),
                ));
            }
            let numerator = relative * i128::from(edit.movie_timescale)
                + i128::from(edit.empty_duration) * i128::from(timescale);
            let denominator = i128::from(timescale) * i128::from(edit.movie_timescale);
            let micros = round_timing_ratio(numerator * 1_000_000, denominator);
            result.push(i64::try_from(micros).map_err(|_| {
                Error::InvalidMedia("video presentation microseconds overflow".into())
            })?);
        }
        result.sort_unstable();
        if result.is_empty() || result.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Error::InvalidMedia(
                "video presentation timestamps are empty or not unique at microsecond precision"
                    .into(),
            ));
        }
        Ok(Some((result, sample_count)))
    }

    fn read_timing_runs(&mut self, header: &BoxHeader) -> Result<Vec<u8>> {
        if header.payload_size() < 8 {
            return Err(Error::InvalidMedia("truncated video timing table".into()));
        }
        let prefix = self.read_exact_at(header.payload_offset(), 8)?;
        let count = be_u32(&prefix[4..8]) as usize;
        let length = 8 + count as u64 * 8;
        if prefix[1..4] != [0; 3] || count > MAX_TIMING_SAMPLES || length != header.payload_size() {
            return Err(Error::InvalidMedia(
                "invalid or oversized video timing table".into(),
            ));
        }
        self.read_exact_at(header.payload_offset(), length as usize)
    }

    fn read_timing_sample_count(&mut self, tables: &[BoxHeader]) -> Result<usize> {
        let mut sizes = tables
            .iter()
            .filter(|item| item.kind == *b"stsz" || item.kind == *b"stz2");
        let header = sizes
            .next()
            .ok_or_else(|| Error::InvalidMedia("missing video sample-size table".into()))?;
        if sizes.next().is_some() || header.payload_size() < 12 {
            return Err(Error::InvalidMedia(
                "ambiguous or truncated video sample-size table".into(),
            ));
        }
        let prefix = self.read_exact_at(header.payload_offset(), 12)?;
        if prefix[..4] != [0; 4] {
            return Err(Error::MissingCapability(
                "unsupported sample-size table version or flags".into(),
            ));
        }
        let count = be_u32(&prefix[8..12]);
        let bytes = if header.kind == *b"stsz" {
            if be_u32(&prefix[4..8]) == 0 {
                u64::from(count) * 4
            } else {
                0
            }
        } else {
            let bits = prefix[7];
            if !matches!(bits, 4 | 8 | 16) {
                return Err(Error::InvalidMedia(
                    "invalid compact sample-size field width".into(),
                ));
            }
            (u64::from(count) * u64::from(bits)).div_ceil(8)
        };
        if header.payload_size() != 12 + bytes {
            return Err(Error::InvalidMedia(
                "truncated video sample-size entries".into(),
            ));
        }
        Ok(count as usize)
    }

    fn read_timing_edit(
        &mut self,
        children: &[BoxHeader],
        movie_timescale: Option<u32>,
    ) -> Result<TimingEdit> {
        if !children.iter().any(|item| item.kind == *b"edts") {
            return Ok(TimingEdit {
                media_start: 0,
                empty_duration: 0,
                duration: None,
                movie_timescale: 1,
            });
        }
        let edts = unique_timing_box(children, b"edts")?;
        let edits = self.child_boxes(edts.payload_offset(), edts.end())?;
        let elst = unique_timing_box(&edits, b"elst")?;
        if elst.payload_size() < 8 {
            return Err(Error::InvalidMedia("truncated video edit list".into()));
        }
        let prefix = self.read_exact_at(elst.payload_offset(), 8)?;
        let count = be_u32(&prefix[4..8]);
        if prefix[0] > 1 || prefix[1..4] != [0; 3] || count == 0 || count > 2 {
            return Err(Error::MissingCapability("video timestamp mapping supports one rate-one edit with an optional leading empty edit".into()));
        }
        let entry_size = if prefix[0] == 1 { 20 } else { 12 };
        if elst.payload_size() != 8 + u64::from(count) * entry_size as u64 {
            return Err(Error::InvalidMedia(
                "truncated video edit-list entries".into(),
            ));
        }
        let data = self.read_exact_at(elst.payload_offset() + 8, count as usize * entry_size)?;
        let mut edit = TimingEdit {
            media_start: 0,
            empty_duration: 0,
            duration: None,
            movie_timescale: movie_timescale.ok_or_else(|| {
                Error::InvalidMedia("video edit list requires a valid movie timescale".into())
            })?,
        };
        for (index, entry) in data.chunks_exact(entry_size).enumerate() {
            let (duration, start, rate_offset) = if prefix[0] == 1 {
                (
                    be_u64(&entry[..8]),
                    i64::from_be_bytes(entry[8..16].try_into().expect("edit width")),
                    16,
                )
            } else {
                (
                    u64::from(be_u32(&entry[..4])),
                    i64::from(i32::from_be_bytes(
                        entry[4..8].try_into().expect("edit width"),
                    )),
                    8,
                )
            };
            if entry[rate_offset..] != [0, 1, 0, 0] || duration == 0 {
                return Err(Error::MissingCapability(
                    "zero-duration, dwell, or non-unit-rate video edits are unsupported".into(),
                ));
            }
            if start == -1 && index == 0 && count == 2 {
                edit.empty_duration = duration;
            } else if start >= 0 && index + 1 == count as usize {
                edit.media_start = start;
                edit.duration = Some(duration);
            } else {
                return Err(Error::MissingCapability(
                    "multiple or negative media edits are unsupported".into(),
                ));
            }
        }
        Ok(edit)
    }

    /// Reads one indexed proprietary record after re-validating its bounds.
    ///
    /// The explicit maximum keeps callers from accidentally allocating an
    /// unbounded payload from untrusted media. A hard 512 MiB ceiling applies
    /// even when a larger value is requested.
    pub fn read_record_payload(
        &mut self,
        record: &RecordInfo,
        maximum_size: u64,
    ) -> Result<Vec<u8>> {
        let maximum_size = maximum_size.min(MAX_RECORD_READ_SIZE);
        if record.size > maximum_size {
            return Err(Error::InvalidMedia(format!(
                "INSV record {} is {} bytes, exceeding the {} byte read limit",
                record.id, record.size, maximum_size
            )));
        }
        let (_, indexed) = self.read_trailer_index()?;
        if !indexed.contains(record) {
            return Err(Error::InvalidMedia(
                "record descriptor is not present in this INSV trailer".into(),
            ));
        }
        let length = usize::try_from(record.size)
            .map_err(|_| Error::InvalidMedia("record size does not fit memory".into()))?;
        self.read_exact_at(record.offset, length)
    }

    /// Returns the wrapped reader.
    pub fn into_inner(self) -> R {
        self.inner
    }

    fn scan_top_level_boxes(&mut self) -> Result<Vec<BoxInfo>> {
        let mut boxes = Vec::new();
        let mut position = 0_u64;
        while position < self.file_size {
            if boxes.len() == MAX_TOP_LEVEL_BOXES {
                return Err(Error::InvalidMedia(
                    "ISO-BMFF top-level box count exceeds the parser limit".into(),
                ));
            }
            let header = self.read_box_header(position, self.file_size)?;
            position = header
                .offset
                .checked_add(header.size)
                .ok_or_else(|| Error::InvalidMedia("ISO-BMFF box end overflows".into()))?;
            boxes.push(BoxInfo {
                kind: header.kind,
                offset: header.offset,
                size: header.size,
            });
        }
        Ok(boxes)
    }

    fn read_trailer_index(&mut self) -> Result<(TrailerInfo, Vec<RecordInfo>)> {
        let minimum_size = TRAILER_HEADER_SIZE + 8;
        if self.file_size < minimum_size {
            return Err(Error::InvalidMedia(
                "input is too small to contain an INSV trailer".into(),
            ));
        }

        let header_offset = self.file_size - TRAILER_HEADER_SIZE;
        let header = self.read_exact_at(header_offset, TRAILER_HEADER_SIZE as usize)?;
        if &header[40..] != INSTA360_MAGIC {
            return Err(Error::InvalidMedia("missing Insta360 trailer magic".into()));
        }

        let payload_size = u64::from(le_u32(&header[32..36]));
        let version_u32 = le_u32(&header[36..40]);
        let version = u8::try_from(version_u32).map_err(|_| {
            Error::MissingCapability(format!("unsupported INSV trailer version {version_u32}"))
        })?;
        if version != 3 {
            return Err(Error::MissingCapability(format!(
                "INSV trailer version {version} is unsupported by the typed V3 reader"
            )));
        }
        let trailer_size = payload_size
            .checked_add(8)
            .ok_or_else(|| Error::InvalidMedia("INSV trailer size overflows".into()))?;
        if trailer_size > self.file_size {
            return Err(Error::InvalidMedia(
                "INSV trailer declares more data than the file contains".into(),
            ));
        }
        let trailer_offset = self.file_size - trailer_size;
        let box_header = self.read_box_header(trailer_offset, self.file_size)?;
        if box_header.kind != *b"inst" || box_header.size != trailer_size {
            return Err(Error::InvalidMedia(
                "INSV trailer length does not match its `inst` box".into(),
            ));
        }

        let payload_offset = trailer_offset + box_header.header_size;
        if header_offset == payload_offset {
            return Ok((
                TrailerInfo {
                    offset: trailer_offset,
                    size: trailer_size,
                    version,
                    record_count: 0,
                },
                Vec::new(),
            ));
        }
        if header_offset < payload_offset || header_offset - payload_offset < RECORD_FOOTER_SIZE {
            return Err(Error::InvalidMedia(
                "INSV trailer has no complete record footer".into(),
            ));
        }
        let footer_offset = self.file_size - TRAILER_HEADER_SIZE - RECORD_FOOTER_SIZE;
        let footer = self.read_exact_at(footer_offset, RECORD_FOOTER_SIZE as usize)?;
        if footer[..2] != [0, 0] {
            return self.read_records_backwards(
                payload_offset,
                version,
                trailer_offset,
                trailer_size,
            );
        }

        let index_size = u64::from(le_u32(&footer[2..6]));
        if index_size > MAX_INDEX_SIZE || index_size % OFFSET_ENTRY_SIZE as u64 != 0 {
            return Err(Error::InvalidMedia("invalid INSV record index size".into()));
        }
        let index_offset = footer_offset.checked_sub(index_size).ok_or_else(|| {
            Error::InvalidMedia("INSV record index starts before the file".into())
        })?;
        if index_offset < payload_offset {
            return Err(Error::InvalidMedia(
                "INSV record index starts before the trailer payload".into(),
            ));
        }
        let index = self.read_exact_at(index_offset, index_size as usize)?;
        let mut records = Vec::new();
        for entry in index.chunks_exact(OFFSET_ENTRY_SIZE) {
            let id = entry[0];
            if entry.iter().all(|byte| *byte == 0) {
                continue;
            }
            if id == 0 {
                return Err(Error::InvalidMedia(
                    "nonempty INSV index entry uses reserved record ID zero".into(),
                ));
            }
            if records.len() == MAX_INDEX_RECORDS {
                return Err(Error::InvalidMedia(
                    "INSV record count exceeds the parser limit".into(),
                ));
            }
            let format = entry[1];
            let size = u64::from(le_u32(&entry[2..6]));
            let relative_offset = u64::from(le_u32(&entry[6..10]));
            let offset = payload_offset
                .checked_add(relative_offset)
                .ok_or_else(|| Error::InvalidMedia("INSV record offset overflows".into()))?;
            validate_record_bounds(offset, size, index_offset)?;
            let record_footer = self.read_exact_at(offset + size, RECORD_FOOTER_SIZE as usize)?;
            if record_footer[0] != format
                || record_footer[1] != id
                || u64::from(le_u32(&record_footer[2..6])) != size
            {
                return Err(Error::InvalidMedia(format!(
                    "INSV record {id} does not match its index entry"
                )));
            }
            records.push(RecordInfo {
                id,
                format,
                offset,
                size,
            });
        }
        records.sort_by_key(|record| record.offset);
        for pair in records.windows(2) {
            if pair[0].offset + pair[0].size + RECORD_FOOTER_SIZE > pair[1].offset {
                return Err(Error::InvalidMedia(
                    "INSV index contains overlapping or duplicate record ranges".into(),
                ));
            }
        }
        records.sort_by_key(|record| record.id);

        let record_count = u32::try_from(records.len())
            .map_err(|_| Error::InvalidMedia("INSV record count overflows".into()))?;
        Ok((
            TrailerInfo {
                offset: trailer_offset,
                size: trailer_size,
                version,
                record_count,
            },
            records,
        ))
    }

    fn read_records_backwards(
        &mut self,
        payload_offset: u64,
        version: u8,
        trailer_offset: u64,
        trailer_size: u64,
    ) -> Result<(TrailerInfo, Vec<RecordInfo>)> {
        let payload_end = trailer_offset
            .checked_add(trailer_size)
            .and_then(|end| end.checked_sub(TRAILER_HEADER_SIZE))
            .ok_or_else(|| Error::InvalidMedia("invalid legacy trailer bounds".into()))?;
        let mut cursor = payload_end;
        let mut records = Vec::new();
        while cursor >= payload_offset + RECORD_FOOTER_SIZE {
            if records.len() == MAX_INDEX_RECORDS {
                return Err(Error::InvalidMedia(
                    "INSV record count exceeds the parser limit".into(),
                ));
            }
            let footer_offset = cursor - RECORD_FOOTER_SIZE;
            let footer = self.read_exact_at(footer_offset, RECORD_FOOTER_SIZE as usize)?;
            let size = u64::from(le_u32(&footer[2..6]));
            let offset = footer_offset.checked_sub(size).ok_or_else(|| {
                Error::InvalidMedia("legacy INSV record starts before trailer".into())
            })?;
            if offset < payload_offset {
                return Err(Error::InvalidMedia(
                    "legacy INSV record starts before trailer payload".into(),
                ));
            }
            records.push(RecordInfo {
                id: footer[1],
                format: footer[0],
                offset,
                size,
            });
            cursor = offset;
            if cursor == payload_offset {
                break;
            }
        }
        if cursor != payload_offset {
            return Err(Error::InvalidMedia(
                "legacy INSV trailer contains a partial record prefix".into(),
            ));
        }
        records.sort_by_key(|record| record.id);
        let record_count = u32::try_from(records.len())
            .map_err(|_| Error::InvalidMedia("INSV record count overflows".into()))?;
        Ok((
            TrailerInfo {
                offset: trailer_offset,
                size: trailer_size,
                version,
                record_count,
            },
            records,
        ))
    }

    fn read_metadata(&mut self, records: &[RecordInfo]) -> Result<InsvMetadata> {
        let Some(record) = records.iter().find(|record| record.id == 1) else {
            return Ok(InsvMetadata::default());
        };
        if record.format != 1 {
            return Err(Error::MissingCapability(format!(
                "INSV metadata format {} is unsupported by the typed protobuf reader",
                record.format
            )));
        }
        if record.size > MAX_METADATA_SIZE {
            return Err(Error::InvalidMedia(format!(
                "metadata record exceeds the {MAX_METADATA_SIZE}-byte parser limit"
            )));
        }
        let data = self.read_exact_at(record.offset, record.size as usize)?;
        parse_metadata(&data)
    }

    fn read_movie_info(&mut self, boxes: &[BoxInfo]) -> Result<MovieInfo> {
        let Some(moov) = boxes.iter().find(|item| item.kind == *b"moov") else {
            return Ok(MovieInfo::default());
        };
        let header = self.read_box_header(moov.offset, moov.offset + moov.size)?;
        let children =
            self.child_boxes(moov.offset + header.header_size, moov.offset + moov.size)?;
        let mut video_tracks = Vec::new();
        let mut duration = None;
        let mut fps = None;
        for child in children.into_iter().filter(|item| item.kind == *b"trak") {
            if let Some(track) = self.read_track(&child)? {
                let index = video_tracks.len();
                video_tracks.push(VideoTrackInfo {
                    index,
                    width: track.width,
                    height: track.height,
                    codec: track.codec,
                });
                duration = max_duration(duration, track.duration);
                if fps.is_none() {
                    fps = track.fps;
                }
            }
        }
        Ok(MovieInfo {
            video_tracks,
            duration,
            fps,
        })
    }

    fn read_track(&mut self, track: &BoxHeader) -> Result<Option<TrackInfo>> {
        let children = self.child_boxes(track.payload_offset(), track.end())?;
        let Some(mdia) = children.iter().find(|item| item.kind == *b"mdia") else {
            return Ok(None);
        };
        let mdia_children = self.child_boxes(mdia.payload_offset(), mdia.end())?;
        let handler = mdia_children
            .iter()
            .find(|item| item.kind == *b"hdlr")
            .map(|item| self.read_handler(item))
            .transpose()?;
        if handler != Some(*b"vide") {
            return Ok(None);
        }

        let timing = mdia_children
            .iter()
            .find(|item| item.kind == *b"mdhd")
            .map(|item| self.read_media_header(item))
            .transpose()?
            .flatten();
        let sample_table = self.find_descendant(&mdia_children, b"minf", b"stbl")?;
        let sample_children = match sample_table {
            Some(table) => self.child_boxes(table.payload_offset(), table.end())?,
            None => Vec::new(),
        };
        let visual = sample_children
            .iter()
            .find(|item| item.kind == *b"stsd")
            .map(|item| self.read_sample_description(item))
            .transpose()?
            .flatten();
        let Some((codec, width, height)) = visual else {
            return Ok(None);
        };

        let sample_timing = sample_children
            .iter()
            .find(|item| item.kind == *b"stts")
            .map(|item| self.read_sample_timing(item))
            .transpose()?
            .flatten();
        let duration = timing.and_then(|(timescale, ticks)| duration_from_ticks(timescale, ticks));
        let fps = match (timing, sample_timing) {
            (Some((timescale, _)), Some((sample_count, sample_ticks))) if sample_ticks > 0 => {
                Some(sample_count as f64 * f64::from(timescale) / sample_ticks as f64)
            }
            _ => None,
        };
        Ok(Some(TrackInfo {
            width,
            height,
            codec,
            duration,
            fps,
        }))
    }

    fn find_descendant(
        &mut self,
        boxes: &[BoxHeader],
        parent_kind: &[u8; 4],
        child_kind: &[u8; 4],
    ) -> Result<Option<BoxHeader>> {
        let Some(parent) = boxes.iter().find(|item| &item.kind == parent_kind) else {
            return Ok(None);
        };
        Ok(self
            .child_boxes(parent.payload_offset(), parent.end())?
            .into_iter()
            .find(|item| &item.kind == child_kind))
    }

    fn read_handler(&mut self, header: &BoxHeader) -> Result<[u8; 4]> {
        if header.payload_size() < 12 {
            return Err(Error::InvalidMedia("truncated `hdlr` box".into()));
        }
        let data = self.read_exact_at(header.payload_offset() + 8, 4)?;
        Ok([data[0], data[1], data[2], data[3]])
    }

    fn read_media_header(&mut self, header: &BoxHeader) -> Result<Option<(u32, u64)>> {
        if header.payload_size() < 20 {
            return Err(Error::InvalidMedia("truncated `mdhd` box".into()));
        }
        let version = self.read_exact_at(header.payload_offset(), 1)?[0];
        let (timescale_offset, duration_offset, required) = match version {
            0 => (12_u64, 16_u64, 20_u64),
            1 => (20_u64, 24_u64, 32_u64),
            _ => return Ok(None),
        };
        if header.payload_size() < required {
            return Err(Error::InvalidMedia("truncated `mdhd` timing fields".into()));
        }
        let timescale = be_u32(&self.read_exact_at(header.payload_offset() + timescale_offset, 4)?);
        let duration = if version == 0 {
            u64::from(be_u32(
                &self.read_exact_at(header.payload_offset() + duration_offset, 4)?,
            ))
        } else {
            be_u64(&self.read_exact_at(header.payload_offset() + duration_offset, 8)?)
        };
        if timescale == 0 {
            return Ok(None);
        }
        Ok(Some((timescale, duration)))
    }

    fn read_sample_description(
        &mut self,
        header: &BoxHeader,
    ) -> Result<Option<(String, u32, u32)>> {
        if header.payload_size() < 8 {
            return Err(Error::InvalidMedia("truncated `stsd` box".into()));
        }
        let prefix = self.read_exact_at(header.payload_offset(), 8)?;
        if be_u32(&prefix[4..8]) == 0 {
            return Ok(None);
        }
        let entry_offset = header.payload_offset() + 8;
        let entry = self.read_box_header(entry_offset, header.end())?;
        if entry.size < 36 {
            return Err(Error::InvalidMedia("truncated video sample entry".into()));
        }
        let dimensions = self.read_exact_at(entry_offset + 32, 4)?;
        let width = u32::from(be_u16(&dimensions[0..2]));
        let height = u32::from(be_u16(&dimensions[2..4]));
        let codec = String::from_utf8_lossy(&entry.kind).into_owned();
        Ok(Some((codec, width, height)))
    }

    fn read_sample_timing(&mut self, header: &BoxHeader) -> Result<Option<(u64, u64)>> {
        if header.payload_size() < 8 {
            return Err(Error::InvalidMedia("truncated `stts` box".into()));
        }
        let prefix = self.read_exact_at(header.payload_offset(), 8)?;
        let entry_count = be_u32(&prefix[4..8]);
        if entry_count == 0 || entry_count > MAX_STTS_ENTRIES {
            return Ok(None);
        }
        let bytes = u64::from(entry_count) * 8;
        if header.payload_size() < 8 + bytes {
            return Err(Error::InvalidMedia("truncated `stts` entries".into()));
        }
        let mut sample_count = 0_u64;
        let mut sample_ticks = 0_u64;
        for index in 0..entry_count {
            let entry =
                self.read_exact_at(header.payload_offset() + 8 + u64::from(index) * 8, 8)?;
            let count = u64::from(be_u32(&entry[0..4]));
            let delta = u64::from(be_u32(&entry[4..8]));
            sample_count = sample_count
                .checked_add(count)
                .ok_or_else(|| Error::InvalidMedia("sample count overflows".into()))?;
            sample_ticks = sample_ticks
                .checked_add(
                    count
                        .checked_mul(delta)
                        .ok_or_else(|| Error::InvalidMedia("sample duration overflows".into()))?,
                )
                .ok_or_else(|| Error::InvalidMedia("sample duration overflows".into()))?;
        }
        Ok(Some((sample_count, sample_ticks)))
    }

    fn child_boxes(&mut self, start: u64, end: u64) -> Result<Vec<BoxHeader>> {
        let mut boxes = Vec::new();
        let mut position = start;
        while position < end {
            if boxes.len() == MAX_TOP_LEVEL_BOXES {
                return Err(Error::InvalidMedia(
                    "ISO-BMFF child box count exceeds the parser limit".into(),
                ));
            }
            let header = self.read_box_header(position, end)?;
            position = header.end();
            boxes.push(header);
        }
        Ok(boxes)
    }

    fn read_box_header(&mut self, offset: u64, limit: u64) -> Result<BoxHeader> {
        if offset > limit || limit - offset < 8 {
            return Err(Error::InvalidMedia("truncated ISO-BMFF box header".into()));
        }
        let basic = self.read_exact_at(offset, 8)?;
        let size32 = be_u32(&basic[0..4]);
        let kind = [basic[4], basic[5], basic[6], basic[7]];
        let (size, header_size) = match size32 {
            0 => (limit - offset, 8),
            1 => {
                if limit - offset < 16 {
                    return Err(Error::InvalidMedia(
                        "truncated extended ISO-BMFF box header".into(),
                    ));
                }
                (be_u64(&self.read_exact_at(offset + 8, 8)?), 16)
            }
            value => (u64::from(value), 8),
        };
        if size < header_size || size > limit - offset {
            return Err(Error::InvalidMedia(format!(
                "invalid ISO-BMFF box size at byte {offset}"
            )));
        }
        Ok(BoxHeader {
            kind,
            offset,
            size,
            header_size,
        })
    }

    fn read_exact_at(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        let length_u64 = u64::try_from(length)
            .map_err(|_| Error::InvalidMedia("requested read length overflows".into()))?;
        if offset > self.file_size || length_u64 > self.file_size - offset {
            return Err(Error::InvalidMedia(format!(
                "read at byte {offset} exceeds the input bounds"
            )));
        }
        self.inner
            .seek(SeekFrom::Start(offset))
            .map_err(|error| invalid_io("seeking input", error))?;
        let mut data = vec![0; length];
        self.inner
            .read_exact(&mut data)
            .map_err(|error| invalid_io("reading input", error))?;
        Ok(data)
    }
}

/// Probes a validated recording input set.
pub fn probe(inputs: &InputSet) -> Result<MediaInfo> {
    let mut inspections = Vec::with_capacity(inputs.paths.len());
    for path in &inputs.paths {
        let file = File::open(path).map_err(|error| io_error(path, error))?;
        let mut reader = InsvReader::new(file)?;
        let inspection = reader
            .inspect()
            .map_err(|error| contextualize(path, error))?;
        inspections.push(inspection);
    }

    let primary = inspections
        .first()
        .ok_or_else(|| Error::InvalidMedia("input set is empty".into()))?;
    validate_pair_inspections(&inspections)?;

    let mut tracks = Vec::new();
    for inspection in &inspections {
        for track in &inspection.video_tracks {
            let mut track = track.clone();
            track.index = tracks.len();
            tracks.push(track);
        }
    }
    if tracks.is_empty() {
        return Err(Error::InvalidMedia(
            "recording contains no video tracks".into(),
        ));
    }

    let camera_name = primary.metadata.camera_name.clone();
    let camera = match camera_name.as_deref() {
        Some(name) => camera_profile_for_name(name)
            .map(|profile| profile.camera.clone())
            .unwrap_or_else(|| CameraModel::Unknown(name.to_owned())),
        None => CameraModel::Unknown("unspecified".into()),
    };
    let mut offset_versions = primary
        .metadata
        .offsets
        .iter()
        .map(|offset| offset.version)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    offset_versions.sort_unstable();
    let optical_profiles = primary
        .metadata
        .profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(MediaInfo {
        inputs: inputs.paths.clone(),
        camera,
        camera_name,
        serial: primary.metadata.serial.clone(),
        firmware: primary.metadata.firmware.clone(),
        duration: primary.duration,
        fps: primary.fps,
        video_tracks: tracks,
        offset_versions,
        optical_profiles,
        gyro_sample_count: primary.metadata.gyro_sample_count,
        exposure_sample_count: primary.metadata.exposure_sample_count,
        trailer: primary.trailer,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairKind {
    Primary,
    Secondary,
}

#[derive(Clone, Debug)]
struct BoxHeader {
    kind: [u8; 4],
    offset: u64,
    size: u64,
    header_size: u64,
}

impl BoxHeader {
    fn payload_offset(&self) -> u64 {
        self.offset + self.header_size
    }

    fn payload_size(&self) -> u64 {
        self.size - self.header_size
    }

    fn end(&self) -> u64 {
        self.offset + self.size
    }
}

#[derive(Default)]
struct MovieInfo {
    video_tracks: Vec<VideoTrackInfo>,
    duration: Option<Duration>,
    fps: Option<f64>,
}

struct TrackInfo {
    width: u32,
    height: u32,
    codec: String,
    duration: Option<Duration>,
    fps: Option<f64>,
}

fn validate_input_path(path: &Path) -> Result<()> {
    if !path.is_file() {
        let source = std::fs::metadata(path)
            .err()
            .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not a file"));
        return Err(io_error(path, source));
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("insv"))
    {
        return Err(Error::InvalidMedia(format!(
            "input {} does not have an .insv extension",
            path.display()
        )));
    }
    Ok(())
}

fn pair_identity(path: &Path) -> Option<(PairKind, String)> {
    let name = path.file_name()?.to_str()?;
    let uppercase = name.to_ascii_uppercase();
    if let Some(index) = uppercase.find("_00_") {
        let mut identity = uppercase;
        identity.replace_range(index..index + 4, "_XX_");
        Some((PairKind::Primary, identity))
    } else if let Some(index) = uppercase.find("_10_") {
        let mut identity = uppercase;
        identity.replace_range(index..index + 4, "_XX_");
        Some((PairKind::Secondary, identity))
    } else {
        None
    }
}

fn replace_pair_marker(path: &Path, from: &str, to: &str) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let uppercase = name.to_ascii_uppercase();
    let index = uppercase.find(from)?;
    let mut replacement = name.to_owned();
    replacement.replace_range(index..index + from.len(), to);
    Some(path.with_file_name(replacement))
}

fn validate_pair_inspections(inspections: &[InsvInspection]) -> Result<()> {
    if inspections.len() != 2 {
        return Ok(());
    }
    let primary = &inspections[0];
    let secondary = &inspections[1];
    if let (Some(first), Some(second)) = (
        primary.metadata.camera_name.as_deref(),
        secondary.metadata.camera_name.as_deref(),
    ) {
        if first != second {
            return Err(Error::InvalidMedia(
                "paired files report different camera models".into(),
            ));
        }
    }
    if let (Some(first), Some(second)) = (primary.duration, secondary.duration) {
        let difference = first.abs_diff(second);
        if difference > Duration::from_millis(250) {
            return Err(Error::InvalidMedia(
                "paired files have incompatible durations".into(),
            ));
        }
    }
    Ok(())
}

fn validate_record_bounds(offset: u64, size: u64, record_region_end: u64) -> Result<()> {
    let footer_end = offset
        .checked_add(size)
        .and_then(|end| end.checked_add(RECORD_FOOTER_SIZE))
        .ok_or_else(|| Error::InvalidMedia("INSV record bounds overflow".into()))?;
    if footer_end > record_region_end {
        return Err(Error::InvalidMedia(
            "INSV record extends into the trailer index".into(),
        ));
    }
    Ok(())
}

pub(crate) fn parse_metadata(data: &[u8]) -> Result<InsvMetadata> {
    let fields = protobuf_fields(data)?;
    let recorded_color_mode = parse_recorded_color_mode(&fields);
    let mut metadata = InsvMetadata {
        gamma_mode: parse_gamma_mode(&fields),
        recorded_color_mode_invalid: recorded_color_mode.is_err(),
        recorded_color_mode: recorded_color_mode.ok().flatten(),
        ..InsvMetadata::default()
    };
    match parse_sequence_metadata(&fields) {
        Ok((group, split)) => {
            metadata.recording_group = group;
            metadata.file_split_type = split;
        }
        Err(()) => metadata.sequence_metadata_invalid = true,
    }
    for field in &fields {
        let handled = match (field.number, &field.value) {
            (1, ProtobufValue::Bytes(value)) => {
                metadata.serial = utf8_value(value);
                metadata.serial.is_some()
            }
            (2, ProtobufValue::Bytes(value)) => {
                metadata.camera_name = utf8_value(value);
                metadata.camera_name.is_some()
            }
            (3, ProtobufValue::Bytes(value)) => {
                metadata.firmware = utf8_value(value);
                metadata.firmware.is_some()
            }
            (5, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 1, false, value),
            (17, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 1, true, value),
            (20, ProtobufValue::Varint(value)) => {
                metadata.nominal_frame_rate = u32::try_from(*value).ok();
                metadata.nominal_frame_rate.is_some()
            }
            (24, ProtobufValue::Varint(value)) => {
                // Protobuf int64 uses a ten-byte two's-complement varint for
                // negative values, not an unsigned range-checked conversion.
                metadata.first_frame_timestamp = Some(*value as i64);
                true
            }
            (25, ProtobufValue::Fixed64(value)) => {
                metadata.rolling_shutter_time = finite_nonnegative_f64(*value);
                metadata.rolling_shutter_time.is_some()
            }
            (27, ProtobufValue::Bytes(value)) => {
                metadata.crop_window = parse_crop_window(value);
                metadata.crop_window.is_some()
            }
            (28, ProtobufValue::Fixed64(value)) => {
                let milliseconds = f64::from_bits(*value);
                if milliseconds.is_finite() {
                    metadata.gyro_timestamp_adjust_ms = Some(milliseconds);
                    true
                } else {
                    false
                }
            }
            (29, ProtobufValue::Varint(value)) => {
                metadata.has_gyro_timestamp_adjust = Some(*value != 0);
                true
            }
            (30, ProtobufValue::Fixed32(value)) => {
                let interval = f32::from_bits(*value);
                if interval.is_finite() && interval >= 0.0 {
                    metadata.timelapse_interval = Some(f64::from(interval));
                    true
                } else {
                    false
                }
            }
            (53, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 2, false, value),
            (54, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 3, false, value),
            (55, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 2, true, value),
            (56, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 3, true, value),
            (59, ProtobufValue::Varint(value)) => {
                metadata.timelapse_interval_is_milliseconds = Some(*value != 0);
                true
            }
            (62, ProtobufValue::Varint(value)) => {
                metadata.is_raw_gyro = Some(*value != 0);
                true
            }
            (64, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| {
                    metadata.video_pts_map_type = Some(VideoPtsMapType::from_raw(value));
                })
                .is_ok(),
            (65, ProtobufValue::Bytes(value)) => {
                metadata.imu_range = parse_imu_range(value);
                metadata.imu_range.is_some()
            }
            (68, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| metadata.offset_state = Some(OffsetState::from_raw(value)))
                .is_ok(),
            (79, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| metadata.pano_record_type = Some(PanoRecordType::from_raw(value)))
                .is_ok(),
            (80, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| {
                    metadata.pano_record_track_order = Some(PanoRecordTrackOrder::from_raw(value));
                })
                .is_ok(),
            (104, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| {
                    metadata.guard_detected_type = Some(GuardDetectedType::from_raw(value));
                })
                .is_ok(),
            (111, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 6, false, value),
            (112, ProtobufValue::Bytes(value)) => push_offset(&mut metadata, 6, true, value),
            (127, ProtobufValue::Varint(value)) => {
                let value = protobuf_i32(*value);
                if value >= 0 {
                    metadata.file_duration_tailor_time_ms = Some(value);
                    true
                } else {
                    false
                }
            }
            (128, ProtobufValue::Varint(value)) => {
                let value = protobuf_i32(*value);
                if value >= 0 {
                    metadata.blend_angle = Some(value);
                    true
                } else {
                    false
                }
            }
            (129, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| metadata.file_category = Some(FileCategory::from_raw(value)))
                .is_ok(),
            (130, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| metadata.file_rotation = Some(FileRotation::from_raw(value)))
                .is_ok(),
            (131, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| metadata.stream_type = Some(StreamType::from_raw(value)))
                .is_ok(),
            (132, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| metadata.codec_type = Some(CodecType::from_raw(value)))
                .is_ok(),
            (133, ProtobufValue::Varint(value)) => {
                metadata.timelapse_interval_ms = Some(*value);
                true
            }
            (134, ProtobufValue::Varint(value)) => {
                metadata.is_pre_recorded = Some(*value != 0);
                true
            }
            (135, ProtobufValue::Varint(value)) => {
                metadata.expected_bitrate = Some(*value);
                true
            }
            (136, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| {
                    metadata.capture_offset_version = Some(CaptureOffsetVersion::from_raw(value));
                })
                .is_ok(),
            (145, ProtobufValue::Bytes(value)) => {
                let previous_length = metadata.profiles.len();
                collect_profiles(value, &mut metadata.profiles, 0);
                metadata.profiles.len() > previous_length
            }
            (186, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| {
                    metadata.lens_accessory_type = Some(LensAccessoryType::from_raw(value));
                })
                .is_ok(),
            (193, ProtobufValue::Varint(value)) => {
                metadata.p3_fake_on = Some(*value != 0);
                true
            }
            _ => false,
        };
        if !handled {
            metadata.unknown_fields.push(retain_unknown_field(field));
        }
    }
    populate_declared_track_layout(&mut metadata);
    deduplicate_offsets(&mut metadata.offsets);
    deduplicate_profiles(&mut metadata.profiles);
    Ok(metadata)
}

fn parse_sequence_metadata(
    fields: &[ProtobufField<'_>],
) -> std::result::Result<(Option<RecordingGroup>, Option<FileSplitType>), ()> {
    let mut group = None;
    let mut split = None;
    for field in fields {
        match (field.number, &field.value) {
            (26, ProtobufValue::Bytes(bytes)) => {
                let nested = protobuf_fields(bytes).map_err(|_| ())?;
                let mut parsed = RecordingGroup::default();
                let mut seen = std::collections::BTreeMap::new();
                for child in &nested {
                    if !(1..=4).contains(&child.number) {
                        continue;
                    }
                    let retained = retain_unknown_field(child);
                    if seen
                        .insert(child.number, retained.clone())
                        .is_some_and(|old| old != retained)
                    {
                        return Err(());
                    }
                    match (child.number, &child.value) {
                        (1, ProtobufValue::Varint(value)) => {
                            parsed.capture_type = u32::try_from(*value).map_err(|_| ())?
                        }
                        (2, ProtobufValue::Varint(value)) => {
                            parsed.index = u32::try_from(*value).map_err(|_| ())?
                        }
                        (3, ProtobufValue::Bytes(value)) => {
                            if value.len() > 4096 {
                                return Err(());
                            }
                            parsed.identity =
                                std::str::from_utf8(value).map_err(|_| ())?.to_owned();
                        }
                        (4, ProtobufValue::Varint(value)) => {
                            parsed.total = u32::try_from(*value).map_err(|_| ())?
                        }
                        _ => return Err(()),
                    }
                }
                if group.as_ref().is_some_and(|old| old != &parsed) {
                    return Err(());
                }
                group = Some(parsed);
            }
            (88, ProtobufValue::Varint(value)) => {
                let parsed = match u32::try_from(*value).map_err(|_| ())? {
                    0 => FileSplitType::Unknown,
                    1 => FileSplitType::NotSplit,
                    2 => FileSplitType::Split,
                    value => FileSplitType::Other(value),
                };
                if split.is_some_and(|old| old != parsed) {
                    return Err(());
                }
                split = Some(parsed);
            }
            (26 | 88, _) => return Err(()),
            _ => {}
        }
    }
    Ok((group, split))
}

fn parse_gamma_mode(fields: &[ProtobufField<'_>]) -> Option<String> {
    let mut gamma = None;
    for field in fields.iter().filter(|field| field.number == 22) {
        let ProtobufValue::Bytes(bytes) = field.value else {
            return None;
        };
        let value = std::str::from_utf8(bytes).ok()?;
        if gamma.is_some_and(|previous| previous != value) {
            return None;
        }
        gamma = Some(value);
    }
    gamma.map(str::to_owned)
}

fn parse_recorded_color_mode(
    fields: &[ProtobufField<'_>],
) -> std::result::Result<Option<RecordedColorMode>, ()> {
    // iOS SDK 1.10.4's embedded ExtraMetadata descriptor declares message 212,
    // nested enum field 8: UNKNOWN=0, STANDARD=1, ILOG=2, DOLBY=3.
    let mut color_mode = None;
    for field in fields.iter().filter(|field| field.number == 212) {
        let ProtobufValue::Bytes(bytes) = field.value else {
            return Err(());
        };
        for nested in protobuf_fields(bytes).map_err(|_| ())? {
            if nested.number != 8 {
                continue;
            }
            let ProtobufValue::Varint(value) = nested.value else {
                return Err(());
            };
            let value = RecordedColorMode::from_raw(u32::try_from(value).map_err(|_| ())?);
            if color_mode.is_some_and(|previous| previous != value) {
                return Err(());
            }
            color_mode = Some(value);
        }
    }
    Ok(color_mode)
}

fn push_offset(metadata: &mut InsvMetadata, version: u8, original: bool, value: &[u8]) -> bool {
    if let Some(value) = utf8_value(value).filter(|value| looks_like_offset(value)) {
        metadata.offsets.push(EmbeddedOffset {
            version,
            original,
            value,
        });
        true
    } else {
        false
    }
}

fn finite_nonnegative_f64(value: u64) -> Option<f64> {
    let value = f64::from_bits(value);
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn protobuf_i32(value: u64) -> i32 {
    value as u32 as i32
}

fn parse_crop_window(data: &[u8]) -> Option<CropWindow> {
    let fields = protobuf_fields(data).ok()?;
    let mut source_width = None;
    let mut source_height = None;
    let mut destination_width = None;
    let mut destination_height = None;
    let mut x_offset = 0;
    let mut y_offset = 0;
    let mut unknown_fields = Vec::new();

    for field in &fields {
        let handled = match (field.number, &field.value) {
            (1, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| source_width = Some(value))
                .is_ok(),
            (2, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| source_height = Some(value))
                .is_ok(),
            (3, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| destination_width = Some(value))
                .is_ok(),
            (4, ProtobufValue::Varint(value)) => u32::try_from(*value)
                .map(|value| destination_height = Some(value))
                .is_ok(),
            (5, ProtobufValue::Varint(value)) => {
                x_offset = protobuf_i32(*value);
                true
            }
            (6, ProtobufValue::Varint(value)) => {
                y_offset = protobuf_i32(*value);
                true
            }
            _ => false,
        };
        if !handled {
            unknown_fields.push(retain_unknown_field(field));
        }
    }

    let crop = CropWindow {
        source_width: source_width?,
        source_height: source_height?,
        destination_width: destination_width?,
        destination_height: destination_height?,
        x_offset,
        y_offset,
        unknown_fields,
    };
    (crop.source_width > 0
        && crop.source_height > 0
        && crop.destination_width > 0
        && crop.destination_height > 0)
        .then_some(crop)
}

fn populate_declared_track_layout(metadata: &mut InsvMetadata) {
    metadata.video_track_count = match metadata.stream_type {
        Some(StreamType::SingleStreamFile | StreamType::DualStreamFiles) => Some(1),
        Some(StreamType::DualStreamTracks | StreamType::DualStreamTracksReversed) => Some(2),
        _ => None,
    };
    metadata.reverse_video_track_order = match metadata.pano_record_track_order {
        Some(PanoRecordTrackOrder::Track0IsStream10) => Some(true),
        Some(PanoRecordTrackOrder::Track0IsStream00) => Some(false),
        _ => match metadata.stream_type {
            Some(StreamType::DualStreamTracksReversed) => Some(true),
            Some(StreamType::DualStreamTracks) => Some(false),
            _ => None,
        },
    };
}

fn looks_like_offset(value: &str) -> bool {
    value.starts_with("2_")
        && value.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-' | b'+' | b'e' | b'E')
        })
}

fn collect_profiles(data: &[u8], output: &mut Vec<EmbeddedProfile>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(fields) = protobuf_fields(data) else {
        return;
    };
    for field in fields {
        let ProtobufValue::Bytes(value) = field.value else {
            continue;
        };
        if field.number == 1 {
            if let Some(name) = utf8_value(value).filter(|name| is_known_profile(name)) {
                output.push(EmbeddedProfile {
                    name,
                    payload: data.to_vec(),
                });
                return;
            }
        }
        collect_profiles(value, output, depth + 1);
    }
}

fn is_known_profile(value: &str) -> bool {
    matches!(
        value,
        "bare"
            | "BareUnderwater"
            | "ProtectorA"
            | "ProtectorS"
            | "ProtectorAS"
            | "InvisibleDiveWater"
            | "InvisibleDiveAir"
            | "ND16"
            | "ND32"
            | "ND64"
            | "invisibleDive"
            | "heat_bare"
            | "heat_protector"
    )
}

fn deduplicate_offsets(offsets: &mut Vec<EmbeddedOffset>) {
    offsets.sort_by(|left, right| {
        (left.version, left.original, &left.value).cmp(&(
            right.version,
            right.original,
            &right.value,
        ))
    });
    offsets.dedup();
}

fn deduplicate_profiles(profiles: &mut Vec<EmbeddedProfile>) {
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    profiles.dedup_by(|left, right| left.name == right.name);
}

fn populate_sample_counts(metadata: &mut InsvMetadata, records: &[RecordInfo]) {
    metadata.gyro_sample_count = records
        .iter()
        .find(|record| record.id == 3)
        .and_then(|record| match metadata.is_raw_gyro {
            Some(true) if record.size % 20 == 0 => Some(record.size / 20),
            Some(false) if record.size % 56 == 0 => Some(record.size / 56),
            _ => None,
        })
        .unwrap_or(0);
    metadata.exposure_sample_count = records
        .iter()
        .filter(|record| matches!(record.id, 4 | 12) && record.size % 16 == 0)
        .map(|record| record.size / 16)
        .sum();
}

#[derive(Debug)]
struct ProtobufField<'a> {
    number: u32,
    value: ProtobufValue<'a>,
}

#[derive(Debug)]
enum ProtobufValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64(u64),
    Fixed32(u32),
}

fn protobuf_fields(data: &[u8]) -> Result<Vec<ProtobufField<'_>>> {
    let mut cursor = 0_usize;
    let mut fields = Vec::new();
    while cursor < data.len() {
        let key = read_varint(data, &mut cursor)?;
        let number = key >> 3;
        if number == 0 || number > MAX_PROTOBUF_FIELD_NUMBER {
            return Err(Error::InvalidMedia(
                "protobuf metadata contains an invalid field number".into(),
            ));
        }
        let wire_type = key & 7;
        let value = match wire_type {
            0 => ProtobufValue::Varint(read_varint(data, &mut cursor)?),
            1 => {
                let start = cursor;
                advance(&mut cursor, 8, data.len())?;
                ProtobufValue::Fixed64(u64::from_le_bytes(
                    data[start..cursor]
                        .try_into()
                        .expect("fixed64 length was checked"),
                ))
            }
            2 => {
                let length = usize::try_from(read_varint(data, &mut cursor)?)
                    .map_err(|_| Error::InvalidMedia("protobuf field length overflows".into()))?;
                let start = cursor;
                advance(&mut cursor, length, data.len())?;
                ProtobufValue::Bytes(&data[start..cursor])
            }
            5 => {
                let start = cursor;
                advance(&mut cursor, 4, data.len())?;
                ProtobufValue::Fixed32(u32::from_le_bytes(
                    data[start..cursor]
                        .try_into()
                        .expect("fixed32 length was checked"),
                ))
            }
            _ => {
                return Err(Error::InvalidMedia(format!(
                    "protobuf metadata uses unsupported wire type {wire_type}"
                )))
            }
        };
        if fields.len() == MAX_PROTOBUF_FIELDS {
            return Err(Error::InvalidMedia(
                "protobuf metadata field count exceeds the parser limit".into(),
            ));
        }
        fields.push(ProtobufField {
            number: u32::try_from(number).expect("protobuf field number was range checked"),
            value,
        });
    }
    Ok(fields)
}

fn retain_unknown_field(field: &ProtobufField<'_>) -> UnknownMetadataField {
    let value = match field.value {
        ProtobufValue::Varint(value) => UnknownMetadataValue::Varint(value),
        ProtobufValue::Bytes(value) => UnknownMetadataValue::LengthDelimited(value.to_vec()),
        ProtobufValue::Fixed64(value) => UnknownMetadataValue::Fixed64(value),
        ProtobufValue::Fixed32(value) => UnknownMetadataValue::Fixed32(value),
    };
    UnknownMetadataField {
        number: field.number,
        value,
    }
}

fn parse_imu_range(data: &[u8]) -> Option<ImuRange> {
    let fields = protobuf_fields(data).ok()?;
    let mut accelerometer_g = None;
    let mut gyroscope_degrees_per_second = None;
    for field in fields {
        match (field.number, field.value) {
            (1, ProtobufValue::Varint(value)) => accelerometer_g = Some(value as f64),
            (2, ProtobufValue::Varint(value)) => gyroscope_degrees_per_second = Some(value as f64),
            _ => {}
        }
    }
    let range = ImuRange {
        accelerometer_g: accelerometer_g?,
        gyroscope_degrees_per_second: gyroscope_degrees_per_second?,
    };
    (range.accelerometer_g.is_finite()
        && range.accelerometer_g > 0.0
        && range.gyroscope_degrees_per_second.is_finite()
        && range.gyroscope_degrees_per_second > 0.0)
        .then_some(range)
}

fn read_varint(data: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0_u64;
    for shift in (0..=63).step_by(7) {
        let byte = *data
            .get(*cursor)
            .ok_or_else(|| Error::InvalidMedia("truncated protobuf varint".into()))?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(Error::InvalidMedia(
                "protobuf varint exceeds 64 bits".into(),
            ));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::InvalidMedia(
        "protobuf varint exceeds 64 bits".into(),
    ))
}

fn advance(cursor: &mut usize, count: usize, limit: usize) -> Result<()> {
    *cursor = cursor
        .checked_add(count)
        .filter(|end| *end <= limit)
        .ok_or_else(|| Error::InvalidMedia("protobuf field exceeds metadata bounds".into()))?;
    Ok(())
}

fn utf8_value(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|value| !value.is_empty() && value.len() <= MAX_METADATA_SIZE as usize)
        .map(str::to_owned)
}

fn duration_from_ticks(timescale: u32, ticks: u64) -> Option<Duration> {
    if timescale == 0 {
        return None;
    }
    let seconds = ticks as f64 / f64::from(timescale);
    seconds
        .is_finite()
        .then(|| Duration::from_secs_f64(seconds))
}

fn max_duration(first: Option<Duration>, second: Option<Duration>) -> Option<Duration> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.max(second)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn contextualize(path: &Path, error: Error) -> Error {
    match error {
        Error::InvalidMedia(message) => {
            Error::InvalidMedia(format!("{}: {message}", path.display()))
        }
        other => other,
    }
}

fn invalid_io(context: &str, error: std::io::Error) -> Error {
    Error::InvalidMedia(format!("I/O error while {context}: {error}"))
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
