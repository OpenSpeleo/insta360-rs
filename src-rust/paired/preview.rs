//! Random-access native previews: one persistent demuxer/decoder worker per lens.
//!
//! Export decoding remains a single demux pass. Scrubbing has a different access
//! pattern: keeping each lens on its own worker lets the codec and seek work run
//! concurrently without buffering an entire GOP of full-resolution frame pairs.

use std::num::NonZeroU32;
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

/// Selects a forward preview sample before copying hardware pixels to host memory.
/// Every decoded pair is still checked for exact lens synchronization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewSelection {
    /// Advance by this many actual pairs, preserving the final pair at EOF.
    pub advance: NonZeroU32,
    /// Then advance until this recording time, preserving the final pair at EOF.
    /// This display clock uses the same microsecond timestamps as FramePair;
    /// lens synchronization itself retains its exact rational PTS comparison.
    pub not_before: Option<Duration>,
}

impl Default for PreviewSelection {
    fn default() -> Self {
        Self {
            advance: NonZeroU32::MIN,
            not_before: None,
        }
    }
}

/// Cumulative reader diagnostics. These counters never change selection policy.
/// Codec preroll is excluded. Prefetch work appears only when its worker replies
/// are consumed or drained; discarded prefetch does not increment validated pairs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewDecodeStats {
    pub validated_pairs: u64,
    /// Selected pairs returned with CPU-readable pixels.
    pub materialized_pairs: u64,
    /// Completed eager-prefetch pairs consumed or drained, including those later discarded.
    pub speculative_pairs: u64,
    /// Hardware frames sent to their lens worker for a host transfer. A failed
    /// attempt may recover by decoding that exact frame in software.
    pub hardware_transfer_attempts: u64,
}

/// Native pixels cannot leave the reader until the selected pair is materialized.
struct NativePair {
    pair: FramePair,
    origins: Option<[i64; 2]>,
}

#[derive(Clone)]
struct Chapter {
    paths: Vec<PathBuf>,
    layout: DecodedLayout,
    start: Duration,
    duration: Duration,
}

#[derive(Clone)]
struct ReadRequest {
    chapter: usize,
    time: Option<Duration>,
    cancel: Arc<AtomicBool>,
    eager: bool,
}

struct LensFrame {
    frame: ffmpeg::frame::Video,
    time_base: ffmpeg::Rational,
    origin_pts: i64,
}

struct WorkerReply {
    result: Result<Option<LensFrame>>,
    hardware_transfer_attempts: u64,
}

impl From<Result<Option<LensFrame>>> for WorkerReply {
    fn from(result: Result<Option<LensFrame>>) -> Self {
        Self {
            result,
            hardware_transfer_attempts: 0,
        }
    }
}

enum WorkerRequest {
    Read(ReadRequest),
    Materialize {
        chapter: usize,
        cancel: Arc<AtomicBool>,
        frame: LensFrame,
    },
}

struct Worker {
    sender: Option<mpsc::SyncSender<WorkerRequest>>,
    receiver: mpsc::Receiver<WorkerReply>,
    thread: Option<JoinHandle<()>>,
}

fn receive_lens(
    worker: &Worker,
    index: usize,
    stats: &mut PreviewDecodeStats,
) -> Result<Option<LensFrame>> {
    let reply = worker
        .receiver
        .recv()
        .map_err(|_| invalid(format!("camera {index} preview worker stopped")))?;
    stats.hardware_transfer_attempts = stats
        .hardware_transfer_attempts
        .saturating_add(reply.hardware_transfer_attempts);
    reply.result
}

fn receive_lenses(
    workers: &[Worker; 2],
    stats: &mut PreviewDecodeStats,
    speculative: bool,
) -> [Result<Option<LensFrame>>; 2] {
    // Never short-circuit: the other reply belongs to this request even on error.
    let results: [Result<Option<LensFrame>>; 2] =
        std::array::from_fn(|index| receive_lens(&workers[index], index, stats));
    if speculative && results.iter().all(|result| matches!(result, Ok(Some(_)))) {
        stats.speculative_pairs = stats.speculative_pairs.saturating_add(1);
    }
    results
}

fn request_lenses(
    workers: &[Worker; 2],
    request: ReadRequest,
    stats: &mut PreviewDecodeStats,
) -> Result<()> {
    request_workers(
        workers,
        [
            WorkerRequest::Read(request.clone()),
            WorkerRequest::Read(request),
        ],
        stats,
    )
}

fn request_workers(
    workers: &[Worker; 2],
    requests: [WorkerRequest; 2],
    stats: &mut PreviewDecodeStats,
) -> Result<()> {
    for (index, (worker, request)) in workers.iter().zip(requests).enumerate() {
        let sent = worker
            .sender
            .as_ref()
            .ok_or_else(|| invalid("preview worker stopped"))
            .and_then(|sender| {
                sender
                    .send(request)
                    .map_err(|_| invalid("preview worker stopped"))
            });
        if let Err(error) = sent {
            // Account for and drain A even when only that lens accepted the request.
            for (started_index, started) in workers[..index].iter().enumerate() {
                let _ = receive_lens(started, started_index, stats);
            }
            return Err(error);
        }
    }
    Ok(())
}

impl Worker {
    fn spawn(
        chapters: Arc<Vec<Chapter>>,
        camera: usize,
        acceleration: PreviewAcceleration,
    ) -> Result<Self> {
        let (sender, requests) = mpsc::sync_channel::<WorkerRequest>(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!(
                "insv-preview-{}",
                if camera == 0 { "A" } else { "B" }
            ))
            .spawn(move || {
                let mut worker = LensWorker {
                    chapters,
                    camera,
                    acceleration,
                    session: None,
                    hardware_transfer_attempts: 0,
                };
                while let Ok(request) = requests.recv() {
                    let before = worker.hardware_transfer_attempts;
                    let result = match request {
                        WorkerRequest::Read(request) => worker.read(request),
                        WorkerRequest::Materialize {
                            chapter,
                            cancel,
                            frame,
                        } => worker.materialize(chapter, frame, &cancel),
                    };
                    if result.is_err() {
                        // A failed/cancelled decoder is never reused with uncertain state.
                        worker.session = None;
                    }
                    if results
                        .send(WorkerReply {
                            result,
                            hardware_transfer_attempts: worker
                                .hardware_transfer_attempts
                                .saturating_sub(before),
                        })
                        .is_err()
                    {
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

/// Decoder policy and transfer recovery stay on the lens's original worker.
struct LensWorker {
    chapters: Arc<Vec<Chapter>>,
    camera: usize,
    acceleration: PreviewAcceleration,
    session: Option<(usize, LensReader)>,
    hardware_transfer_attempts: u64,
}

impl LensWorker {
    fn read(&mut self, request: ReadRequest) -> Result<Option<LensFrame>> {
        let resume_after = self.session.as_ref().and_then(|(chapter, reader)| {
            (*chapter == request.chapter)
                .then_some(reader.last_pts)
                .flatten()
        });
        let mut result = (|| {
            check_cancel(&request.cancel)?;
            if self
                .session
                .as_ref()
                .is_none_or(|(index, _)| *index != request.chapter)
            {
                self.session = Some((
                    request.chapter,
                    LensReader::open(
                        &self.chapters[request.chapter],
                        self.camera,
                        self.acceleration,
                    )?,
                ));
            }
            self.session
                .as_mut()
                .expect("preview decoder opened")
                .1
                .read_frame(request.time, None, &request.cancel)
        })();
        if matches!(result, Err(Error::Media(_))) && self.acceleration == PreviewAcceleration::Auto
        {
            // A device can initialize even when this codec/profile cannot decode.
            self.acceleration = PreviewAcceleration::Software;
            self.session = None;
            result = (|| {
                check_cancel(&request.cancel)?;
                let mut reader = LensReader::open(
                    &self.chapters[request.chapter],
                    self.camera,
                    self.acceleration,
                )?;
                let after = request.time.is_none().then_some(resume_after).flatten();
                if let Some(pts) = after {
                    reader.seek(pts.rescale(reader.time_base, (1, 1_000_000)))?;
                }
                let frame = reader.read_frame(request.time, after, &request.cancel)?;
                self.session = Some((request.chapter, reader));
                Ok(frame)
            })();
        }
        match result? {
            Some(frame) if request.eager => {
                self.materialize(request.chapter, frame, &request.cancel)
            }
            frame => Ok(frame),
        }
    }

    fn materialize(
        &mut self,
        chapter: usize,
        frame: LensFrame,
        cancel: &AtomicBool,
    ) -> Result<Option<LensFrame>> {
        self.materialize_with(chapter, frame, cancel, transfer_native_frame)
    }

    fn materialize_with(
        &mut self,
        chapter: usize,
        frame: LensFrame,
        cancel: &AtomicBool,
        transfer: impl FnOnce(ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video>,
    ) -> Result<Option<LensFrame>> {
        check_cancel(cancel)?;
        let pts = frame
            .frame
            .pts()
            .ok_or_else(|| invalid("preview timestamp missing before transfer"))?;
        let time_base = frame.time_base;
        let origin_pts = frame.origin_pts;
        if is_hardware_frame(&frame.frame)? {
            self.hardware_transfer_attempts = self.hardware_transfer_attempts.saturating_add(1);
        }
        match transfer(frame.frame) {
            Ok(frame) => {
                check_cancel(cancel)?;
                Ok(Some(LensFrame {
                    frame,
                    time_base,
                    origin_pts,
                }))
            }
            Err(Error::Media(_)) => {
                // A retained native candidate can outlive a later decoder's
                // switch to software. Transfer failure still retries its exact PTS.
                self.acceleration = PreviewAcceleration::Software;
                self.session = None;
                check_cancel(cancel)?;
                let mut reader =
                    LensReader::open(&self.chapters[chapter], self.camera, self.acceleration)?;
                if reader.time_base != time_base || reader.origin_pts != origin_pts {
                    return Err(invalid(
                        "software retry changed the native presentation clock",
                    ));
                }
                reader.seek(pts.rescale(time_base, (1, 1_000_000)))?;
                // Filter by the native integer PTS, never a rounded seek target.
                let after = pts
                    .checked_sub(1)
                    .ok_or_else(|| invalid("preview timestamp underflow"))?;
                let restored = reader
                    .read_frame(None, Some(after), cancel)?
                    .ok_or_else(|| invalid("software retry lost the selected preview frame"))?;
                if restored.frame.pts() != Some(pts) {
                    return Err(invalid("software retry changed the selected preview frame"));
                }
                self.session = Some((chapter, reader));
                Ok(Some(restored))
            }
            Err(error) => Err(error),
        }
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

/// Reusable random-access and sequential reader with bounded native decoding.
/// Separate lenses use concurrent workers. Packed sources share one persistent
/// software decoder so each packed picture is decoded only once.
///
/// Each request returns source-resolution original frames, matched using exact
/// rational PTS. No proxies, frame-number estimates, or rounded time joins occur.
/// At most one request and one output per lens are queued. Failed reads reset
/// that lens session, and dropping the reader joins any worker threads.
pub struct PairedPreviewReader {
    chapters: Arc<Vec<Chapter>>,
    backend: PreviewBackend,
    cursor: usize,
    pending: Option<ReadRequest>,
    stats: PreviewDecodeStats,
}

enum PreviewBackend {
    Lenses([Worker; 2]),
    Packed {
        sequence: RecordingSequence,
        reader: Option<Box<PairedReader>>,
    },
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
            PreviewBackend::Packed {
                sequence: sequence.clone(),
                reader: None,
            }
        } else {
            PreviewBackend::Lenses([
                Worker::spawn(chapters.clone(), 0, acceleration)?,
                Worker::spawn(chapters.clone(), 1, acceleration)?,
            ])
        };
        Ok(Self {
            chapters,
            backend,
            cursor: 0,
            pending: None,
            stats: PreviewDecodeStats::default(),
        })
    }

    /// Returns the first simultaneous native pair at or after recording time.
    pub fn frame_at(&mut self, time: Duration, cancel: Arc<AtomicBool>) -> Result<FramePair> {
        self.seek_pair(time, cancel)?
            .ok_or_else(|| invalid("no paired frame at this time"))
    }

    /// Seeks to the first pair at or after time, returning `None` after the final
    /// sample. Decode failures remain errors so callers can distinguish EOF.
    pub fn seek_pair(
        &mut self,
        time: Duration,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<FramePair>> {
        let pair = self.read_native_pair(Some(time), cancel.clone(), true)?;
        pair.map(|pair| self.materialize_pair(pair, &cancel))
            .transpose()
    }

    /// Reads the next exact pair without seeking or estimating frame duration.
    /// A new reader starts at the recording origin. After `frame_at`, reading
    /// continues with its immediate successor; EOF remains EOF until a new seek.
    pub fn next_pair(&mut self, cancel: Arc<AtomicBool>) -> Result<Option<FramePair>> {
        self.next_selected_pair(PreviewSelection::default(), cancel)
    }

    /// Selects forward by actual pairs and display time without downloading
    /// discarded hardware frames. A final candidate survives EOF; an initial
    /// EOF still returns None. Every skipped pair is synchronized and validated.
    /// One eager prefetched pair may already have been downloaded; additional
    /// catch-up candidates remain native until selection finishes.
    pub fn next_selected_pair(
        &mut self,
        selection: PreviewSelection,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<FramePair>> {
        let eager = selection == PreviewSelection::default();
        let Some(mut candidate) = self.read_native_pair(None, cancel.clone(), eager)? else {
            return Ok(None);
        };
        for _ in 1..selection.advance.get() {
            let Some(next) = self.read_native_pair(None, cancel.clone(), false)? else {
                break;
            };
            candidate = next;
        }
        while selection.not_before.is_some_and(|time| {
            Duration::from_micros(candidate.pair.timestamp_micros.max(0) as u64) < time
        }) {
            let Some(next) = self.read_native_pair(None, cancel.clone(), false)? else {
                break;
            };
            candidate = next;
        }
        self.materialize_pair(candidate, &cancel).map(Some)
    }

    pub fn decode_stats(&self) -> PreviewDecodeStats {
        self.stats
    }

    fn materialize_pair(
        &mut self,
        native: NativePair,
        cancel: &Arc<AtomicBool>,
    ) -> Result<FramePair> {
        check_cancel(cancel)?;
        let NativePair { pair, origins } = native;
        let transfers = [is_hardware_frame(&pair.a)?, is_hardware_frame(&pair.b)?];
        let pair = if transfers.iter().any(|&transfer| transfer) {
            let PreviewBackend::Lenses(workers) = &self.backend else {
                return Err(invalid("packed software decoder returned a hardware frame"));
            };
            let origins =
                origins.ok_or_else(|| invalid("native lens presentation origins missing"))?;
            let dimensions = [
                (pair.a.width(), pair.a.height()),
                (pair.b.width(), pair.b.height()),
            ];
            request_workers(
                workers,
                [
                    WorkerRequest::Materialize {
                        chapter: pair.chapter_index,
                        cancel: cancel.clone(),
                        frame: LensFrame {
                            frame: pair.a,
                            time_base: pair.a_time_base,
                            origin_pts: origins[0],
                        },
                    },
                    WorkerRequest::Materialize {
                        chapter: pair.chapter_index,
                        cancel: cancel.clone(),
                        frame: LensFrame {
                            frame: pair.b,
                            time_base: pair.b_time_base,
                            origin_pts: origins[1],
                        },
                    },
                ],
                &mut self.stats,
            )?;
            let [a, b] = receive_lenses(workers, &mut self.stats, false);
            let a =
                a?.ok_or_else(|| invalid("selected camera A frame disappeared during transfer"))?;
            let b =
                b?.ok_or_else(|| invalid("selected camera B frame disappeared during transfer"))?;
            if a.frame.pts() != Some(pair.a_pts)
                || b.frame.pts() != Some(pair.b_pts)
                || a.time_base != pair.a_time_base
                || b.time_base != pair.b_time_base
                || a.origin_pts != origins[0]
                || b.origin_pts != origins[1]
                || [
                    (a.frame.width(), a.frame.height()),
                    (b.frame.width(), b.frame.height()),
                ] != dimensions
            {
                return Err(invalid("host transfer changed the selected native pair"));
            }
            FramePair {
                a: a.frame,
                b: b.frame,
                ..pair
            }
        } else {
            pair
        };
        check_cancel(cancel)?;
        self.stats.materialized_pairs = self.stats.materialized_pairs.saturating_add(1);
        Ok(pair)
    }

    /// Starts one next-pair decode and host transfer while the caller processes
    /// the current pair. This bounded speculative copy preserves decode overlap.
    /// Repeated calls do not queue more work. `next_pair` consumes both replies;
    /// a seek discards them before changing position. The packed-source backend
    /// remains synchronous and treats this request as a no-op.
    pub fn prefetch_next(&mut self, cancel: Arc<AtomicBool>) -> Result<()> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            self.discard_pending();
        }
        check_cancel(&cancel)?;
        if self.pending.is_some() || self.cursor >= self.chapters.len() {
            return Ok(());
        }
        if let PreviewBackend::Lenses(workers) = &self.backend {
            let request = ReadRequest {
                chapter: self.cursor,
                time: None,
                cancel,
                eager: true,
            };
            request_lenses(workers, request.clone(), &mut self.stats)?;
            self.pending = Some(request);
        }
        Ok(())
    }

    fn discard_pending(&mut self) {
        if self.pending.take().is_some() {
            if let PreviewBackend::Lenses(workers) = &self.backend {
                // A fresh seek can recover a cancelled or failed read. Its
                // replies must still leave the channels before that seek starts.
                let _ = receive_lenses(workers, &mut self.stats, true);
            }
        }
    }

    fn read_native_pair(
        &mut self,
        time: Option<Duration>,
        cancel: Arc<AtomicBool>,
        eager: bool,
    ) -> Result<Option<NativePair>> {
        if time.is_some() || cancel.load(std::sync::atomic::Ordering::Relaxed) {
            self.discard_pending();
        }
        check_cancel(&cancel)?;
        if time.is_some_and(|time| {
            self.chapters
                .last()
                .is_none_or(|chapter| time >= chapter.start + chapter.duration)
        }) {
            self.cursor = self.chapters.len();
            return Ok(None);
        }
        if time.is_some() {
            self.cursor = 0;
        }
        if self.cursor >= self.chapters.len() {
            return Ok(None);
        }
        let workers = match &mut self.backend {
            PreviewBackend::Packed { sequence, reader } => {
                if time.is_some() || reader.is_none() {
                    *reader = Some(Box::new(PairedReader::open(
                        sequence,
                        time.unwrap_or_default(),
                    )?));
                }
                let pair = reader
                    .as_mut()
                    .ok_or_else(|| invalid("packed decoder unavailable"))?
                    .next_pair(&cancel)?;
                if pair.is_some() {
                    self.stats.validated_pairs = self.stats.validated_pairs.saturating_add(1);
                }
                return Ok(pair.map(|pair| NativePair {
                    pair,
                    origins: None,
                }));
            }
            PreviewBackend::Lenses(workers) => workers,
        };
        let mut chapter_index = if let Some(time) = time {
            self.chapters
                .iter()
                .position(|chapter| {
                    time >= chapter.start && time < chapter.start + chapter.duration
                })
                .ok_or_else(|| invalid("preview time lies outside the recording"))?
        } else {
            self.cursor
        };
        if chapter_index >= self.chapters.len() {
            return Ok(None);
        }
        loop {
            let chapter = &self.chapters[chapter_index];
            let speculative = self.pending.is_some();
            let request = match self.pending.take() {
                Some(request) => request,
                None => {
                    let request = ReadRequest {
                        chapter: chapter_index,
                        time: time.map(|time| time.saturating_sub(chapter.start)),
                        cancel: cancel.clone(),
                        eager,
                    };
                    request_lenses(workers, request.clone(), &mut self.stats)?;
                    request
                }
            };
            let [a, b] = receive_lenses(workers, &mut self.stats, speculative);
            let a = a?;
            let b = b?;
            check_cancel(&request.cancel)?;
            check_cancel(&cancel)?;
            if request.chapter != chapter_index {
                return Err(invalid("prefetched pair belongs to another chapter"));
            }
            let (a, b) = match (a, b) {
                (Some(a), Some(b)) => (a, b),
                (None, None) if chapter_index + 1 < self.chapters.len() => {
                    chapter_index += 1;
                    continue;
                }
                (None, None) => {
                    self.cursor = self.chapters.len();
                    return Ok(None);
                }
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
            self.cursor = chapter_index;
            self.stats.validated_pairs = self.stats.validated_pairs.saturating_add(1);
            return Ok(Some(NativePair {
                origins: Some([a.origin_pts, b.origin_pts]),
                pair: FramePair {
                    a: a.frame,
                    b: b.frame,
                    a_pts,
                    b_pts,
                    a_time_base: a.time_base,
                    b_time_base: b.time_base,
                    chapter_index,
                    source_timestamp_micros,
                    timestamp_micros,
                },
            }));
        }
    }
}

impl Drop for PairedPreviewReader {
    fn drop(&mut self) {
        self.discard_pending();
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

    fn read_frame(
        &mut self,
        time: Option<Duration>,
        after_pts: Option<i64>,
        cancel: &AtomicBool,
    ) -> Result<Option<LensFrame>> {
        if let Some(time) = time {
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
                    if time.is_some_and(|time| {
                        pts_before_time(pts, self.origin_pts, self.time_base, time)
                    }) || after_pts.is_some_and(|after| pts <= after)
                    {
                        continue;
                    }
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
                            time.is_some_and(|time| {
                                pts_before_time(pts, self.origin_pts, self.time_base, time)
                            })
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

fn is_hardware_frame(frame: &ffmpeg::frame::Video) -> Result<bool> {
    // SAFETY: format is read from a live FFmpeg frame; descriptors are static.
    unsafe {
        let descriptor = ffmpeg::ffi::av_pix_fmt_desc_get(frame.format().into());
        if descriptor.is_null() {
            return Err(invalid("unknown decoded pixel format"));
        }
        Ok((*descriptor).flags & ffmpeg::ffi::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0)
    }
}

fn transfer_native_frame(frame: ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video> {
    if !is_hardware_frame(&frame)? {
        return Ok(frame);
    }
    unsafe {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_channels() -> (
        Worker,
        mpsc::Receiver<WorkerRequest>,
        mpsc::SyncSender<WorkerReply>,
    ) {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::sync_channel(1);
        (
            Worker {
                sender: Some(sender),
                receiver,
                thread: None,
            },
            requests,
            results,
        )
    }

    /// Transfer log: chapter, native PTS, performed during the original read.
    type TransferLog = Arc<std::sync::Mutex<Vec<(usize, i64, bool)>>>;

    fn scripted_reader(
        times: [Vec<Vec<i64>>; 2],
        hardware: bool,
    ) -> (PairedPreviewReader, [TransferLog; 2]) {
        let chapters = Arc::new(
            (0..times[0].len())
                .map(|index| Chapter {
                    paths: Vec::new(),
                    layout: DecodedLayout {
                        lenses: [super::super::layout::LensSource { input: 0, video: 0 }; 2],
                        packed: None,
                    },
                    start: Duration::from_secs(index as u64),
                    duration: Duration::from_secs(1),
                })
                .collect(),
        );
        let logs: [TransferLog; 2] = std::array::from_fn(|_| Arc::default());
        let workers = times
            .into_iter()
            .zip(logs.iter())
            .map(|(times, log)| {
                let (mut worker, requests, results) = worker_channels();
                let log = log.clone();
                worker.thread = Some(std::thread::spawn(move || {
                    let (mut current_chapter, mut next_index) = (usize::MAX, 0);
                    while let Ok(request) = requests.recv() {
                        let before = log.lock().unwrap().len();
                        let result = match request {
                            WorkerRequest::Read(request) => {
                                if request.cancel.load(Ordering::Relaxed) {
                                    Err(Error::Cancelled)
                                } else {
                                    if current_chapter != request.chapter {
                                        current_chapter = request.chapter;
                                        next_index = 0;
                                    }
                                    if let Some(time) = request.time {
                                        next_index = times[current_chapter]
                                            .iter()
                                            .position(|&pts| {
                                                Duration::from_micros(pts as u64) >= time
                                            })
                                            .unwrap_or(times[current_chapter].len());
                                    }
                                    let next = times[current_chapter].get(next_index).map(|&pts| {
                                        next_index += 1;
                                        if hardware && request.eager {
                                            log.lock().unwrap().push((
                                                request.chapter,
                                                250_000 + pts,
                                                true,
                                            ));
                                        }
                                        let mut frame = if hardware && !request.eager {
                                            let mut frame = ffmpeg::frame::Video::empty();
                                            // This descriptor-only frame is handled exclusively by the
                                            // mock actor; it is never passed to a native transfer call.
                                            frame.set_format(ffmpeg::format::Pixel::VIDEOTOOLBOX);
                                            frame.set_width(2);
                                            frame.set_height(2);
                                            frame
                                        } else {
                                            let mut frame = ffmpeg::frame::Video::new(
                                                ffmpeg::format::Pixel::RGB24,
                                                2,
                                                2,
                                            );
                                            frame.data_mut(0).fill(37);
                                            frame
                                        };
                                        frame.set_pts(Some(250_000 + pts));
                                        LensFrame {
                                            frame,
                                            time_base: (1, 1_000_000).into(),
                                            origin_pts: 250_000,
                                        }
                                    });
                                    Ok(next)
                                }
                            }
                            WorkerRequest::Materialize {
                                chapter,
                                frame,
                                cancel,
                            } => {
                                if cancel.load(Ordering::Relaxed) {
                                    Err(Error::Cancelled)
                                } else {
                                    let pts = frame.frame.pts().unwrap();
                                    log.lock().unwrap().push((chapter, pts, false));
                                    let mut pixels = ffmpeg::frame::Video::new(
                                        ffmpeg::format::Pixel::RGB24,
                                        2,
                                        2,
                                    );
                                    pixels.set_pts(Some(pts));
                                    pixels.data_mut(0).fill(37);
                                    Ok(Some(LensFrame {
                                        frame: pixels,
                                        ..frame
                                    }))
                                }
                            }
                        };
                        let attempts = log.lock().unwrap().len() - before;
                        if results
                            .send(WorkerReply {
                                result,
                                hardware_transfer_attempts: attempts as u64,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
                worker
            })
            .collect::<Vec<_>>();
        let [first, second]: [Worker; 2] = workers.try_into().ok().unwrap();
        (
            PairedPreviewReader {
                chapters,
                backend: PreviewBackend::Lenses([first, second]),
                cursor: 0,
                pending: None,
                stats: PreviewDecodeStats::default(),
            },
            logs,
        )
    }

    #[test]
    fn native_selection_downloads_only_admitted_pairs_across_gaps_chapters_and_eof() {
        let times = vec![vec![0, 33_333, 700_000], vec![0, 50_000, 200_000]];
        let (mut reader, logs) = scripted_reader([times.clone(), times], true);
        let cancel = Arc::new(AtomicBool::new(false));
        reader.prefetch_next(cancel.clone()).unwrap();
        reader.prefetch_next(cancel.clone()).unwrap();
        assert_eq!(
            reader.decode_stats(),
            PreviewDecodeStats::default(),
            "prefetch replies have not yet been consumed"
        );
        let selected = reader
            .next_selected_pair(
                PreviewSelection {
                    advance: NonZeroU32::new(3).unwrap(),
                    not_before: Some(Duration::from_nanos(1_000_000_001)),
                },
                cancel.clone(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (selected.chapter_index, selected.timestamp_micros),
            (1, 1_050_000)
        );
        assert!(!is_hardware_frame(&selected.a).unwrap());
        assert_eq!(selected.a.data(0)[0], 37);
        assert_eq!(
            reader.decode_stats(),
            PreviewDecodeStats {
                validated_pairs: 5,
                materialized_pairs: 1,
                speculative_pairs: 1,
                hardware_transfer_attempts: 4,
            }
        );
        let last = reader
            .next_selected_pair(
                PreviewSelection {
                    advance: NonZeroU32::new(8).unwrap(),
                    not_before: Some(Duration::from_secs(5)),
                },
                cancel.clone(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(last.timestamp_micros, 1_200_000);
        assert!(reader.next_pair(cancel.clone()).unwrap().is_none());
        for log in logs {
            assert_eq!(
                *log.lock().unwrap(),
                [(0, 250_000, true), (1, 300_000, false), (1, 450_000, false)]
            );
        }
        assert_eq!(reader.decode_stats().materialized_pairs, 2);
        let first = reader.seek_pair(Duration::ZERO, cancel).unwrap().unwrap();
        assert_eq!(first.timestamp_micros, 0);
    }

    #[test]
    fn failed_transfer_recovers_exact_retained_pts_and_successor_after_decoder_eof() {
        ffmpeg::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transfer-recovery.mp4");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=30:duration=1",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=30:duration=1,hue=h=90",
                "-map",
                "0:v",
                "-map",
                "1:v",
                "-c:v",
                "mpeg4",
                "-g",
                "6",
                "-bf",
                "2",
                "-threads",
                "1",
                "-output_ts_offset",
                "0.067",
                "-f",
                "mp4",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let chapters = Arc::new(vec![Chapter {
            paths: vec![path],
            layout: DecodedLayout {
                lenses: [
                    super::super::layout::LensSource { input: 0, video: 0 },
                    super::super::layout::LensSource { input: 0, video: 1 },
                ],
                packed: None,
            },
            start: Duration::ZERO,
            duration: Duration::from_secs(1),
        }]);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut reference =
            LensReader::open(&chapters[0], 0, PreviewAcceleration::Software).unwrap();
        let mut frames = Vec::new();
        while let Some(frame) = reference.read_frame(None, None, &cancel).unwrap() {
            frames.push(frame);
        }
        assert_eq!(frames.len(), 30);
        assert!(reference.decoder.has_b_frames());
        assert_ne!(frames[8].origin_pts, 0);
        let assert_same = |actual: &LensFrame, expected: &LensFrame| {
            assert_eq!(actual.frame.pts(), expected.frame.pts());
            assert_eq!(actual.time_base, expected.time_base);
            assert_eq!(actual.origin_pts, expected.origin_pts);
            assert_eq!(actual.frame.format(), ffmpeg::format::Pixel::YUV420P);
            assert_eq!(actual.frame.color_space(), expected.frame.color_space());
            assert_eq!(actual.frame.color_range(), expected.frame.color_range());
            for plane in 0..3 {
                let width = expected.frame.plane_width(plane) as usize;
                for row in 0..expected.frame.plane_height(plane) as usize {
                    assert_eq!(
                        &actual.frame.data(plane)[row * actual.frame.stride(plane)..][..width],
                        &expected.frame.data(plane)[row * expected.frame.stride(plane)..][..width],
                    );
                }
            }
        };
        for policy_at_transfer in [PreviewAcceleration::Auto, PreviewAcceleration::Software] {
            let mut worker = LensWorker {
                chapters: chapters.clone(),
                camera: 0,
                acceleration: PreviewAcceleration::Software,
                session: None,
                hardware_transfer_attempts: 0,
            };
            let request = ReadRequest {
                chapter: 0,
                time: None,
                cancel: cancel.clone(),
                eager: false,
            };
            for _ in 0..8 {
                worker.read(request.clone()).unwrap().unwrap();
            }
            let retained = worker.read(request.clone()).unwrap().unwrap();
            assert_same(&retained, &frames[8]);
            while worker.read(request.clone()).unwrap().is_some() {}
            // A retained native picture may survive an EOF probe or a later
            // hardware decoder failure. Its own PTS remains the recovery target.
            worker.acceleration = policy_at_transfer;
            let restored = worker
                .materialize_with(0, retained, &cancel, |frame| {
                    assert_eq!(frame.pts(), frames[8].frame.pts());
                    Err(media(ffmpeg::Error::Other {
                        errno: ffmpeg::ffi::EIO,
                    }))
                })
                .unwrap()
                .unwrap();
            assert_same(&restored, &frames[8]);
            assert_eq!(worker.acceleration, PreviewAcceleration::Software);
            assert_same(&worker.read(request.clone()).unwrap().unwrap(), &frames[9]);

            // Recovery must fail rather than silently substitute the next
            // picture when a requested native PTS does not actually exist.
            let mut missing = LensFrame {
                frame: frames[8].frame.clone(),
                time_base: frames[8].time_base,
                origin_pts: frames[8].origin_pts,
            };
            let missing_pts = missing.frame.pts().unwrap() + 1;
            missing.frame.set_pts(Some(missing_pts));
            let error = worker
                .materialize_with(0, missing, &cancel, |_| {
                    Err(media(ffmpeg::Error::Other {
                        errno: ffmpeg::ffi::EIO,
                    }))
                })
                .err()
                .unwrap();
            assert!(error
                .to_string()
                .contains("changed the selected preview frame"));
            assert!(worker.session.is_none());

            let interrupted = LensFrame {
                frame: frames[8].frame.clone(),
                time_base: frames[8].time_base,
                origin_pts: frames[8].origin_pts,
            };
            let error = worker
                .materialize_with(0, interrupted, &cancel, |_| {
                    cancel.store(true, Ordering::Relaxed);
                    Err(media(ffmpeg::Error::Other {
                        errno: ffmpeg::ffi::EIO,
                    }))
                })
                .err()
                .unwrap();
            assert!(matches!(error, Error::Cancelled));
            assert!(worker.session.is_none());
            cancel.store(false, Ordering::Relaxed);
            assert_same(&worker.read(request).unwrap().unwrap(), &frames[0]);
        }
    }

    #[test]
    fn normal_next_and_prefetch_keep_transfer_on_the_original_read() {
        let times = vec![vec![0, 33_333, 66_667]];
        let (mut reader, logs) = scripted_reader([times.clone(), times], true);
        let cancel = Arc::new(AtomicBool::new(false));
        let first = reader.next_pair(cancel.clone()).unwrap().unwrap();
        assert_eq!(first.timestamp_micros, 0);
        reader.prefetch_next(cancel.clone()).unwrap();
        reader.prefetch_next(cancel.clone()).unwrap();
        let second = reader.next_pair(cancel).unwrap().unwrap();
        assert_eq!(second.timestamp_micros, 33_333);
        for log in logs {
            assert_eq!(
                *log.lock().unwrap(),
                [(0, 250_000, true), (0, 283_333, true)]
            );
        }
        assert_eq!(
            reader.decode_stats(),
            PreviewDecodeStats {
                validated_pairs: 2,
                materialized_pairs: 2,
                speculative_pairs: 1,
                hardware_transfer_attempts: 4,
            }
        );
    }

    #[test]
    fn selection_cannot_hide_a_misaligned_discarded_native_pair() {
        let (mut reader, logs) = scripted_reader(
            [
                vec![vec![0, 33_333, 66_667, 100_000]],
                vec![vec![0, 33_334, 66_667, 100_000]],
            ],
            true,
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let error = reader
            .next_selected_pair(
                PreviewSelection {
                    advance: NonZeroU32::new(4).unwrap(),
                    not_before: None,
                },
                cancel.clone(),
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains("not simultaneous"));
        assert!(logs.iter().all(|log| log.lock().unwrap().is_empty()));
        assert_eq!(reader.decode_stats().materialized_pairs, 0);
        assert_eq!(
            reader
                .seek_pair(Duration::ZERO, cancel)
                .unwrap()
                .unwrap()
                .timestamp_micros,
            0
        );
    }

    #[test]
    fn cancelled_native_prefetch_is_drained_before_a_recovery_seek() {
        let times = vec![vec![0, 33_333, 66_667]];
        let (mut reader, logs) = scripted_reader([times.clone(), times], true);
        let cancelled = Arc::new(AtomicBool::new(false));
        reader.prefetch_next(cancelled.clone()).unwrap();
        cancelled.store(true, Ordering::Relaxed);
        assert!(matches!(
            reader.next_selected_pair(PreviewSelection::default(), cancelled),
            Err(Error::Cancelled)
        ));
        assert!(logs.iter().all(|log| log.lock().unwrap().len() <= 1));
        let before = reader.decode_stats().hardware_transfer_attempts;
        let pair = reader
            .seek_pair(Duration::ZERO, Arc::new(AtomicBool::new(false)))
            .unwrap()
            .unwrap();
        assert_eq!(pair.timestamp_micros, 0);
        assert_eq!(reader.decode_stats().hardware_transfer_attempts, before + 2);
    }

    #[test]
    fn dispatch_returns_before_decode_completes_and_drains_both_error_replies() {
        let (a, a_requests, a_results) = worker_channels();
        let (b, b_requests, b_results) = worker_channels();
        let workers = [a, b];
        request_lenses(
            &workers,
            ReadRequest {
                chapter: 3,
                time: None,
                cancel: Arc::new(AtomicBool::new(false)),
                eager: false,
            },
            &mut PreviewDecodeStats::default(),
        )
        .unwrap();
        // No decoder has returned a frame. The caller can already render its
        // current pair while exactly one request waits at each lens worker.
        for requests in [&a_requests, &b_requests] {
            let WorkerRequest::Read(request) = requests.try_recv().unwrap() else {
                panic!("expected decode request")
            };
            assert_eq!(request.chapter, 3);
        }
        assert!(a_requests.try_recv().is_err());
        assert!(b_requests.try_recv().is_err());
        assert!(a_results.send(Err(invalid("A failed")).into()).is_ok());
        assert!(b_results.send(Ok(None).into()).is_ok());
        let [a, b] = receive_lenses(&workers, &mut PreviewDecodeStats::default(), false);
        assert!(a.is_err());
        assert!(matches!(b, Ok(None)));
        assert!(workers
            .iter()
            .all(|worker| worker.receiver.try_recv().is_err()));
    }

    #[test]
    fn partial_pair_dispatch_drains_the_started_lens() {
        let (a, a_requests, a_results) = worker_channels();
        let (b, b_requests, _b_results) = worker_channels();
        drop(b_requests);
        let responder = std::thread::spawn(move || {
            a_requests.recv().unwrap();
            assert!(a_results
                .send(WorkerReply {
                    result: Err(Error::Media("injected transfer failure".into())),
                    hardware_transfer_attempts: 1,
                })
                .is_ok());
        });
        let workers = [a, b];
        let mut stats = PreviewDecodeStats {
            hardware_transfer_attempts: 7,
            ..PreviewDecodeStats::default()
        };
        assert!(request_lenses(
            &workers,
            ReadRequest {
                chapter: 0,
                time: None,
                cancel: Arc::new(AtomicBool::new(false)),
                eager: true,
            },
            &mut stats,
        )
        .is_err());
        responder.join().unwrap();
        assert_eq!(
            stats.hardware_transfer_attempts, 8,
            "drained transfer is counted exactly once"
        );
        assert_eq!(stats.validated_pairs, 0);
        assert_eq!(stats.materialized_pairs, 0);
        assert_eq!(stats.speculative_pairs, 0);
        assert!(
            workers[0].receiver.try_recv().is_err(),
            "an incomplete dispatch cannot leave an old A reply queued"
        );
    }
}
