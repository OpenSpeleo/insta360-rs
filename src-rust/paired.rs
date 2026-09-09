//! Direct, bounded decoding of exact simultaneous lens pairs across chapters.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use crate::{Error, RecordingSequence, Result};

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

/// One demuxer and two decoders per chapter, never temporary exported video.
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
    /// Packed images and legacy two-file decoding require a separate proven lens adapter.
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
        seek_with_pair_preroll(
            &mut current.input,
            &current.decoders,
            target,
            current.origin_micros,
        )?;
        current.discard_before = target;
        reader.exact_start = Some(identity);
        Ok(reader)
    }

    /// Enables original audio packet forwarding before the first read.
    pub fn enable_audio(&mut self) {
        self.audio_enabled = true;
    }

    /// Drains original audio packets with their chapter indexes and native timestamps.
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
        let mut pending = current.audio_streams.clone();
        while !pending.is_empty() && !current.eof {
            check_cancel(cancel)?;
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut current.input) {
                Ok(()) => {
                    if !current.audio_streams.contains(&packet.stream()) {
                        continue;
                    }
                    let stream = current
                        .input
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
                    self.audio_bytes = self
                        .audio_bytes
                        .checked_add(packet.size())
                        .ok_or_else(|| invalid("audio backlog overflow"))?;
                    if self.audio_bytes > MAX_AUDIO_BYTES
                        || self.audio_packets.len() >= MAX_AUDIO_PACKETS
                    {
                        return Err(invalid("audio tail exceeds the bounded packet backlog"));
                    }
                    self.audio_packets.push((self.chapter_index, packet));
                }
                Err(ffmpeg::Error::Eof) => {
                    let io_error = unsafe {
                        let io = (*current.input.as_ptr()).pb;
                        if io.is_null() {
                            0
                        } else {
                            (*io).error
                        }
                    };
                    if io_error < 0 && io_error != ffmpeg::ffi::AVERROR_EOF {
                        return Err(media(ffmpeg::Error::from(io_error)));
                    }
                    current.eof = true;
                }
                Err(error) => return Err(media(error)),
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
                let b_time_base = current.decoders[1].time_base;
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
                let relative = source_timestamp_micros
                    .checked_sub(current.origin_micros)
                    .ok_or_else(|| invalid("frame timestamp overflow"))?;
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
            if current.eof {
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
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut current.input) {
                Ok(()) => {
                    if let Some(index) = current
                        .decoders
                        .iter()
                        .position(|decoder| decoder.index == packet.stream())
                    {
                        current.decode(index, &packet, cancel)?;
                    } else if self.audio_enabled && current.audio_streams.contains(&packet.stream())
                    {
                        self.audio_bytes = self
                            .audio_bytes
                            .checked_add(packet.size())
                            .ok_or_else(|| invalid("audio backlog overflow"))?;
                        if self.audio_bytes > MAX_AUDIO_BYTES
                            || self.audio_packets.len() >= MAX_AUDIO_PACKETS
                        {
                            return Err(invalid("audio packet backlog exceeded its bound; drain packets after each pair"));
                        }
                        self.audio_packets.push((self.chapter_index, packet));
                    }
                }
                Err(ffmpeg::Error::Eof) => {
                    let io_error = unsafe {
                        let io = (*current.input.as_ptr()).pb;
                        if io.is_null() {
                            0
                        } else {
                            (*io).error
                        }
                    };
                    if io_error < 0 && io_error != ffmpeg::ffi::AVERROR_EOF {
                        return Err(media(ffmpeg::Error::from(io_error)));
                    }
                    for index in 0..2 {
                        check_cancel(cancel)?;
                        current.decoders[index].decoder.send_eof().map_err(media)?;
                        current.receive(index, cancel)?;
                    }
                    current.eof = true;
                }
                Err(error) => return Err(media(error)),
            }
        }
    }

    fn open_chapter(&mut self) -> Result<()> {
        self.current = None;
        let Some(chapter) = self.sequence.chapters.get(self.chapter_index) else {
            return Ok(());
        };
        if chapter.inputs.paths().len() != 1 {
            return Err(Error::MissingCapability("paired decoding currently requires one INSV with two video tracks; legacy lens-file pairs can be unpacked losslessly".into()));
        }
        let path = &chapter.inputs.paths()[0];
        let mut input = ffmpeg::format::input(path).map_err(media)?;
        let mut decoders = input
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
            .map(|stream| {
                let time_base = stream.time_base();
                if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
                    return Err(invalid("invalid video stream time base"));
                }
                let mut context =
                    ffmpeg::codec::context::Context::from_parameters(stream.parameters())
                        .map_err(media)?;
                context.set_threading(ffmpeg::codec::threading::Config {
                    kind: ffmpeg::codec::threading::Type::Frame,
                    // Two streams decode concurrently. Bound codec frame delay as
                    // well as our queues so an 8K EOF drain cannot retain dozens
                    // of full-resolution frames before its partner is drained.
                    count: 4,
                });
                unsafe {
                    (*context.as_mut_ptr()).max_pixels = crate::stream::MAX_FRAME_PIXELS as i64;
                }
                Ok(Decoder {
                    index: stream.index(),
                    time_base,
                    decoder: context.decoder().video().map_err(media)?,
                    last_pts: None,
                    start_pts: stream.start_time(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if decoders.len() != 2 {
            return Err(Error::MissingCapability(
                "paired fisheye decoding requires exactly two video tracks".into(),
            ));
        }
        let reverse = chapter
            .inspection
            .metadata
            .reverse_video_track_order
            .ok_or_else(|| {
                Error::MissingCapability(
                    "recording does not declare which video track belongs to camera A/B".into(),
                )
            })?;
        if reverse {
            decoders.swap(0, 1);
        }
        let mut iter = decoders.into_iter();
        let a = iter
            .next()
            .ok_or_else(|| invalid("camera A decoder disappeared"))?;
        let b = iter
            .next()
            .ok_or_else(|| invalid("camera B decoder disappeared"))?;
        if a.start_pts == ffmpeg::ffi::AV_NOPTS_VALUE || b.start_pts == ffmpeg::ffi::AV_NOPTS_VALUE
        {
            return Err(invalid("video tracks do not declare a presentation origin"));
        }
        if compare_pts(a.start_pts, a.time_base, b.start_pts, b.time_base) != 0 {
            return Err(invalid(
                "camera tracks start at different presentation times",
            ));
        }
        let origin_micros = a.start_pts.rescale(a.time_base, (1, 1_000_000));
        let local = self.start.saturating_sub(chapter.timeline_start);
        let discard_before = origin_micros
            .checked_add(micros(local)?)
            .ok_or_else(|| invalid("seek timestamp overflow"))?;
        let decoders = [a, b];
        if !local.is_zero() {
            seek_with_pair_preroll(&mut input, &decoders, discard_before, origin_micros)?;
        }
        let audio_streams = input
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio)
            .map(|stream| stream.index())
            .collect();
        self.current = Some(ChapterReader {
            input,
            decoders,
            queues: [VecDeque::new(), VecDeque::new()],
            queued_bytes: 0,
            eof: false,
            origin_micros,
            discard_before,
            audio_streams,
        });
        Ok(())
    }
}

struct Decoder {
    index: usize,
    time_base: ffmpeg::Rational,
    decoder: ffmpeg::codec::decoder::Video,
    last_pts: Option<i64>,
    start_pts: i64,
}

/// MP4 indexes use decode timestamps. A key packet before the requested DTS can
/// have a later presentation time, with intervening B frames still depending on
/// the previous GOP. Keep one earlier indexed keyframe for *each* lens, then
/// choose their common earliest seek point. If either index cannot prove that
/// preroll, retain the freshly opened demuxer at the beginning of the chapter.
fn seek_with_pair_preroll(
    input: &mut ffmpeg::format::context::Input,
    decoders: &[Decoder; 2],
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
    input: ffmpeg::format::context::Input,
    decoders: [Decoder; 2],
    queues: [VecDeque<ffmpeg::frame::Video>; 2],
    queued_bytes: usize,
    eof: bool,
    origin_micros: i64,
    discard_before: i64,
    audio_streams: Vec<usize>,
}

impl ChapterReader {
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
                    return Ok(())
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
            if compare_pts(
                pts,
                decoder.time_base,
                self.discard_before,
                (1, 1_000_000).into(),
            ) < 0
            {
                continue;
            }
            self.queued_bytes = self
                .queued_bytes
                .checked_add(frame_bytes(&frame))
                .ok_or_else(|| invalid("decoded frame backlog overflow"))?;
            if self.queued_bytes > MAX_QUEUED_BYTES || self.queues[index].len() >= MAX_QUEUED_FRAMES
            {
                return Err(invalid(
                    "camera synchronization exceeds the bounded frame backlog",
                ));
            }
            self.queues[index].push_back(frame);
        }
    }

    fn take_pair(&mut self) -> Result<Option<(ffmpeg::frame::Video, ffmpeg::frame::Video)>> {
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

fn frame_bytes(frame: &ffmpeg::frame::Video) -> usize {
    (0..frame.planes())
        .map(|plane| frame.data(plane).len())
        .sum()
}
fn compare_pts(a: i64, a_base: ffmpeg::Rational, b: i64, b_base: ffmpeg::Rational) -> i32 {
    unsafe { ffmpeg::ffi::av_compare_ts(a, a_base.into(), b, b_base.into()) }
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
