//! Independent, seekable readers backed directly by the original recording.
//!
//! Packet access preserves encoded video/audio and never decodes or re-encodes.
//! Video decoding is explicit and produces RGB24 preview/extraction frames in
//! memory. No intermediate media files, camera profiles, or calibration are used.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ffmpeg::codec::packet::Ref;
use ffmpeg::util::mathematics::rescale::Rescale;
use ffmpeg_next as ffmpeg;
use serde::{Deserialize, Serialize};

use crate::{Error, InputSet, Result};

const MAX_STREAMS: usize = 256;
const MAX_PACKET_BYTES: usize = 256 * 1024 * 1024;
const MAX_SIDE_DATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_SIDE_DATA_ENTRIES: usize = 1024;
pub(crate) const MAX_FRAME_PIXELS: u64 = 128 * 1024 * 1024;

/// One stream tick expressed as a positive rational number of seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamTimeBase {
    /// Seconds numerator.
    pub numerator: i32,
    /// Seconds denominator.
    pub denominator: i32,
}

impl StreamTimeBase {
    fn validate(self) -> Result<Self> {
        if self.numerator <= 0 || self.denominator <= 0 {
            return Err(Error::InvalidMedia(
                "stream time base must be positive".into(),
            ));
        }
        Ok(self)
    }

    fn ticks(self, duration: Duration) -> Result<i64> {
        self.validate()?;
        let ticks = duration
            .as_nanos()
            .checked_mul(self.denominator as u128)
            .and_then(|value| value.checked_div(self.numerator as u128 * 1_000_000_000))
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| {
                Error::InvalidMedia("seek timestamp is outside the stream range".into())
            })?;
        Ok(ticks)
    }

    fn elapsed(self, pts: i64, start: i64) -> Option<Duration> {
        let ticks = i128::from(pts) - i128::from(start);
        if ticks < 0 || self.numerator <= 0 || self.denominator <= 0 {
            return None;
        }
        let nanos =
            ticks as u128 * self.numerator as u128 * 1_000_000_000 / self.denominator as u128;
        Some(Duration::new(
            u64::try_from(nanos / 1_000_000_000).ok()?,
            (nanos % 1_000_000_000) as u32,
        ))
    }

    fn as_ffmpeg(self) -> ffmpeg::Rational {
        ffmpeg::Rational(self.numerator, self.denominator)
    }
}

/// Container stream type, independent of the camera or codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StreamKind {
    /// Encoded video, including individual fisheye lenses.
    Video,
    /// Encoded audio.
    Audio,
    /// Timecode, telemetry, or other data carried as packets.
    Data,
    /// Encoded subtitles.
    Subtitle,
    /// An attached resource.
    Attachment,
    /// Unrecognized media type.
    Unknown,
}

/// Owned stream description. Timestamp integers use this stream's time base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Zero-based input index in the ordered [`InputSet`].
    pub input_index: usize,
    /// Original stream index within that input, including non-video streams.
    pub stream_index: usize,
    /// Container media type.
    pub kind: StreamKind,
    /// Codec name reported by the demuxer.
    pub codec: String,
    /// Numeric FFmpeg codec identifier, retained without exposing FFmpeg types.
    pub codec_id: i32,
    /// Unit for source PTS, DTS, start time, and duration.
    pub time_base: StreamTimeBase,
    /// Original stream start time, if present; never rebased.
    pub start_time: Option<i64>,
    /// Declared stream duration in ticks, if present.
    pub duration: Option<i64>,
    /// Declared coded video width, or zero for other stream kinds.
    pub width: u32,
    /// Declared coded video height, or zero for other stream kinds.
    pub height: u32,
    /// Original codec configuration bytes needed to interpret encoded packets.
    pub codec_extradata: Vec<u8>,
}

/// Recording stream inventory with no persistent decoder or read cursor.
///
/// Each reader opened from a stream owns an independent file/demuxer cursor, so
/// playback, frame extraction, and packet copying can run concurrently.
/// Original files must remain available and unchanged while descriptors are used.
///
/// ```no_run
/// use insta360_rs::{InputSet, MediaSource};
/// use std::time::Duration;
/// # fn example() -> insta360_rs::Result<()> {
/// let source = MediaSource::open(InputSet::discover("recording.insv")?)?;
/// if let Some(video) = source.video_streams().next() {
///     let mut packets = video.open_packets()?;
///     let first_original_packet = packets.read_packet()?;
///     let mut frames = video.open_video()?;
///     let preview = frames.frame_at(Duration::from_secs(10))?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct MediaSource {
    streams: Vec<MediaStream>,
}

impl MediaSource {
    /// Inspects every stream in the supplied files without requiring INSV metadata.
    pub fn open(inputs: InputSet) -> Result<Self> {
        let mut streams = Vec::new();
        for (input_index, path) in inputs.paths().iter().enumerate() {
            let path =
                std::fs::canonicalize(path).map_err(|error| crate::error::io_error(path, error))?;
            let input = open_input(&path)?;
            for stream in input.streams() {
                streams.push(MediaStream {
                    path: path.clone(),
                    info: Arc::new(stream_info(input_index, &stream)?),
                });
            }
        }
        Ok(Self { streams })
    }

    /// Returns all streams in input order and then original stream-index order.
    pub fn streams(&self) -> &[MediaStream] {
        &self.streams
    }

    /// Returns video streams without changing their original stream indexes.
    pub fn video_streams(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.info.kind == StreamKind::Video)
    }
}

/// Cloneable reference to one original file stream; it holds no read cursor.
#[derive(Clone, Debug)]
pub struct MediaStream {
    path: PathBuf,
    info: Arc<StreamInfo>,
}

impl MediaStream {
    /// Returns the stream's owned metadata and original codec configuration.
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Returns the canonical path to the original input file.
    pub fn source_path(&self) -> &Path {
        &self.path
    }

    /// Opens an independent reader for original encoded packets of this stream.
    pub fn open_packets(&self) -> Result<PacketReader> {
        let input = open_input(&self.path)?;
        let selected = input.stream(self.info.stream_index).ok_or_else(|| {
            Error::InvalidMedia("selected stream no longer exists in the input".into())
        })?;
        if stream_info(self.info.input_index, &selected)? != *self.info {
            return Err(Error::InvalidMedia(
                "stream metadata changed since opening the source".into(),
            ));
        }
        Ok(PacketReader {
            input,
            info: Arc::clone(&self.info),
            eof: false,
        })
    }

    /// Opens a software decoder for RGB24 frames without writing intermediate files.
    ///
    /// This explicit preview conversion reduces higher-bit-depth input to 8-bit
    /// RGB. It applies supported YUV matrices and the recorded range, using
    /// BT.601 when the matrix is unspecified. Unsupported declared matrices
    /// fail explicitly. It does not stitch, rotate, stabilize, tone-map HDR,
    /// or apply I-Log conversion. Use [`Self::open_packets`]
    /// to retain the original encoded representation and bit depth.
    pub fn open_video(&self) -> Result<VideoFrameReader> {
        if self.info.kind != StreamKind::Video {
            return Err(Error::InvalidMedia("selected stream is not video".into()));
        }
        rgb_size(self.info.width, self.info.height)?;
        let packets = self.open_packets()?;
        let stream = packets
            .input
            .stream(self.info.stream_index)
            .ok_or_else(|| Error::InvalidMedia("selected video stream is missing".into()))?;
        let mut context = ffmpeg::codec::Context::from_parameters(stream.parameters())
            .map_err(|error| media_error("creating video decoder", error))?;
        context.set_threading(ffmpeg::codec::threading::Config::kind(
            ffmpeg::codec::threading::Type::Frame,
        ));
        // SAFETY: the unopened codec context is uniquely owned. FFmpeg enforces
        // this limit before allocating decoded frame buffers, including changes
        // of coded resolution inside a stream.
        unsafe {
            (*context.as_mut_ptr()).max_pixels = MAX_FRAME_PIXELS as i64;
        }
        let mut decoder = context.decoder();
        decoder.set_packet_time_base(self.info.time_base.as_ffmpeg());
        let decoder = decoder
            .video()
            .map_err(|error| media_error("opening video decoder", error))?;
        Ok(VideoFrameReader {
            packets,
            decoder,
            pending: None,
            eof_sent: false,
            finished: false,
            send_blocked: false,
            discard_before: None,
            scaler: None,
        })
    }
}

/// Original packet side-data bytes and their numeric FFmpeg type identifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSideData {
    /// Numeric packet side-data type, including types unknown to this crate.
    pub kind: i32,
    /// Original side-data bytes.
    pub data: Vec<u8>,
}

/// One owned demuxed packet, with original timing and no codec transformations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedPacket {
    /// Input index from the original [`InputSet`].
    pub input_index: usize,
    /// Original per-file stream index.
    pub stream_index: usize,
    /// Encoded payload, copied directly from the demuxer.
    pub data: Vec<u8>,
    /// Original presentation timestamp in stream ticks.
    pub pts: Option<i64>,
    /// Original decoding timestamp in stream ticks, possibly negative.
    pub dts: Option<i64>,
    /// Original packet duration in ticks; zero means unknown.
    pub duration: i64,
    /// Time base used by packet timestamp fields.
    pub time_base: StreamTimeBase,
    /// All original AVPacket flag bits, including unrecognized flags.
    pub flags: i32,
    /// Whether the packet is marked as a keyframe.
    pub key_frame: bool,
    /// Whether the demuxer marked the packet corrupt.
    pub corrupt: bool,
    /// Original file byte position, if provided by the demuxer.
    pub source_position: Option<i64>,
    /// Original packet side data, including skip-sample or codec-update data.
    pub side_data: Vec<StreamSideData>,
}

/// Independent encoded-packet cursor for one stream in the original file.
pub struct PacketReader {
    input: ffmpeg::format::context::Input,
    info: Arc<StreamInfo>,
    eof: bool,
}

impl std::fmt::Debug for PacketReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PacketReader")
            .field("input_index", &self.info.input_index)
            .field("stream_index", &self.info.stream_index)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl PacketReader {
    /// Returns the selected stream's description.
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Reads the next packet for this stream; other streams are skipped.
    ///
    /// Returns `None` only for clean end-of-file. Demuxer and underlying I/O
    /// failures remain errors, including I/O errors reported alongside EOF.
    pub fn read_packet(&mut self) -> Result<Option<EncodedPacket>> {
        let Some(packet) = self.read_native()? else {
            return Ok(None);
        };
        copy_packet(&packet, &self.info).map(Some)
    }

    /// Seeks backward to a keyframe at or before a stream-relative time.
    ///
    /// Packets retain original PTS/DTS and can precede the requested time. Seek
    /// verifies video preroll in presentation order, including reordered B-frames,
    /// and clears EOF. Unknown stream start time is treated as zero.
    pub fn seek(&mut self, position: Duration) -> Result<()> {
        let timestamp = self
            .info
            .time_base
            .ticks(position)?
            .checked_add(self.info.start_time.unwrap_or(0))
            .ok_or_else(|| Error::InvalidMedia("seek timestamp overflow".into()))?;
        if self.info.kind == StreamKind::Video {
            seek_before_presentation(&mut self.input, &[(self.info.stream_index, timestamp)])?;
        } else {
            seek_stream(&mut self.input, self.info.stream_index, timestamp)?;
        }
        self.eof = false;
        Ok(())
    }

    fn read_native(&mut self) -> Result<Option<ffmpeg::Packet>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            let Some(packet) = read_checked_packet(&mut self.input)? else {
                self.eof = true;
                return Ok(None);
            };
            if packet.stream() != self.info.stream_index {
                continue;
            }
            return Ok(Some(packet));
        }
    }
}

/// Seeks to indexed preroll whose first video packets precede their requested
/// presentation timestamps. Each target uses its own stream's native time base.
/// The demuxer is rewound after inspection so callers retain every original packet.
pub(crate) fn seek_before_presentation(
    input: &mut ffmpeg::format::context::Input,
    targets: &[(usize, i64)],
) -> Result<()> {
    let &(reference_index, mut timestamp) = targets
        .first()
        .ok_or_else(|| Error::InvalidMedia("seek requires a video stream".into()))?;
    let time_bases = targets
        .iter()
        .map(|&(index, _)| {
            input
                .stream(index)
                .map(|stream| stream.time_base())
                .ok_or_else(|| Error::InvalidMedia("seek stream does not exist".into()))
        })
        .collect::<Result<Vec<_>>>()?;

    loop {
        seek_stream(input, reference_index, timestamp)?;
        let mut seen = vec![false; targets.len()];
        let mut earlier = None;
        while seen.iter().any(|seen| !seen) {
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(input) {
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => {
                    check_io(input)?;
                    break;
                }
                Err(error) => return Err(media_error("reading seek preroll", error)),
            }
            let Some(index) = targets
                .iter()
                .position(|&(index, _)| index == packet.stream())
            else {
                continue;
            };
            if seen[index] {
                continue;
            }
            validate_packet(&packet)?;
            seen[index] = true;
            if packet.pts().is_some_and(|pts| pts > targets[index].1) {
                // Container indexes can use DTS: a keyframe decoded before the
                // target may be presented after it, skipping preceding B-frames.
                // Exclude that keyframe and retry the previous indexed position.
                earlier = packet
                    .dts()
                    .map(|dts| dts.rescale(time_bases[index], time_bases[0]))
                    .and_then(|dts| dts.checked_sub(1))
                    .filter(|&previous| previous < timestamp);
                if earlier.is_none() {
                    return Err(Error::MissingCapability(
                        "cannot locate video preroll before the requested presentation time".into(),
                    ));
                }
                break;
            }
        }
        if let Some(previous) = earlier {
            timestamp = previous;
        } else {
            // Inspecting consumes packets from every interleaved stream. Seeking
            // again retains their order without buffering or decoding the GOP.
            return seek_stream(input, reference_index, timestamp);
        }
    }
}

fn seek_stream(
    input: &mut ffmpeg::format::context::Input,
    stream_index: usize,
    timestamp: i64,
) -> Result<()> {
    check_io(input)?;
    input.clear_eof();
    // SAFETY: input is exclusively borrowed and stream_index is checked by the
    // callers. No ANY flag is set, so the demuxer retains keyframe preroll.
    let status = unsafe {
        ffmpeg::ffi::avformat_seek_file(
            input.as_mut_ptr(),
            stream_index as i32,
            i64::MIN,
            timestamp,
            timestamp,
            0,
        )
    };
    if status < 0 {
        return Err(media_error(
            "seeking media stream",
            ffmpeg::Error::from(status),
        ));
    }
    Ok(())
}

/// One decoded preview frame: tightly packed RGB24, with no row padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedVideoFrame {
    /// Row-major RGB bytes; length is exactly `width * height * 3`.
    pub data: Vec<u8>,
    /// Decoded width, which can change during the recording.
    pub width: u32,
    /// Decoded height, which can change during the recording.
    pub height: u32,
    /// Presentation time relative to stream start (zero when start is absent).
    /// Missing timestamps and negative preroll timestamps are represented by `None`.
    pub timestamp: Option<Duration>,
    /// Original best-effort presentation timestamp, with frame PTS as fallback.
    pub pts: Option<i64>,
    /// Time base for the original `pts` field.
    pub time_base: StreamTimeBase,
}

/// Independent software video decoder for sequential playback or seeking.
///
/// B-frame delay is drained at EOF. Seeking flushes the decoder and discards
/// preroll frames until the first frame at or after the exact requested time.
pub struct VideoFrameReader {
    packets: PacketReader,
    decoder: ffmpeg::codec::decoder::Video,
    pending: Option<ffmpeg::Packet>,
    eof_sent: bool,
    finished: bool,
    send_blocked: bool,
    discard_before: Option<Duration>,
    scaler: Option<RgbScaler>,
}

impl std::fmt::Debug for VideoFrameReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoFrameReader")
            .field("input_index", &self.packets.info.input_index)
            .field("stream_index", &self.packets.info.stream_index)
            .field("finished", &self.finished)
            .field("discard_before", &self.discard_before)
            .finish_non_exhaustive()
    }
}

impl VideoFrameReader {
    /// Returns the selected stream's description.
    pub fn info(&self) -> &StreamInfo {
        self.packets.info()
    }

    /// Decodes the next frame in presentation order, returning `None` at clean EOF.
    pub fn read_frame(&mut self) -> Result<Option<DecodedVideoFrame>> {
        if self.finished {
            return Ok(None);
        }
        loop {
            let mut frame = ffmpeg::frame::Video::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    if frame.is_corrupt() || frame.has_decode_errors() {
                        return Err(Error::InvalidMedia("decoded video frame is corrupt".into()));
                    }
                    self.send_blocked = false;
                    let pts = frame.timestamp().or_else(|| frame.pts());
                    let info = self.packets.info();
                    let timestamp = pts.and_then(|value| {
                        info.time_base.elapsed(value, info.start_time.unwrap_or(0))
                    });
                    if let Some(target) = self.discard_before {
                        if pts.is_none() {
                            return Err(Error::MissingCapability(
                                "cannot seek accurately in video without presentation timestamps"
                                    .into(),
                            ));
                        }
                        if timestamp.is_none_or(|timestamp| timestamp < target) {
                            continue;
                        }
                        self.discard_before = None;
                    }
                    let data = convert_rgb(&mut self.scaler, &frame)?;
                    return Ok(Some(DecodedVideoFrame {
                        data,
                        width: frame.width(),
                        height: frame.height(),
                        timestamp,
                        pts,
                        time_base: self.packets.info.time_base,
                    }));
                }
                Err(ffmpeg::Error::Eof) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(error) if is_again(error) => {
                    // FFmpeg's send/receive contract forbids both directions
                    // being blocked at once. Report a broken state instead of
                    // losing a packet or spinning indefinitely.
                    if self.send_blocked || self.eof_sent {
                        return Err(Error::Media(
                            "video decoder made no progress while draining".into(),
                        ));
                    }
                }
                Err(error) => return Err(media_error("receiving video frame", error)),
            }
            if self.pending.is_none() {
                self.pending = self.packets.read_native()?;
            }
            if let Some(packet) = self.pending.as_ref() {
                match self.decoder.send_packet(packet) {
                    Ok(()) => {
                        self.pending = None;
                    }
                    Err(error) if is_again(error) => {
                        self.send_blocked = true;
                    }
                    Err(error) => return Err(media_error("sending video packet", error)),
                }
            } else {
                match self.decoder.send_eof() {
                    Ok(()) => {
                        self.eof_sent = true;
                    }
                    Err(error) if is_again(error) => {
                        self.send_blocked = true;
                    }
                    Err(ffmpeg::Error::Eof) => {
                        self.finished = true;
                        return Ok(None);
                    }
                    Err(error) => return Err(media_error("draining video decoder", error)),
                }
            }
        }
    }

    /// Seeks to a stream-relative time and flushes all queued decoded frames.
    ///
    /// The next read returns the first timestamped frame at or after `position`,
    /// or `None` when the requested time is past the last frame. Missing frame
    /// timestamps are an error while locating the requested position.
    pub fn seek(&mut self, position: Duration) -> Result<()> {
        self.packets.seek(position)?;
        self.decoder.flush();
        self.pending = None;
        self.eof_sent = false;
        self.finished = false;
        self.send_blocked = false;
        self.discard_before = Some(position);
        Ok(())
    }

    /// Seeks, then returns the first frame at or after the requested time.
    pub fn frame_at(&mut self, position: Duration) -> Result<Option<DecodedVideoFrame>> {
        self.seek(position)?;
        self.read_frame()
    }
}

struct RgbScaler {
    context: ffmpeg::software::scaling::Context,
    matrix: ffmpeg::util::color::Space,
    range: ffmpeg::util::color::Range,
}

// SAFETY: this scaler owns its SwsContext, retains no borrowed frame pointers,
// and is used only through &mut access. Moving ownership between worker threads
// does not allow concurrent access to libswscale state. It is intentionally !Sync.
unsafe impl Send for RgbScaler {}

pub(crate) fn open_input(path: &Path) -> Result<ffmpeg::format::context::Input> {
    ffmpeg::init().map_err(|error| media_error("initializing stream reader", error))?;
    let file = File::open(path).map_err(|error| crate::error::io_error(path, error))?;
    let io = ffmpeg::format::context::StreamIo::from_read_seek(file)
        .map_err(|error| media_error("opening seekable input", error))?;
    let mut options = ffmpeg::Dictionary::new();
    options.set("max_streams", &MAX_STREAMS.to_string());
    options.set("probesize", "33554432");
    options.set("analyzeduration", "10000000");
    options.set("enable_drefs", "0");
    options.set("protocol_whitelist", "file");
    options.set("format_whitelist", "mov");
    let input = ffmpeg::format::input_from_stream(io, None, Some(options))
        .map_err(|error| media_error("opening media stream", error))?;
    if input.nb_streams() as usize > MAX_STREAMS {
        return Err(Error::InvalidMedia("too many media streams".into()));
    }
    check_io(&input)?;
    Ok(input)
}

/// Unlike FFmpeg's packet iterator, errors are never mistaken for clean EOF.
pub(crate) fn read_checked_packet(
    input: &mut ffmpeg::format::context::Input,
) -> Result<Option<ffmpeg::Packet>> {
    let mut packet = ffmpeg::Packet::empty();
    match packet.read(input) {
        Ok(()) => {}
        Err(ffmpeg::Error::Eof) => {
            check_io(input)?;
            return Ok(None);
        }
        Err(error) => return Err(media_error("reading stream packet", error)),
    }
    check_io(input)?;
    if packet.stream() >= input.nb_streams() as usize {
        return Err(Error::InvalidMedia(
            "packet refers to an unknown stream".into(),
        ));
    }
    validate_packet(&packet)?;
    Ok(Some(packet))
}

fn stream_info(input_index: usize, stream: &ffmpeg::Stream<'_>) -> Result<StreamInfo> {
    let time_base = StreamTimeBase {
        numerator: stream.time_base().numerator(),
        denominator: stream.time_base().denominator(),
    }
    .validate()?;
    let parameters = stream.parameters();
    let kind = match parameters.medium() {
        ffmpeg::media::Type::Video => StreamKind::Video,
        ffmpeg::media::Type::Audio => StreamKind::Audio,
        ffmpeg::media::Type::Data => StreamKind::Data,
        ffmpeg::media::Type::Subtitle => StreamKind::Subtitle,
        ffmpeg::media::Type::Attachment => StreamKind::Attachment,
        ffmpeg::media::Type::Unknown => StreamKind::Unknown,
    };
    // SAFETY: codec parameters borrow the live stream. copy_bytes checks the
    // size and pointer before copying extradata; no borrowed data escapes.
    let raw = unsafe { &*parameters.as_ptr() };
    let extra_size = usize::try_from(raw.extradata_size)
        .map_err(|_| Error::InvalidMedia("negative codec extradata size".into()))?;
    let codec_extradata = unsafe { copy_bytes(raw.extradata, extra_size, MAX_SIDE_DATA_BYTES)? };
    Ok(StreamInfo {
        input_index,
        stream_index: stream.index(),
        kind,
        codec: parameters.id().name().to_owned(),
        codec_id: raw.codec_id as i32,
        time_base,
        start_time: known_timestamp(stream.start_time()),
        duration: known_timestamp(stream.duration()),
        width: u32::try_from(raw.width)
            .map_err(|_| Error::InvalidMedia("negative stream width".into()))?,
        height: u32::try_from(raw.height)
            .map_err(|_| Error::InvalidMedia("negative stream height".into()))?,
        codec_extradata,
    })
}

fn validate_packet(packet: &ffmpeg::Packet) -> Result<()> {
    // SAFETY: packet owns its AVPacket and every side-data allocation. Lengths
    // and pointer validity are checked before constructing any raw slices.
    let raw = unsafe { &*packet.as_ptr() };
    if raw.size < 0 || raw.size as usize > MAX_PACKET_BYTES || (raw.size > 0 && raw.data.is_null())
    {
        return Err(Error::InvalidMedia(
            "invalid or oversized encoded packet".into(),
        ));
    }
    if raw.side_data_elems < 0
        || raw.side_data_elems as usize > MAX_SIDE_DATA_ENTRIES
        || (raw.side_data_elems > 0 && raw.side_data.is_null())
    {
        return Err(Error::InvalidMedia("invalid packet side-data count".into()));
    }
    let mut total = 0_usize;
    for side in packet.side_data() {
        // SAFETY: the iterator indexes the checked, packet-owned array.
        let raw = unsafe { &*side.as_ptr() };
        total = total
            .checked_add(raw.size)
            .ok_or_else(|| Error::InvalidMedia("packet side-data size overflow".into()))?;
        if total > MAX_SIDE_DATA_BYTES || (raw.size > 0 && raw.data.is_null()) {
            return Err(Error::InvalidMedia(
                "invalid or oversized packet side data".into(),
            ));
        }
    }
    Ok(())
}

fn copy_packet(packet: &ffmpeg::Packet, info: &StreamInfo) -> Result<EncodedPacket> {
    validate_packet(packet)?;
    // SAFETY: validate_packet checked lengths and pointers in this owned packet.
    // These slices are copied while the packet remains alive.
    let raw = unsafe { &*packet.as_ptr() };
    let data = unsafe { copy_bytes(raw.data, raw.size as usize, MAX_PACKET_BYTES)? };
    let mut side_data = Vec::with_capacity(raw.side_data_elems as usize);
    for side in packet.side_data() {
        // SAFETY: side borrows packet, and its length/pointer were checked above.
        let side = unsafe { &*side.as_ptr() };
        side_data.push(StreamSideData {
            kind: side.type_ as i32,
            data: unsafe { copy_bytes(side.data, side.size, MAX_SIDE_DATA_BYTES)? },
        });
    }
    Ok(EncodedPacket {
        input_index: info.input_index,
        stream_index: info.stream_index,
        data,
        pts: packet.pts(),
        dts: packet.dts(),
        duration: packet.duration(),
        time_base: info.time_base,
        flags: raw.flags,
        key_frame: packet.is_key(),
        corrupt: packet.is_corrupt(),
        source_position: (raw.pos >= 0).then_some(raw.pos),
        side_data,
    })
}

// SAFETY: pointer must refer to a live FFmpeg-owned allocation of at least size
// bytes, or may be null when size is zero. Callers retain that owner for the copy.
unsafe fn copy_bytes(pointer: *const u8, size: usize, limit: usize) -> Result<Vec<u8>> {
    if size > limit || (size > 0 && pointer.is_null()) {
        return Err(Error::InvalidMedia(
            "invalid or oversized stream byte buffer".into(),
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| Error::Media("allocating stream byte buffer".into()))?;
    if size > 0 {
        // SAFETY: caller guarantees allocation lifetime; size and null checks above.
        bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(pointer, size) });
    }
    Ok(bytes)
}

fn check_io(input: &ffmpeg::format::context::Input) -> Result<()> {
    // SAFETY: the live input owns its AVIOContext; scalar error state is read only.
    let error = unsafe {
        let io = (*input.as_ptr()).pb;
        if io.is_null() {
            0
        } else {
            (*io).error
        }
    };
    if error < 0 && error != ffmpeg::ffi::AVERROR_EOF {
        return Err(media_error(
            "reading original media file",
            ffmpeg::Error::from(error),
        ));
    }
    Ok(())
}

fn rgb_size(width: u32, height: u32) -> Result<usize> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || pixels > MAX_FRAME_PIXELS
    {
        return Err(Error::InvalidMedia(
            "video frame dimensions exceed supported bounds".into(),
        ));
    }
    usize::try_from(pixels * 3)
        .map_err(|_| Error::InvalidMedia("RGB frame allocation size overflow".into()))
}

pub(crate) fn allocate_video_frame(
    format: ffmpeg::format::Pixel,
    width: u32,
    height: u32,
) -> Result<ffmpeg::frame::Video> {
    rgb_size(width, height)?;
    let mut frame = ffmpeg::frame::Video::empty();
    frame.set_format(format);
    frame.set_width(width);
    frame.set_height(height);
    // SAFETY: the frame is uniquely owned, dimensions are bounded, and allocation
    // failure is checked before any buffer access or scaling.
    let status = unsafe { ffmpeg::ffi::av_frame_get_buffer(frame.as_mut_ptr(), 32) };
    if status < 0 {
        return Err(media_error(
            "allocating video frame",
            ffmpeg::Error::from(status),
        ));
    }
    Ok(frame)
}

pub(crate) fn scale_video_frame(
    scaler: &mut ffmpeg::software::scaling::Context,
    source: &ffmpeg::frame::Video,
    destination: &mut ffmpeg::frame::Video,
) -> Result<()> {
    let input = scaler.input();
    let output = scaler.output();
    if (input.format, input.width, input.height)
        != (source.format(), source.width(), source.height())
        || (output.format, output.width, output.height)
            != (
                destination.format(),
                destination.width(),
                destination.height(),
            )
    {
        return Err(Error::InvalidMedia(
            "video dimensions changed during conversion".into(),
        ));
    }
    // SAFETY: source is a live decoder/allocated frame, destination owns its
    // checked allocation, and the scaler definitions match both frames. Scaling
    // is synchronous and retains no pointers. Check the return value discarded
    // by ffmpeg-next's Context::run.
    let rows = unsafe {
        ffmpeg::ffi::sws_scale(
            scaler.as_mut_ptr(),
            (*source.as_ptr()).data.as_ptr() as *const *const u8,
            (*source.as_ptr()).linesize.as_ptr(),
            0,
            source.height() as i32,
            (*destination.as_mut_ptr()).data.as_ptr(),
            (*destination.as_mut_ptr()).linesize.as_ptr(),
        )
    };
    if rows != destination.height() as i32 {
        return Err(Error::Media(format!(
            "video conversion produced {rows} rows instead of {}",
            destination.height()
        )));
    }
    Ok(())
}

fn convert_rgb(scaler: &mut Option<RgbScaler>, frame: &ffmpeg::frame::Video) -> Result<Vec<u8>> {
    let size = rgb_size(frame.width(), frame.height())?;
    if scaler.as_ref().is_none_or(|scaler| {
        let input = scaler.context.input();
        input.format != frame.format()
            || input.width != frame.width()
            || input.height != frame.height()
            || scaler.matrix != frame.color_space()
            || scaler.range != frame.color_range()
    }) {
        let mut context = ffmpeg::software::scaling::Context::get(
            frame.format(),
            frame.width(),
            frame.height(),
            ffmpeg::format::Pixel::RGB24,
            frame.width(),
            frame.height(),
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .map_err(|error| media_error("creating RGB preview converter", error))?;
        let matrix = preview_matrix(frame.color_space())?;
        let full_range = frame.color_range() == ffmpeg::util::color::Range::JPEG
            || matches!(
                frame.format(),
                ffmpeg::format::Pixel::YUVJ420P
                    | ffmpeg::format::Pixel::YUVJ422P
                    | ffmpeg::format::Pixel::YUVJ444P
            );
        // SAFETY: context is uniquely owned; FFmpeg copies the static coefficient
        // tables and retains no frame or Rust buffer pointers during this call.
        let status = unsafe {
            let coefficients = ffmpeg::ffi::sws_getCoefficients(matrix);
            ffmpeg::ffi::sws_setColorspaceDetails(
                context.as_mut_ptr(),
                coefficients,
                i32::from(full_range),
                coefficients,
                1,
                0,
                1 << 16,
                1 << 16,
            )
        };
        if status < 0 {
            return Err(media_error(
                "configuring RGB preview color",
                ffmpeg::Error::from(status),
            ));
        }
        *scaler = Some(RgbScaler {
            context,
            matrix: frame.color_space(),
            range: frame.color_range(),
        });
    }
    let mut rgb =
        allocate_video_frame(ffmpeg::format::Pixel::RGB24, frame.width(), frame.height())?;
    let scaler = scaler
        .as_mut()
        .ok_or_else(|| Error::Media("RGB preview converter was not initialized".into()))?;
    scale_video_frame(&mut scaler.context, frame, &mut rgb)?;
    let row_bytes = frame.width() as usize * 3;
    let stride = rgb.stride(0);
    if stride < row_bytes {
        return Err(Error::InvalidMedia(
            "RGB frame stride is shorter than a row".into(),
        ));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(size)
        .map_err(|_| Error::Media("allocating packed RGB preview".into()))?;
    for row in rgb
        .data(0)
        .chunks_exact(stride)
        .take(frame.height() as usize)
    {
        data.extend_from_slice(&row[..row_bytes]);
    }
    if data.len() != size {
        return Err(Error::InvalidMedia("RGB frame plane is truncated".into()));
    }
    Ok(data)
}

pub(crate) fn preview_matrix(space: ffmpeg::util::color::Space) -> Result<i32> {
    use ffmpeg::util::color::Space;
    match space {
        Space::BT709 => Ok(ffmpeg::ffi::SWS_CS_ITU709),
        Space::FCC => Ok(ffmpeg::ffi::SWS_CS_FCC),
        Space::SMPTE240M => Ok(ffmpeg::ffi::SWS_CS_SMPTE240M),
        Space::BT2020NCL => Ok(ffmpeg::ffi::SWS_CS_BT2020),
        Space::RGB | Space::Unspecified | Space::BT470BG | Space::SMPTE170M => {
            Ok(ffmpeg::ffi::SWS_CS_DEFAULT)
        }
        other => Err(Error::MissingCapability(format!(
            "RGB preview does not support the declared color matrix {other:?}; encoded packets remain available"
        ))),
    }
}

fn known_timestamp(value: i64) -> Option<i64> {
    (value != ffmpeg::ffi::AV_NOPTS_VALUE).then_some(value)
}
fn is_again(error: ffmpeg::Error) -> bool {
    matches!(error, ffmpeg::Error::Other { errno } if errno == ffmpeg::ffi::EAGAIN)
}
fn media_error(context: &str, error: ffmpeg::Error) -> Error {
    Error::Media(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_refuses_unsupported_declared_matrix_instead_of_changing_colors() {
        let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, 2, 2);
        frame.set_color_space(ffmpeg::util::color::Space::BT2020CL);
        assert!(matches!(
            convert_rgb(&mut None, &frame),
            Err(Error::MissingCapability(_))
        ));
    }

    #[test]
    fn timestamp_conversion_preserves_origin_and_fractional_precision() {
        let base = StreamTimeBase {
            numerator: 1,
            denominator: 90_000,
        };
        assert_eq!(
            base.elapsed(180_001, 90_000),
            Some(Duration::from_nanos(1_000_011_111))
        );
        assert_eq!(base.elapsed(89_999, 90_000), None);
        assert_eq!(
            base.ticks(Duration::from_nanos(1_000_011_111)).unwrap(),
            90_000
        );
        assert_eq!(base.ticks(Duration::from_millis(1_250)).unwrap(), 112_500);
    }

    #[test]
    fn timestamp_conversion_rejects_invalid_bases_and_overflow() {
        for base in [
            StreamTimeBase {
                numerator: 0,
                denominator: 1,
            },
            StreamTimeBase {
                numerator: 1,
                denominator: 0,
            },
            StreamTimeBase {
                numerator: -1,
                denominator: 1,
            },
        ] {
            assert!(base.ticks(Duration::ZERO).is_err());
        }
        let base = StreamTimeBase {
            numerator: 1,
            denominator: i32::MAX,
        };
        assert!(base.ticks(Duration::MAX).is_err());
        assert_eq!(base.elapsed(i64::MIN, i64::MAX), None);
    }

    #[test]
    fn frame_bounds_reject_zero_and_oversized_allocations() {
        assert_eq!(rgb_size(1920, 1080).unwrap(), 1920 * 1080 * 3);
        assert!(rgb_size(0, 1080).is_err());
        assert!(rgb_size(u32::MAX, u32::MAX).is_err());
        assert!(rgb_size(32_768, 32_768).is_err());
        assert!(allocate_video_frame(ffmpeg::format::Pixel::RGB24, 0, 1080).is_err());
        assert!(allocate_video_frame(ffmpeg::format::Pixel::RGB24, 32_768, 32_768).is_err());
        assert!(allocate_video_frame(ffmpeg::format::Pixel::None, 16, 16).is_err());
    }

    #[test]
    fn scaling_rejects_mismatched_frames_before_accessing_planes() {
        let mut scaler = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGB24,
            2,
            2,
            ffmpeg::format::Pixel::YUV420P,
            2,
            2,
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .unwrap();
        let rgb = allocate_video_frame(ffmpeg::format::Pixel::RGB24, 2, 2).unwrap();
        let mut wrong_size = allocate_video_frame(ffmpeg::format::Pixel::YUV420P, 4, 4).unwrap();
        assert!(matches!(
            scale_video_frame(&mut scaler, &rgb, &mut wrong_size),
            Err(Error::InvalidMedia(_))
        ));
    }

    #[test]
    fn readers_can_move_to_independent_worker_threads() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send::<PacketReader>();
        assert_send::<VideoFrameReader>();
        assert_send_sync::<MediaSource>();
        assert_send_sync::<MediaStream>();
    }

    #[test]
    fn encoded_packets_keep_original_payload_timing_flags_and_side_data() {
        use ffmpeg::codec::packet::Mut;
        let mut packet = ffmpeg::Packet::copy(&[0, 0, 0, 1, 0x65, 0xab]);
        packet.set_pts(Some(12));
        packet.set_dts(Some(-3));
        packet.set_duration(15);
        packet.set_position(42);
        packet.set_flags(ffmpeg::packet::Flags::KEY);
        // SAFETY: packet is uniquely owned; FFmpeg allocates the side-data bytes.
        let side = unsafe {
            ffmpeg::ffi::av_packet_new_side_data(
                packet.as_mut_ptr(),
                ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES,
                10,
            )
        };
        assert!(!side.is_null());
        unsafe {
            std::ptr::write_bytes(side, 0x7f, 10);
            (*packet.as_mut_ptr()).flags |= 1 << 25;
        }
        let info = StreamInfo {
            input_index: 1,
            stream_index: 2,
            kind: StreamKind::Video,
            codec: "h264".into(),
            codec_id: 27,
            time_base: StreamTimeBase {
                numerator: 1,
                denominator: 90_000,
            },
            start_time: Some(9),
            duration: Some(300),
            width: 32,
            height: 16,
            codec_extradata: Vec::new(),
        };
        let copied = copy_packet(&packet, &info).unwrap();
        drop(packet);
        assert_eq!(copied.data, &[0, 0, 0, 1, 0x65, 0xab]);
        assert_eq!(copied.input_index, 1);
        assert_eq!(copied.stream_index, 2);
        assert_eq!(copied.pts, Some(12));
        assert_eq!(copied.dts, Some(-3));
        assert_eq!(copied.duration, 15);
        assert_eq!(copied.source_position, Some(42));
        assert_eq!(copied.time_base, info.time_base);
        assert_eq!(copied.flags & (1 << 25), 1 << 25);
        assert!(copied.key_frame);
        assert_eq!(
            copied.side_data[0].kind,
            ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32
        );
        assert_eq!(copied.side_data[0].data, &[0x7f; 10]);
    }

    #[test]
    fn encoded_packet_validation_rejects_invalid_lengths_before_copying() {
        use ffmpeg::codec::packet::Mut;
        let mut packet = ffmpeg::Packet::empty();
        validate_packet(&packet).unwrap();
        // SAFETY: only the scalar size is changed; restoring it before drop
        // leaves the packet's owned allocation state unchanged.
        unsafe {
            (*packet.as_mut_ptr()).size = -1;
        }
        assert!(validate_packet(&packet).is_err());
        unsafe {
            (*packet.as_mut_ptr()).size = MAX_PACKET_BYTES as i32 + 1;
        }
        assert!(validate_packet(&packet).is_err());
        unsafe {
            (*packet.as_mut_ptr()).size = 0;
        }
    }
}
