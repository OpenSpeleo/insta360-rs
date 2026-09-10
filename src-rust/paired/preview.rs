//! Random-access native previews: one persistent demuxer/decoder worker per lens.
//!
//! Export decoding remains a single demux pass. Scrubbing has a different access
//! pattern: keeping each lens on its own worker lets the codec and seek work run
//! concurrently without buffering an entire GOP of full-resolution frame pairs.

use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use super::*;

/// Hardware decoder policy for exact native preview frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewAcceleration {
    /// Use a supported hardware device, with portable software fallback.
    #[default]
    Auto,
    /// Decode in software; useful for deterministic reference comparisons.
    Software,
}

#[derive(Clone)]
struct Chapter {
    paths: Vec<PathBuf>,
    layout: DecodedLayout,
    start: Duration,
    duration: Duration,
}

struct Request {
    chapter: usize,
    time: Duration,
    cancel: Arc<AtomicBool>,
}

struct LensFrame {
    frame: ffmpeg::frame::Video,
    time_base: ffmpeg::Rational,
    origin_pts: i64,
}

struct Worker {
    sender: Option<mpsc::SyncSender<Request>>,
    receiver: mpsc::Receiver<Result<Option<LensFrame>>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn(
        chapters: Arc<Vec<Chapter>>,
        camera: usize,
        acceleration: PreviewAcceleration,
    ) -> Result<Self> {
        let (sender, requests) = mpsc::sync_channel::<Request>(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!(
                "insv-preview-{}",
                if camera == 0 { "A" } else { "B" }
            ))
            .spawn(move || {
                let mut session: Option<(usize, LensReader)> = None;
                let mut acceleration = acceleration;
                while let Ok(request) = requests.recv() {
                    let mut result = (|| {
                        check_cancel(&request.cancel)?;
                        if session
                            .as_ref()
                            .is_none_or(|(index, _)| *index != request.chapter)
                        {
                            session = Some((
                                request.chapter,
                                LensReader::open(&chapters[request.chapter], camera, acceleration)?,
                            ));
                        }
                        let reader = &mut session
                            .as_mut()
                            .ok_or_else(|| invalid("preview decoder unavailable"))?
                            .1;
                        reader.frame_at(request.time, &request.cancel)
                    })();
                    if matches!(result, Err(Error::Media(_)))
                        && acceleration == PreviewAcceleration::Auto
                    {
                        // Device creation can succeed even when this codec/profile
                        // cannot decode on it. Retry once in software, and keep
                        // that policy for subsequent requests in this session.
                        acceleration = PreviewAcceleration::Software;
                        session = None;
                        result = (|| {
                            check_cancel(&request.cancel)?;
                            let mut reader =
                                LensReader::open(&chapters[request.chapter], camera, acceleration)?;
                            let frame = reader.frame_at(request.time, &request.cancel)?;
                            session = Some((request.chapter, reader));
                            Ok(frame)
                        })();
                    }
                    if result.is_err() {
                        // A failed/cancelled decoder is never reused with uncertain state.
                        session = None;
                    }
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| invalid(format!("cannot start lens preview worker: {error}")))?;
        Ok(Self {
            sender: Some(sender),
            receiver,
            thread: Some(thread),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reusable random-access reader with bounded native decoding.
/// Separate lenses use concurrent workers. Packed sources use one software
/// decoder per request so each packed picture is decoded only once.
///
/// Each request returns source-resolution original frames, matched using exact
/// rational PTS. No proxies, frame-number estimates, or rounded time joins occur.
/// At most one request and one output per lens are queued. Failed reads reset
/// that lens session, and dropping the reader joins any worker threads.
pub struct PairedPreviewReader {
    chapters: Arc<Vec<Chapter>>,
    backend: PreviewBackend,
}

enum PreviewBackend {
    Lenses([Worker; 2]),
    Packed(RecordingSequence),
}

impl PairedPreviewReader {
    /// Prepares an inspected recording; opening media is lazy. Recordings with
    /// packed chapters use the serial software path for every request.
    pub fn new(sequence: &RecordingSequence, acceleration: PreviewAcceleration) -> Result<Self> {
        ffmpeg::init().map_err(media)?;
        let chapters = sequence
            .chapters
            .iter()
            .map(|chapter| {
                Ok(Chapter {
                    paths: chapter.inputs.paths().to_vec(),
                    layout: DecodedLayout::inspect(chapter)?,
                    start: chapter.timeline_start,
                    duration: chapter.duration,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let chapters = Arc::new(chapters);
        let backend = if chapters
            .iter()
            .any(|chapter| chapter.layout.packed.is_some())
        {
            PreviewBackend::Packed(sequence.clone())
        } else {
            PreviewBackend::Lenses([
                Worker::spawn(chapters.clone(), 0, acceleration)?,
                Worker::spawn(chapters.clone(), 1, acceleration)?,
            ])
        };
        Ok(Self { chapters, backend })
    }

    /// Returns the first simultaneous native pair at or after recording time.
    pub fn frame_at(&mut self, time: Duration, cancel: Arc<AtomicBool>) -> Result<FramePair> {
        check_cancel(&cancel)?;
        let workers = match &mut self.backend {
            PreviewBackend::Packed(sequence) => {
                return PairedReader::open(sequence, time)?
                    .next_pair(&cancel)?
                    .ok_or_else(|| invalid("no paired frame at this time"));
            }
            PreviewBackend::Lenses(workers) => workers,
        };
        let mut chapter_index = self
            .chapters
            .iter()
            .position(|chapter| time >= chapter.start && time < chapter.start + chapter.duration)
            .ok_or_else(|| invalid("preview time lies outside the recording"))?;
        loop {
            let chapter = &self.chapters[chapter_index];
            for worker in workers.iter() {
                worker
                    .sender
                    .as_ref()
                    .ok_or_else(|| invalid("preview worker stopped"))?
                    .send(Request {
                        chapter: chapter_index,
                        time: time.saturating_sub(chapter.start),
                        cancel: cancel.clone(),
                    })
                    .map_err(|_| invalid("preview worker stopped"))?;
            }
            // Always drain both replies, even on a decode error, so the next request
            // cannot accidentally consume the previous request's other-camera frame.
            let a = workers[0]
                .receiver
                .recv()
                .map_err(|_| invalid("camera A preview worker stopped"));
            let b = workers[1]
                .receiver
                .recv()
                .map_err(|_| invalid("camera B preview worker stopped"));
            let a = a??;
            let b = b??;
            check_cancel(&cancel)?;
            let (a, b) = match (a, b) {
                (Some(a), Some(b)) => (a, b),
                (None, None) if chapter_index + 1 < self.chapters.len() => {
                    chapter_index += 1;
                    continue;
                }
                (None, None) => return Err(invalid("no paired frame at this time")),
                _ => return Err(invalid("chapter ended with an unmatched camera frame")),
            };
            let a_pts = a
                .frame
                .pts()
                .ok_or_else(|| invalid("camera A preview timestamp missing"))?;
            let b_pts = b
                .frame
                .pts()
                .ok_or_else(|| invalid("camera B preview timestamp missing"))?;
            if compare_pts(a_pts, a.time_base, b_pts, b.time_base) != 0
                || compare_pts(a.origin_pts, a.time_base, b.origin_pts, b.time_base) != 0
            {
                return Err(invalid("preview lens frames are not simultaneous"));
            }
            if a.frame.width() != b.frame.width() || a.frame.height() != b.frame.height() {
                return Err(invalid("paired camera dimensions differ"));
            }
            let source_timestamp_micros = a_pts.rescale(a.time_base, (1, 1_000_000));
            let timestamp_micros = micros(chapter.start)?
                .checked_add(relative_timestamp_micros(a_pts, a.origin_pts, a.time_base)?)
                .ok_or_else(|| invalid("preview timestamp overflow"))?;
            return Ok(FramePair {
                a: a.frame,
                b: b.frame,
                a_pts,
                b_pts,
                a_time_base: a.time_base,
                b_time_base: b.time_base,
                chapter_index,
                source_timestamp_micros,
                timestamp_micros,
            });
        }
    }
}

struct LensReader {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::codec::decoder::Video,
    index: usize,
    time_base: ffmpeg::Rational,
    origin_micros: i64,
    origin_pts: i64,
    last_pts: Option<i64>,
    eof: bool,
}

impl LensReader {
    fn open(chapter: &Chapter, camera: usize, acceleration: PreviewAcceleration) -> Result<Self> {
        let source = chapter.layout.lenses[camera];
        let input = crate::stream::open_input(&chapter.paths[source.input])?;
        let tracks = input
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
            .collect::<Vec<_>>();
        let expected = if chapter.paths.len() == 1 { 2 } else { 1 };
        if tracks.len() != expected {
            return Err(Error::MissingCapability(
                "preview input differs from the declared lens layout".into(),
            ));
        }
        let stream = &tracks[source.video];
        let time_base = stream.time_base();
        if time_base.numerator() <= 0
            || time_base.denominator() <= 0
            || stream.start_time() == ffmpeg::ffi::AV_NOPTS_VALUE
        {
            return Err(invalid(
                "video tracks do not declare a valid presentation origin",
            ));
        }
        let origin_pts = stream.start_time();
        let origin_micros = origin_pts.rescale(time_base, (1, 1_000_000));
        let index = stream.index();
        let mut context =
            ffmpeg::codec::context::Context::from_parameters(stream.parameters()).map_err(media)?;
        if acceleration == PreviewAcceleration::Auto {
            initialize_hardware(&mut context);
        }
        context.set_threading(ffmpeg::codec::threading::Config {
            kind: ffmpeg::codec::threading::Type::Frame,
            count: 4,
        });
        unsafe {
            (*context.as_mut_ptr()).max_pixels = crate::stream::MAX_FRAME_PIXELS as i64;
        }
        let decoder = context.decoder().video().map_err(media)?;
        Ok(Self {
            input,
            decoder,
            index,
            time_base,
            origin_micros,
            origin_pts,
            last_pts: None,
            eof: false,
        })
    }

    fn frame_at(&mut self, time: Duration, cancel: &AtomicBool) -> Result<Option<LensFrame>> {
        let target = self
            .origin_micros
            .checked_add(micros(time)?)
            .ok_or_else(|| invalid("preview time overflow"))?;
        let continue_forward = self.last_pts.is_some_and(|pts| {
            let last = pts.rescale(self.time_base, (1, 1_000_000));
            target > last && target <= last.saturating_add(100_000) && !self.eof
        });
        if !continue_forward && (self.last_pts.is_some() || !time.is_zero()) {
            self.seek(target)?;
        }
        loop {
            check_cancel(cancel)?;
            let mut frame = ffmpeg::frame::Video::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    let pts = frame.pts().ok_or_else(|| {
                        invalid("lens frame has no original presentation timestamp")
                    })?;
                    if frame.is_corrupt() || frame.has_decode_errors() {
                        return Err(invalid("a decoded lens frame is corrupt"));
                    }
                    if self.last_pts.is_some_and(|last| pts <= last) {
                        return Err(invalid(
                            "lens presentation timestamps are duplicated or nonmonotonic",
                        ));
                    }
                    self.last_pts = Some(pts);
                    if pts_before_time(pts, self.origin_pts, self.time_base, time) {
                        continue;
                    }
                    let frame = transfer_native_frame(frame)?;
                    return Ok(Some(LensFrame {
                        frame,
                        time_base: self.time_base,
                        origin_pts: self.origin_pts,
                    }));
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::ffi::EAGAIN => {}
                Err(error) => return Err(media(error)),
            }
            loop {
                check_cancel(cancel)?;
                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut self.input) {
                    Ok(()) => {
                        if packet.stream() != self.index {
                            continue;
                        }
                        // Non-reference pictures before the target cannot contribute
                        // pixels to that target or any later picture. Keep every
                        // reference picture and all pictures at/after target, using
                        // packet PTS (never DTS) so B-frame reordering stays exact.
                        let discard = if packet.pts().is_some_and(|pts| {
                            pts_before_time(pts, self.origin_pts, self.time_base, time)
                        }) {
                            ffmpeg::Discard::NonReference
                        } else {
                            ffmpeg::Discard::None
                        };
                        self.decoder.skip_frame(discard);
                        self.decoder.send_packet(&packet).map_err(media)?;
                        break;
                    }
                    Err(ffmpeg::Error::Eof) => {
                        let io_error = unsafe {
                            let io = (*self.input.as_ptr()).pb;
                            if io.is_null() {
                                0
                            } else {
                                (*io).error
                            }
                        };
                        if io_error < 0 && io_error != ffmpeg::ffi::AVERROR_EOF {
                            return Err(media(ffmpeg::Error::from(io_error)));
                        }
                        self.decoder.send_eof().map_err(media)?;
                        self.eof = true;
                        break;
                    }
                    Err(error) => return Err(media(error)),
                }
            }
        }
    }

    fn seek(&mut self, target: i64) -> Result<()> {
        let stream = self
            .input
            .stream(self.index)
            .ok_or_else(|| invalid("preview stream disappeared"))?;
        let dts = target.rescale((1, 1_000_000), self.time_base);
        let mut anchor = self.origin_micros;
        // MP4 indexes are DTS. Only reordered streams need an additional GOP to
        // recover B frames preceding a key packet's presentation time.
        unsafe {
            let stream = stream.as_ptr().cast_mut();
            let mut entry = ffmpeg::ffi::avformat_index_get_entry_from_timestamp(
                stream,
                dts,
                ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
            );
            if !entry.is_null() && self.decoder.has_b_frames() {
                entry = ffmpeg::ffi::avformat_index_get_entry_from_timestamp(
                    stream,
                    (*entry).timestamp.saturating_sub(1),
                    ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
                );
            }
            if !entry.is_null() {
                anchor = (*entry)
                    .timestamp
                    .rescale(self.time_base, (1, 1_000_000))
                    .max(self.origin_micros);
            }
        }
        self.input
            .seek(anchor, ..anchor.saturating_add(1))
            .map_err(media)?;
        self.decoder.flush();
        self.last_pts = None;
        self.eof = false;
        Ok(())
    }
}

fn initialize_hardware(context: &mut ffmpeg::codec::context::Context) -> bool {
    use ffmpeg::ffi::{AVHWDeviceType as Device, AVPixelFormat};
    // Each decoder owns its own device reference. The callback reads only its
    // codec context and the sentinel-terminated format list supplied by FFmpeg.
    unsafe extern "C" fn choose_format(
        context: *mut ffmpeg::ffi::AVCodecContext,
        formats: *const AVPixelFormat,
    ) -> AVPixelFormat {
        unsafe {
            let mut candidate = formats;
            let mut software = AVPixelFormat::AV_PIX_FMT_NONE;
            while *candidate != AVPixelFormat::AV_PIX_FMT_NONE {
                let descriptor = ffmpeg::ffi::av_pix_fmt_desc_get(*candidate);
                if !descriptor.is_null()
                    && (*descriptor).flags & ffmpeg::ffi::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0
                {
                    if !(*context).hw_device_ctx.is_null() {
                        let device = (*(*context).hw_device_ctx)
                            .data
                            .cast::<ffmpeg::ffi::AVHWDeviceContext>();
                        let mut index = 0;
                        loop {
                            let config =
                                ffmpeg::ffi::avcodec_get_hw_config((*context).codec, index);
                            if config.is_null() {
                                break;
                            }
                            if (*config).device_type == (*device).type_
                                && (*config).pix_fmt == *candidate
                            {
                                return *candidate;
                            }
                            index += 1;
                        }
                    }
                } else if software == AVPixelFormat::AV_PIX_FMT_NONE {
                    software = *candidate;
                }
                candidate = candidate.add(1);
            }
            software
        }
    }
    let codec = unsafe { ffmpeg::ffi::avcodec_find_decoder((*context.as_ptr()).codec_id) };
    if codec.is_null() {
        return false;
    }
    for device in [
        Device::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
        Device::AV_HWDEVICE_TYPE_D3D11VA,
        Device::AV_HWDEVICE_TYPE_CUDA,
        Device::AV_HWDEVICE_TYPE_VAAPI,
    ] {
        unsafe {
            let mut index = 0;
            let mut supported = false;
            loop {
                let config = ffmpeg::ffi::avcodec_get_hw_config(codec, index);
                if config.is_null() {
                    break;
                }
                if (*config).device_type == device
                    && (*config).methods
                        & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32
                        != 0
                {
                    supported = true;
                    break;
                }
                index += 1;
            }
            if !supported {
                continue;
            }
            let mut reference = std::ptr::null_mut();
            if ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut reference,
                device,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            ) >= 0
                && !reference.is_null()
            {
                (*context.as_mut_ptr()).hw_device_ctx = reference;
                (*context.as_mut_ptr()).get_format = Some(choose_format);
                return true;
            }
            if !reference.is_null() {
                ffmpeg::ffi::av_buffer_unref(&mut reference);
            }
        }
    }
    false
}

fn transfer_native_frame(frame: ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video> {
    unsafe {
        let descriptor = ffmpeg::ffi::av_pix_fmt_desc_get(frame.format().into());
        if descriptor.is_null() {
            return Err(invalid("unknown decoded pixel format"));
        }
        if (*descriptor).flags & ffmpeg::ffi::AV_PIX_FMT_FLAG_HWACCEL as u64 == 0 {
            return Ok(frame);
        }
        let mut native = ffmpeg::frame::Video::empty();
        let result = ffmpeg::ffi::av_hwframe_transfer_data(native.as_mut_ptr(), frame.as_ptr(), 0);
        if result < 0 {
            return Err(media(ffmpeg::Error::from(result)));
        }
        let result = ffmpeg::ffi::av_frame_copy_props(native.as_mut_ptr(), frame.as_ptr());
        if result < 0 {
            return Err(media(ffmpeg::Error::from(result)));
        }
        Ok(native)
    }
}
