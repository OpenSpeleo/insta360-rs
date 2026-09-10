//! Direct, bounded decoding of exact simultaneous lens pairs across chapters.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use crate::{Error, RecordingSequence, Result};

mod layout;
pub(crate) use layout::DecodedLayout;
mod preview;
pub use preview::{PairedPreviewReader, PreviewAcceleration};

const MAX_QUEUED_FRAMES: usize = 16;
const MAX_QUEUED_BYTES: usize = 256 * 1024 * 1024;
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;
const MAX_AUDIO_PACKETS: usize = 65536;

/// Native decoded lens frames sharing exactly the same rational presentation time.
/// Pixel buffers retain FFmpeg ownership; conversion is deferred to the consumer.
pub struct FramePair {
    pub a: ffmpeg::frame::Video,
    pub b: ffmpeg::frame::Video,
    pub timestamp_micros: i64,
    pub source_timestamp_micros: i64,
    pub chapter_index: usize,
    pub a_pts: i64,
    pub a_time_base: ffmpeg::Rational,
    pub b_pts: i64,
    pub b_time_base: ffmpeg::Rational,
}

/// Stable native identity for saving the exact pair currently displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramePairIdentity {
    pub chapter_index: usize,
    pub a_pts: i64,
    pub a_time_base: ffmpeg::Rational,
    pub b_pts: i64,
    pub b_time_base: ffmpeg::Rational,
}

impl FramePair {
    /// Returns an identity unaffected by display-time rounding.
    pub fn identity(&self) -> FramePairIdentity {
        FramePairIdentity {
            chapter_index: self.chapter_index,
            a_pts: self.a_pts,
            a_time_base: self.a_time_base,
            b_pts: self.b_pts,
            b_time_base: self.b_time_base,
        }
    }
}

/// Bounded native decoding from dual-track files, simultaneous legacy lens files,
/// or explicitly identified side-by-side packed fisheye video.
pub struct PairedReader {
    sequence: RecordingSequence,
    chapter_index: usize,
    current: Option<ChapterReader>,
    start: Duration,
    audio_enabled: bool,
    audio_packets: Vec<(usize, ffmpeg::Packet)>,
    audio_bytes: usize,
    exact_start: Option<FramePairIdentity>,
}

impl PairedReader {
    /// Seeks in recording time, retaining decoder preroll but excluding it from output.
    /// A/B identity comes from declared track order, validated `_00_`/`_10_`
    /// filenames, or the calibrated left/right halves of a proven packed layout.
    pub fn open(sequence: &RecordingSequence, start: Duration) -> Result<Self> {
        ffmpeg::init().map_err(media)?;
        if start > sequence.duration {
            return Err(invalid("seek lies beyond the recording"));
        }
        let chapter_index = sequence
            .chapter_at(start)
            .unwrap_or(sequence.chapters.len());
        let mut reader = Self {
            sequence: sequence.clone(),
            chapter_index,
            current: None,
            start,
            audio_enabled: false,
            audio_packets: Vec::new(),
            audio_bytes: 0,
            exact_start: None,
        };
        reader.open_chapter()?;
        Ok(reader)
    }

    /// Opens the exact previously displayed pair using its native timestamp identity.
    /// The one-microsecond seek preroll prevents rounded display time from skipping it.
    pub fn open_at_pair(sequence: &RecordingSequence, identity: FramePairIdentity) -> Result<Self> {
        let chapter = sequence
            .chapters
            .get(identity.chapter_index)
            .ok_or_else(|| invalid("pair chapter is outside the recording"))?;
        for time_base in [identity.a_time_base, identity.b_time_base] {
            if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
                return Err(invalid("pair identity has an invalid time base"));
            }
        }
        if compare_pts(
            identity.a_pts,
            identity.a_time_base,
            identity.b_pts,
            identity.b_time_base,
        ) != 0
        {
            return Err(invalid("pair identity is not simultaneous"));
        }
        let mut reader = Self::open(sequence, chapter.timeline_start)?;
        let current = reader
            .current
            .as_mut()
            .ok_or_else(|| invalid("pair chapter has no decodable video"))?;
        let target = identity
            .a_pts
            .rescale(identity.a_time_base, (1, 1_000_000))
            .saturating_sub(1)
            .max(current.origin_micros);
        current.seek(target)?;
        current.discard_before =
            Duration::from_micros(target.saturating_sub(current.origin_micros) as u64);
        reader.exact_start = Some(identity);
        Ok(reader)
    }

    /// Enables original audio packet forwarding before the first read.
    pub fn enable_audio(&mut self) {
        self.audio_enabled = true;
    }

    /// Drains original audio packets with their chapter indexes and native timestamps.
    /// Stream indexes are chapter-wide: each input follows the preceding input
    ///'s complete stream table, so identically numbered tracks in lens files remain distinct.
    /// Call after every pair and after EOF to keep retained packet memory bounded.
    pub fn take_audio_packets(&mut self) -> Vec<(usize, ffmpeg::Packet)> {
        self.audio_bytes = 0;
        std::mem::take(&mut self.audio_packets)
    }

    /// Reads the bounded audio tail up to a clipped recording end without decoding video.
    /// Call after video has reached `end`, then drain the forwarded packet batch.
    pub fn finish_audio(&mut self, end: Duration, cancel: &AtomicBool) -> Result<()> {
        if !self.audio_enabled {
            return Ok(());
        }
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
        let chapter = &self.sequence.chapters[self.chapter_index];
        if end <= chapter.timeline_start {
            return Ok(());
        }
        for input_index in 0..current.inputs.len() {
            let mut pending = current.audio_streams[input_index].clone();
            while !pending.is_empty() && !current.input_eof[input_index] {
                check_cancel(cancel)?;
                let input = &mut current.inputs[input_index];
                let Some(mut packet) = crate::stream::read_checked_packet(input)? else {
                    current.input_eof[input_index] = true;
                    break;
                };
                if !current.audio_streams[input_index].contains(&packet.stream()) {
                    continue;
                }
                let stream = input
                    .stream(packet.stream())
                    .ok_or_else(|| invalid("audio stream disappeared"))?;
                let pts = packet
                    .pts()
                    .or_else(|| packet.dts())
                    .ok_or_else(|| invalid("audio packet has no timestamp"))?;
                let local = pts
                    .rescale(stream.time_base(), (1, 1_000_000))
                    .checked_sub(current.origin_micros)
                    .ok_or_else(|| invalid("audio timestamp overflow"))?;
                let global = micros(chapter.timeline_start)?
                    .checked_add(local)
                    .ok_or_else(|| invalid("audio timestamp overflow"))?;
                if global >= micros(end)? {
                    pending.retain(|index| *index != packet.stream());
                }
                packet.set_stream(current.stream_offsets[input_index] + packet.stream());
                retain_audio(
                    &mut self.audio_packets,
                    &mut self.audio_bytes,
                    self.chapter_index,
                    packet,
                )?;
            }
        }
        Ok(())
    }

    /// Returns the next exact lens pair; missing/duplicate/unmatched timestamps fail.
    pub fn next_pair(&mut self, cancel: &AtomicBool) -> Result<Option<FramePair>> {
        loop {
            check_cancel(cancel)?;
            let Some(current) = self.current.as_mut() else {
                return Ok(None);
            };
            if let Some((a, b)) = current.take_pair()? {
                let chapter = &self.sequence.chapters[self.chapter_index];
                let a_pts = a
                    .pts()
                    .ok_or_else(|| invalid("camera A frame has no presentation timestamp"))?;
                let b_pts = b
                    .pts()
                    .ok_or_else(|| invalid("camera B frame has no presentation timestamp"))?;
                let a_time_base = current.decoders[0].time_base;
                let b_time_base = current.camera_decoder(1).time_base;
                if let Some(identity) = self.exact_start {
                    if self.chapter_index != identity.chapter_index {
                        return Err(invalid(
                            "the displayed pair no longer exists in its chapter",
                        ));
                    }
                    let order =
                        compare_pts(a_pts, a_time_base, identity.a_pts, identity.a_time_base);
                    if order < 0 {
                        continue;
                    }
                    if order > 0
                        || compare_pts(b_pts, b_time_base, identity.b_pts, identity.b_time_base)
                            != 0
                    {
                        return Err(invalid("the displayed pair no longer exists in the source"));
                    }
                    self.exact_start = None;
                }
                let source_timestamp_micros = a_pts.rescale(a_time_base, (1, 1_000_000));
                let relative =
                    relative_timestamp_micros(a_pts, current.decoders[0].start_pts, a_time_base)?;
                let global = micros(chapter.timeline_start)?
                    .checked_add(relative)
                    .ok_or_else(|| invalid("recording timestamp overflow"))?;
                if global < micros(self.start)? {
                    continue;
                }
                return Ok(Some(FramePair {
                    a,
                    b,
                    timestamp_micros: global,
                    source_timestamp_micros,
                    chapter_index: self.chapter_index,
                    a_pts,
                    a_time_base,
                    b_pts,
                    b_time_base,
                }));
            }
            if current.input_eof.iter().all(|eof| *eof) {
                if current.queues.iter().any(|queue| !queue.is_empty()) {
                    return Err(invalid("chapter ended with an unmatched camera frame"));
                }
                if self.exact_start.is_some() {
                    return Err(invalid(
                        "the displayed pair is beyond the available chapter frames",
                    ));
                }
                self.chapter_index += 1;
                self.open_chapter()?;
                continue;
            }
            // With separate files, read the lens whose queue is empty first.
            // This keeps demux scheduling bounded even for very different GOPs.
            let input_index = current.next_input();
            let Some(mut packet) =
                crate::stream::read_checked_packet(&mut current.inputs[input_index])?
            else {
                for index in 0..current.decoders.len() {
                    if current.decoders[index].input == input_index {
                        check_cancel(cancel)?;
                        current.decoders[index].decoder.send_eof().map_err(media)?;
                        current.receive(index, cancel)?;
                    }
                }
                current.input_eof[input_index] = true;
                continue;
            };
            if let Some(index) = current.decoders.iter().position(|decoder| {
                decoder.input == input_index && decoder.index == packet.stream()
            }) {
                if packet.is_corrupt() {
                    return Err(invalid("video packet is marked corrupt"));
                }
                current.decode(index, &packet, cancel)?;
            } else if self.audio_enabled
                && current.audio_streams[input_index].contains(&packet.stream())
            {
                packet.set_stream(current.stream_offsets[input_index] + packet.stream());
                retain_audio(
                    &mut self.audio_packets,
                    &mut self.audio_bytes,
                    self.chapter_index,
                    packet,
                )?;
            }
        }
    }

    fn open_chapter(&mut self) -> Result<()> {
        self.current = None;
        let Some(chapter) = self.sequence.chapters.get(self.chapter_index) else {
            return Ok(());
        };
        self.current = Some(ChapterReader::open(
            chapter,
            self.start.saturating_sub(chapter.timeline_start),
        )?);
        Ok(())
    }

    pub(crate) fn validate_chapter(chapter: &crate::RecordingChapter) -> Result<()> {
        ChapterReader::open(chapter, Duration::ZERO).map(|_| ())
    }
}

struct Decoder {
    input: usize,
    index: usize,
    time_base: ffmpeg::Rational,
    decoder: ffmpeg::codec::decoder::Video,
    last_pts: Option<i64>,
    start_pts: i64,
}

impl Decoder {
    fn open(input: &ffmpeg::format::context::Input, source: layout::LensSource) -> Result<Self> {
        let stream = input
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
            .nth(source.video)
            .ok_or_else(|| invalid("declared lens video stream is absent"))?;
        let time_base = stream.time_base();
        if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
            return Err(invalid("invalid video stream time base"));
        }
        let mut context =
            ffmpeg::codec::context::Context::from_parameters(stream.parameters()).map_err(media)?;
        context.set_threading(ffmpeg::codec::threading::Config {
            kind: ffmpeg::codec::threading::Type::Frame,
            count: 4,
        });
        unsafe {
            (*context.as_mut_ptr()).max_pixels = crate::stream::MAX_FRAME_PIXELS as i64;
        }
        Ok(Self {
            input: source.input,
            index: stream.index(),
            time_base,
            decoder: context.decoder().video().map_err(media)?,
            last_pts: None,
            start_pts: stream.start_time(),
        })
    }
}

fn retain_audio(
    packets: &mut Vec<(usize, ffmpeg::Packet)>,
    bytes: &mut usize,
    chapter: usize,
    packet: ffmpeg::Packet,
) -> Result<()> {
    let next = bytes
        .checked_add(packet.size())
        .ok_or_else(|| invalid("audio backlog overflow"))?;
    if next > MAX_AUDIO_BYTES || packets.len() >= MAX_AUDIO_PACKETS {
        return Err(invalid(
            "audio packet backlog exceeded its bound; drain packets after each pair",
        ));
    }
    *bytes = next;
    packets.push((chapter, packet));
    Ok(())
}

/// MP4 indexes use decode timestamps. A key packet before the requested DTS can
/// have a later presentation time, with intervening B frames still depending on
/// the previous GOP. Keep one earlier indexed keyframe for *each* lens, then
/// choose their common earliest seek point. If either index cannot prove that
/// preroll, retain the freshly opened demuxer at the beginning of the chapter.
fn seek_with_pair_preroll<'a>(
    input: &mut ffmpeg::format::context::Input,
    decoders: impl Iterator<Item = &'a Decoder>,
    target_micros: i64,
    origin_micros: i64,
) -> Result<()> {
    let mut anchor = target_micros;
    for decoder in decoders {
        let stream = input
            .stream(decoder.index)
            .ok_or_else(|| invalid("video stream disappeared"))?;
        let target = target_micros.rescale((1, 1_000_000), decoder.time_base);
        // SAFETY: the stream and its index entries remain owned by the input;
        // the index APIs neither retain these pointers nor mutate stream data.
        let previous = unsafe {
            let stream_ptr = stream.as_ptr().cast_mut();
            let entry = ffmpeg::ffi::avformat_index_get_entry_from_timestamp(
                stream_ptr,
                target,
                ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
            );
            if entry.is_null() {
                return Ok(());
            }
            let earlier = ffmpeg::ffi::avformat_index_get_entry_from_timestamp(
                stream_ptr,
                (*entry).timestamp.saturating_sub(1),
                ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
            );
            if earlier.is_null() {
                return Ok(());
            }
            (*earlier).timestamp
        };
        anchor = anchor.min(previous.rescale(decoder.time_base, (1, 1_000_000)));
    }
    if anchor > origin_micros {
        input
            .seek(anchor, ..anchor.saturating_add(1))
            .map_err(media)?;
    }
    Ok(())
}

struct ChapterReader {
    inputs: Vec<ffmpeg::format::context::Input>,
    decoders: Vec<Decoder>,
    packed: Option<(u32, u32)>,
    queues: [VecDeque<ffmpeg::frame::Video>; 2],
    queued_bytes: usize,
    input_eof: Vec<bool>,
    origin_micros: i64,
    discard_before: Duration,
    audio_streams: Vec<Vec<usize>>,
    stream_offsets: Vec<usize>,
}

impl ChapterReader {
    fn open(chapter: &crate::RecordingChapter, local: Duration) -> Result<Self> {
        let layout = DecodedLayout::inspect(chapter)?;
        let inputs = chapter
            .inputs
            .paths()
            .iter()
            .map(|path| crate::stream::open_input(path))
            .collect::<Result<Vec<_>>>()?;
        for (input_index, input) in inputs.iter().enumerate() {
            let expected = if inputs.len() == 1 && layout.packed.is_none() {
                2
            } else {
                1
            };
            if input
                .streams()
                .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
                .count()
                != expected
            {
                return Err(invalid(format!(
                    "input {input_index} does not contain the declared {expected} video streams"
                )));
            }
        }
        let [a_source, b_source] = layout.lenses;
        let a = Decoder::open(&inputs[a_source.input], a_source)?;
        let mut decoders = vec![a];
        if layout.packed.is_none() {
            decoders.push(Decoder::open(&inputs[b_source.input], b_source)?);
        }
        let a = &decoders[0];
        for decoder in &decoders {
            if decoder.start_pts == ffmpeg::ffi::AV_NOPTS_VALUE {
                return Err(invalid("video tracks do not declare a presentation origin"));
            }
            if compare_pts(
                a.start_pts,
                a.time_base,
                decoder.start_pts,
                decoder.time_base,
            ) != 0
            {
                return Err(invalid(
                    "camera tracks start at different presentation times",
                ));
            }
        }
        let origin_micros = a.start_pts.rescale(a.time_base, (1, 1_000_000));
        let seek_target = origin_micros
            .checked_add(micros(local)?)
            .ok_or_else(|| invalid("seek timestamp overflow"))?;
        let mut stream_offset = 0;
        let stream_offsets = inputs
            .iter()
            .map(|input| {
                let offset = stream_offset;
                stream_offset += input.nb_streams() as usize;
                offset
            })
            .collect();
        let audio_streams = inputs
            .iter()
            .map(|input| {
                input
                    .streams()
                    .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio)
                    .map(|stream| stream.index())
                    .collect()
            })
            .collect();
        let input_eof = vec![false; inputs.len()];
        let mut current = ChapterReader {
            inputs,
            decoders,
            packed: layout.packed,
            queues: [VecDeque::new(), VecDeque::new()],
            queued_bytes: 0,
            input_eof,
            origin_micros,
            discard_before: local,
            audio_streams,
            stream_offsets,
        };
        if !local.is_zero() {
            current.seek(seek_target)?;
        }
        Ok(current)
    }

    fn camera_decoder(&self, camera: usize) -> &Decoder {
        &self.decoders[if self.packed.is_some() { 0 } else { camera }]
    }

    fn seek(&mut self, target: i64) -> Result<()> {
        for (index, input) in self.inputs.iter_mut().enumerate() {
            seek_with_pair_preroll(
                input,
                self.decoders
                    .iter()
                    .filter(|decoder| decoder.input == index),
                target,
                self.origin_micros,
            )?;
        }
        Ok(())
    }

    fn next_input(&self) -> usize {
        for (camera, decoder) in self.decoders.iter().enumerate() {
            if self.queues[camera].is_empty() && !self.input_eof[decoder.input] {
                return decoder.input;
            }
        }
        self.input_eof
            .iter()
            .position(|eof| !eof)
            .expect("a chapter still has an open input")
    }

    fn decode(&mut self, index: usize, packet: &ffmpeg::Packet, cancel: &AtomicBool) -> Result<()> {
        check_cancel(cancel)?;
        match self.decoders[index].decoder.send_packet(packet) {
            Ok(()) => {}
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::ffi::EAGAIN => {
                self.receive(index, cancel)?;
                self.decoders[index]
                    .decoder
                    .send_packet(packet)
                    .map_err(media)?;
            }
            Err(error) => return Err(media(error)),
        }
        self.receive(index, cancel)
    }

    fn receive(&mut self, index: usize, cancel: &AtomicBool) -> Result<()> {
        loop {
            check_cancel(cancel)?;
            let mut frame = ffmpeg::frame::Video::empty();
            match self.decoders[index].decoder.receive_frame(&mut frame) {
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => return Ok(()),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::ffi::EAGAIN => {
                    return Ok(());
                }
                Err(error) => return Err(media(error)),
            }
            if frame.is_corrupt() || frame.has_decode_errors() {
                return Err(invalid("a decoded lens frame is corrupt"));
            }
            let decoder = &mut self.decoders[index];
            let pts = frame.pts().ok_or_else(|| {
                invalid(
                    "lens frame has no original presentation timestamp; ordinal pairing is unsafe",
                )
            })?;
            if decoder.last_pts.is_some_and(|last| pts <= last) {
                return Err(invalid(
                    "lens presentation timestamps are duplicated or nonmonotonic",
                ));
            }
            decoder.last_pts = Some(pts);
            if pts_before_time(
                pts,
                decoder.start_pts,
                decoder.time_base,
                self.discard_before,
            ) {
                continue;
            }
            retain_frame(&mut self.queues[index], &mut self.queued_bytes, frame)?;
        }
    }

    fn take_pair(&mut self) -> Result<Option<(ffmpeg::frame::Video, ffmpeg::frame::Video)>> {
        if let Some(dimensions) = self.packed {
            let Some(frame) = self.queues[0].pop_front() else {
                return Ok(None);
            };
            self.queued_bytes = self.queued_bytes.saturating_sub(frame_bytes(&frame));
            if (frame.width(), frame.height()) != dimensions {
                return Err(invalid(
                    "packed video dimensions changed from the validated source windows",
                ));
            }
            return Ok(Some((
                copy_packed_lens(&frame, 0)?,
                copy_packed_lens(&frame, 1)?,
            )));
        }
        let (Some(a), Some(b)) = (self.queues[0].front(), self.queues[1].front()) else {
            return Ok(None);
        };
        let a_pts = a
            .pts()
            .ok_or_else(|| invalid("missing camera A timestamp"))?;
        let b_pts = b
            .pts()
            .ok_or_else(|| invalid("missing camera B timestamp"))?;
        if compare_pts(
            a_pts,
            self.decoders[0].time_base,
            b_pts,
            self.decoders[1].time_base,
        ) != 0
        {
            return Err(invalid(format!(
                "camera frames are not simultaneous: A={a_pts}*{}, B={b_pts}*{}",
                self.decoders[0].time_base, self.decoders[1].time_base
            )));
        }
        if a.width() != b.width() || a.height() != b.height() {
            return Err(invalid("paired camera dimensions differ"));
        }
        let a = self.queues[0]
            .pop_front()
            .ok_or_else(|| invalid("camera A frame disappeared"))?;
        let b = self.queues[1]
            .pop_front()
            .ok_or_else(|| invalid("camera B frame disappeared"))?;
        self.queued_bytes = self
            .queued_bytes
            .saturating_sub(frame_bytes(&a) + frame_bytes(&b));
        Ok(Some((a, b)))
    }
}

/// Decode once, then copy each native-format half into a bounded owning frame.
/// Returning AVFrame crop views would expose `stride * height` through ffmpeg-next's
/// data() past the final cropped row. Row-aware FFmpeg copies avoid that overread.
fn copy_packed_lens(frame: &ffmpeg::frame::Video, camera: usize) -> Result<ffmpeg::frame::Video> {
    let width = frame.width() / 2;
    let mut view = ffmpeg::frame::Video::empty();
    let mut output = crate::stream::allocate_video_frame(frame.format(), width, frame.height())?;
    // SAFETY: av_frame_ref retains the source buffer. Cropping changes only the
    // temporary view; av_frame_copy uses plane widths, not an exposed padded slice.
    unsafe {
        let status = ffmpeg::ffi::av_frame_ref(view.as_mut_ptr(), frame.as_ptr());
        if status < 0 {
            return Err(media(ffmpeg::Error::from(status)));
        }
        (*view.as_mut_ptr()).crop_left = width as usize * camera;
        (*view.as_mut_ptr()).crop_right = width as usize * (1 - camera);
        let status = ffmpeg::ffi::av_frame_apply_cropping(
            view.as_mut_ptr(),
            ffmpeg::ffi::AV_FRAME_CROP_UNALIGNED as i32,
        );
        if status < 0 {
            return Err(media(ffmpeg::Error::from(status)));
        }
        if view.width() != width || view.height() != frame.height() {
            return Err(invalid(
                "decoded pixel format cannot represent the proven lens windows",
            ));
        }
        let status = ffmpeg::ffi::av_frame_copy(output.as_mut_ptr(), view.as_ptr());
        if status < 0 {
            return Err(media(ffmpeg::Error::from(status)));
        }
        let status = ffmpeg::ffi::av_frame_copy_props(output.as_mut_ptr(), frame.as_ptr());
        if status < 0 {
            return Err(media(ffmpeg::Error::from(status)));
        }
    }
    Ok(output)
}

fn retain_frame(
    queue: &mut VecDeque<ffmpeg::frame::Video>,
    bytes: &mut usize,
    frame: ffmpeg::frame::Video,
) -> Result<()> {
    let next = bytes
        .checked_add(frame_bytes(&frame))
        .ok_or_else(|| invalid("decoded frame backlog overflow"))?;
    if next > MAX_QUEUED_BYTES || queue.len() >= MAX_QUEUED_FRAMES {
        return Err(invalid(
            "camera synchronization exceeds the bounded frame backlog",
        ));
    }
    *bytes = next;
    queue.push_back(frame);
    Ok(())
}

fn frame_bytes(frame: &ffmpeg::frame::Video) -> usize {
    (0..frame.planes())
        .map(|plane| frame.data(plane).len())
        .sum()
}
fn compare_pts(a: i64, a_base: ffmpeg::Rational, b: i64, b_base: ffmpeg::Rational) -> i32 {
    unsafe { ffmpeg::ffi::av_compare_ts(a, a_base.into(), b, b_base.into()) }
}

/// Compare relative source time without rounding either the origin or request.
/// Positive i32 time bases and Duration's u64 seconds keep these products in i128.
fn pts_before_time(pts: i64, origin: i64, time_base: ffmpeg::Rational, time: Duration) -> bool {
    (i128::from(pts) - i128::from(origin)) * i128::from(time_base.numerator()) * 1_000_000_000
        < time.as_nanos() as i128 * i128::from(time_base.denominator())
}

fn relative_timestamp_micros(pts: i64, origin: i64, time_base: ffmpeg::Rational) -> Result<i64> {
    Ok(pts
        .checked_sub(origin)
        .ok_or_else(|| invalid("frame timestamp overflow"))?
        .rescale(time_base, (1, 1_000_000)))
}
fn micros(duration: Duration) -> Result<i64> {
    i64::try_from(duration.as_micros())
        .map_err(|_| invalid("recording time exceeds the supported timestamp range"))
}
fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidMedia(message.into())
}
fn media(error: ffmpeg::Error) -> Error {
    Error::Media(format!("paired decoding: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_and_audio_retention_enforce_both_count_and_memory_bounds() {
        let mut queue = VecDeque::new();
        let mut bytes = 0;
        for _ in 0..16 {
            retain_frame(
                &mut queue,
                &mut bytes,
                crate::stream::allocate_video_frame(ffmpeg::format::Pixel::RGB24, 8, 8).unwrap(),
            )
            .unwrap();
        }
        let previous = bytes;
        assert!(retain_frame(
            &mut queue,
            &mut bytes,
            crate::stream::allocate_video_frame(ffmpeg::format::Pixel::RGB24, 8, 8).unwrap()
        )
        .is_err());
        assert_eq!(queue.len(), 16);
        assert_eq!(bytes, previous);
        queue.clear();
        bytes = 256 * 1024 * 1024;
        assert!(retain_frame(
            &mut queue,
            &mut bytes,
            crate::stream::allocate_video_frame(ffmpeg::format::Pixel::RGB24, 8, 8).unwrap()
        )
        .is_err());
        assert!(queue.is_empty());

        let mut packets = Vec::new();
        bytes = 0;
        for _ in 0..65_536 {
            retain_audio(&mut packets, &mut bytes, 0, ffmpeg::Packet::empty()).unwrap();
        }
        assert!(retain_audio(&mut packets, &mut bytes, 0, ffmpeg::Packet::empty()).is_err());
        assert_eq!(packets.len(), 65_536);
        packets.clear();
        bytes = 64 * 1024 * 1024;
        assert!(retain_audio(&mut packets, &mut bytes, 0, ffmpeg::Packet::copy(&[1])).is_err());
        assert!(packets.is_empty());
        assert_eq!(bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn rational_pairing_rejects_equal_rounded_microseconds() {
        assert_eq!(compare_pts(1, (1, 30).into(), 3000, (1, 90000).into()), 0);
        assert_ne!(
            compare_pts(1, (1, 3_000_000).into(), 2, (1, 3_000_000).into()),
            0
        );
        assert_eq!(
            compare_pts(
                i64::MAX,
                (1, i32::MAX).into(),
                i64::MAX,
                (1, i32::MAX).into()
            ),
            0
        );
    }
}
