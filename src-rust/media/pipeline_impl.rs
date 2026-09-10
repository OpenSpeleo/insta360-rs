use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;
use image::ImageEncoder;
use tempfile::TempPath;

use crate::calibration::OffsetSource;
use crate::color::CubeLut;
use crate::container::{InsvInspection, InsvMetadata, RecordedColorMode};
use crate::{
    AudioPolicy, BackendReport, CalibrationResolver, ColorConversion, CpuStitcher,
    EffectiveBackend, EquirectangularProjection, Error, ExportResult, FrameSelection, GpuFailure,
    GpuFailureCode, GpuFailureStage, ImageExportOptions, ImageFormat, InputSet, LensFrame,
    MediaAcceleration, Orientation, PanoramaFrame, ProcessingBackend, ResolvedCalibration, Result,
    StitchConfig, VideoExportOptions,
};

use super::stabilization::FileStabilizer;
use super::{temporary_output_path, ExportContext, ExportEvent, ExportPhase, ExportProgress};
use crate::motion::FrameMotion;
use crate::stream::{allocate_video_frame, scale_video_frame};

const VIDEO_TRACKS: usize = 2;
const RGB_CHANNELS: usize = 3;
const MAX_SELECTIONS: usize = 1_000_000;
const PROGRESS_FRAME_INTERVAL: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendAttempt {
    Cpu,
    Gpu,
}

fn backend_attempts(requested: ProcessingBackend) -> &'static [BackendAttempt] {
    const CPU: &[BackendAttempt] = &[BackendAttempt::Cpu];
    const GPU: &[BackendAttempt] = &[BackendAttempt::Gpu];
    const AUTO: &[BackendAttempt] = &[BackendAttempt::Gpu, BackendAttempt::Cpu];

    match requested {
        ProcessingBackend::Auto => AUTO,
        ProcessingBackend::Cpu => CPU,
        ProcessingBackend::Gpu => GPU,
    }
}

fn take_gpu_failure(error: Error) -> Result<GpuFailure> {
    match error {
        Error::GpuUnavailable(failure) | Error::GpuProcessing(failure) => Ok(*failure),
        error => Err(error),
    }
}

fn run_with_backend_fallback<T>(
    requested: ProcessingBackend,
    context: &ExportContext,
    operation: &str,
    mut run_attempt: impl FnMut(ProcessingBackend, ProcessingBackend, Option<GpuFailure>) -> Result<T>,
) -> Result<T> {
    if requested != ProcessingBackend::Auto {
        return run_attempt(requested, requested, None);
    }

    match run_attempt(ProcessingBackend::Gpu, ProcessingBackend::Auto, None) {
        Ok(result) => Ok(result),
        Err(error) => {
            let failure = take_gpu_failure(error)?;
            context.emit(ExportEvent::Warning(format!(
                "GPU stitching failed; restarting the complete {operation} on CPU: {failure}"
            )));
            run_attempt(
                ProcessingBackend::Cpu,
                ProcessingBackend::Auto,
                Some(failure),
            )
        }
    }
}

pub(super) fn export_frames(
    inputs: InputSet,
    config: StitchConfig,
    output_dir: PathBuf,
    selection: FrameSelection,
    options: ImageExportOptions,
    context: ExportContext,
    started: Instant,
) -> Result<ExportResult> {
    let requested = config.backend;
    run_with_backend_fallback(
        requested,
        &context,
        "frame export",
        |selected, reported_request, fallback| {
            let mut attempt_config = config.clone();
            attempt_config.backend = selected;
            export_frames_attempt(
                inputs.clone(),
                attempt_config,
                output_dir.clone(),
                selection.clone(),
                options.clone(),
                context.clone(),
                started,
                reported_request,
                fallback,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn export_frames_attempt(
    inputs: InputSet,
    config: StitchConfig,
    output_dir: PathBuf,
    selection: FrameSelection,
    options: ImageExportOptions,
    context: ExportContext,
    started: Instant,
    requested: ProcessingBackend,
    fallback: Option<GpuFailure>,
) -> Result<ExportResult> {
    context.check_cancelled()?;
    validate_frame_export_config(&inputs, &options)?;
    config.underwater_color.validate_capabilities()?;
    ffmpeg::init().map_err(|error| media_error("initializing FFmpeg", error))?;
    let mut stitcher = StitchSession::select(config.backend, requested, fallback, &context)?;
    context.emit(ExportEvent::BackendSelected(Box::new(
        stitcher.report.clone(),
    )));

    let sequence = crate::RecordingSequence::single(inputs.clone())?;
    crate::paired::DecodedLayout::inspect(&sequence.chapters[0])?;
    validate_source_color(&sequence.chapters[0])?;
    let calibration = resolve_calibration(&sequence.chapters[0].inspection.metadata, &config)?;
    calibration.validate_for_stitching()?;
    let source = read_source(&inputs, &config)?;
    let mut underwater =
        underwater_color::UnderwaterProcessor::new(config.underwater_color, source.inspection.fps)?;
    stitcher.set_color_lut(resolve_color_lut(
        &source.inspection.metadata,
        config.color_conversion,
    )?);
    if let Some(stabilizer) = &source.stabilizer {
        context.emit(ExportEvent::StabilizationPrepared(stabilizer.diagnostics()));
        for warning in stabilizer.warnings() {
            context.emit(ExportEvent::Warning(warning.clone()));
        }
    }
    let mut selection = SelectionPlan::new(selection)?;
    let total = selection.len() as u64;

    std::fs::create_dir_all(&output_dir)
        .map_err(|error| crate::error::io_error(&output_dir, error))?;

    let extension = match options.format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
    };
    let mut outputs = Vec::with_capacity(selection.len());
    let mut created_outputs = CreatedOutputs::default();
    let mut last_progress_frame = 0_u64;
    let start = selection
        .first_seek_timestamp()
        .map(|time| Duration::from_micros(time as u64))
        .unwrap_or_default();
    let mut reader = crate::PairedReader::open(&sequence, start)?;
    let mut synchronized_index = 0_u64;
    while let Some(pair) = reader.next_pair(&context.cancel)? {
        let source_timestamp = pair.source_timestamp_micros;
        let pair = native_pair(pair);
        let media_time = duration_from_timestamp(pair.timestamp);
        if synchronized_index.saturating_sub(last_progress_frame) >= PROGRESS_FRAME_INTERVAL {
            emit_progress(
                &context,
                ExportPhase::Decoding,
                outputs.len() as u64,
                total,
                media_time,
                started,
            );
            last_progress_frame = synchronized_index;
        }

        let requested = selection.matches(synchronized_index, pair.timestamp)?;
        synchronized_index = synchronized_index
            .checked_add(1)
            .ok_or_else(|| Error::InvalidMedia("decoded frame index overflow".into()))?;
        if requested.is_empty() {
            continue;
        }

        let projection = output_projection(&config, &options, &pair)?;
        emit_progress(
            &context,
            ExportPhase::Stitching,
            outputs.len() as u64,
            total,
            media_time,
            started,
        );
        context.check_cancelled()?;
        let motion = stitch_motion(
            source.stabilizer.as_ref(),
            FrameTimestamp::Pts(source_timestamp),
        )?;
        let panorama = stitcher.stitch(pair, &calibration, projection, &motion)?;
        // Selected images are independent observations, even when adjacent.
        underwater.reset();
        let panorama = underwater.process(panorama, source_timestamp)?;
        context.check_cancelled()?;

        for output_key in requested {
            emit_progress(
                &context,
                ExportPhase::Encoding,
                outputs.len() as u64,
                total,
                media_time,
                started,
            );
            let output = output_dir.join(format!("frame_{output_key}.{extension}"));
            write_image_atomically(&output, &panorama, &options, &context)?;
            created_outputs.track(output.clone());
            outputs.push(output);
        }
        if selection.is_complete() {
            break;
        }
    }

    if !selection.is_complete() {
        return Err(Error::InvalidMedia(format!(
            "the recording ended before {} requested frame(s) could be selected",
            selection.remaining()
        )));
    }

    context.check_cancelled()?;
    emit_progress(
        &context,
        ExportPhase::Finalizing,
        outputs.len() as u64,
        total,
        None,
        started,
    );
    let result = ExportResult {
        frames_written: outputs.len() as u64,
        outputs,
        elapsed: started.elapsed(),
        backend: stitcher.report.clone(),
        optics: calibration.optical_resolution.clone(),
    };
    created_outputs.commit();
    Ok(result)
}

#[path = "audio.rs"]
mod audio;
#[path = "sequence_export.rs"]
mod sequence_export;
#[path = "underwater_color.rs"]
mod underwater_color;
pub(super) use sequence_export::preflight_video;

pub(super) fn export_video(
    inputs: crate::RecordingSequence,
    config: StitchConfig,
    output: PathBuf,
    options: VideoExportOptions,
    context: ExportContext,
    started: Instant,
) -> Result<ExportResult> {
    sequence_export::export_video(inputs, config, output, options, context, started)
}

fn validate_frame_export_config(_inputs: &InputSet, options: &ImageExportOptions) -> Result<()> {
    if matches!(options.format, ImageFormat::Jpeg) && !(1..=100).contains(&options.quality) {
        return Err(Error::InvalidMedia(
            "JPEG quality must be between 1 and 100".into(),
        ));
    }
    if options
        .scale_width
        .is_some_and(|width| width == 0 || width % 2 != 0)
    {
        return Err(Error::InvalidMedia(
            "image export width must be a non-zero even number for 2:1 output".into(),
        ));
    }
    Ok(())
}

fn validate_video_export_config(
    _inputs: &InputSet,
    output: &Path,
    options: &VideoExportOptions,
) -> Result<()> {
    if !(1..=100).contains(&options.quality) {
        return Err(Error::InvalidMedia(
            "HEVC quality must be between 1 and 100".into(),
        ));
    }
    if output.exists() {
        return Err(Error::InvalidMedia(format!(
            "refusing to overwrite existing output {}",
            output.display()
        )));
    }
    if let Some(projection) = options.projection {
        projection.validate()?;
        if projection.width % 2 != 0 || projection.height % 2 != 0 {
            return Err(Error::InvalidMedia(
                "HEVC YUV420 output dimensions must be even".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VideoRangeDecision {
    Before,
    Include(FrameTimestamp),
    End,
}

struct VideoRange {
    start_timestamp: i64,
    end_timestamp: Option<i64>,
    first_included_timestamp: Option<i64>,
}

impl VideoRange {
    fn new(
        start: Option<Duration>,
        duration: Option<Duration>,
        source_duration: Option<Duration>,
    ) -> Result<Self> {
        if duration.is_some_and(|duration| duration.is_zero()) {
            return Err(Error::InvalidMedia(
                "video export duration must be greater than zero".into(),
            ));
        }
        let start_timestamp = duration_to_i64_micros(start.unwrap_or_default())?;
        let end_timestamp = duration
            .map(duration_to_i64_micros)
            .transpose()?
            .map(|duration| {
                start_timestamp.checked_add(duration).ok_or_else(|| {
                    Error::InvalidMedia("video export interval timestamp overflowed".into())
                })
            })
            .transpose()?;
        if let Some(source_duration) = source_duration {
            let source_end = duration_to_i64_micros(source_duration)?;
            if start_timestamp >= source_end {
                return Err(Error::InvalidMedia(format!(
                    "video export start {:.6}s is outside the {:.6}s recording",
                    start_timestamp as f64 / 1_000_000.0,
                    source_end as f64 / 1_000_000.0,
                )));
            }
        }
        Ok(Self {
            start_timestamp,
            end_timestamp,
            first_included_timestamp: None,
        })
    }

    #[cfg(test)]
    fn seek_timestamp(&self) -> Option<i64> {
        (self.start_timestamp > 0).then_some(self.start_timestamp)
    }

    fn effective_duration(&self, source_duration: Option<Duration>) -> Option<Duration> {
        let source_end = source_duration.and_then(|duration| duration_to_i64_micros(duration).ok());
        let end = match (self.end_timestamp, source_end) {
            (Some(requested), Some(source)) => requested.min(source),
            (Some(requested), None) => requested,
            (None, Some(source)) => source,
            (None, None) => return None,
        };
        u64::try_from(end.saturating_sub(self.start_timestamp))
            .ok()
            .map(Duration::from_micros)
    }

    fn classify(&mut self, timestamp: FrameTimestamp) -> Result<VideoRangeDecision> {
        let FrameTimestamp::Pts(timestamp) = timestamp else {
            if self.start_timestamp != 0 || self.end_timestamp.is_some() {
                return Err(Error::InvalidMedia(
                    "timestamp-bounded video export requires presentation timestamps on both tracks"
                        .into(),
                ));
            }
            return Ok(VideoRangeDecision::Include(timestamp));
        };
        if timestamp < self.start_timestamp {
            return Ok(VideoRangeDecision::Before);
        }
        if self
            .end_timestamp
            .is_some_and(|end_timestamp| timestamp >= end_timestamp)
        {
            return Ok(VideoRangeDecision::End);
        }
        let first = *self.first_included_timestamp.get_or_insert(timestamp);
        let rebased = timestamp.checked_sub(first).ok_or_else(|| {
            Error::InvalidMedia("rebased video timestamp overflowed unexpectedly".into())
        })?;
        Ok(VideoRangeDecision::Include(FrameTimestamp::Pts(rebased)))
    }
}

struct SourceData {
    inspection: InsvInspection,
    stabilizer: Option<FileStabilizer>,
}

fn read_source(inputs: &InputSet, config: &StitchConfig) -> Result<SourceData> {
    let path = &inputs.paths()[0];
    let file = File::open(path).map_err(|error| crate::error::io_error(path, error))?;
    let mut reader = crate::InsvReader::new(file)?;
    let inspection = reader.inspect()?;
    let stabilizer = FileStabilizer::from_reader(&mut reader, &inspection, config)?;
    Ok(SourceData {
        inspection,
        stabilizer,
    })
}

fn stitch_motion(
    stabilizer: Option<&FileStabilizer>,
    timestamp: FrameTimestamp,
) -> Result<FrameMotion> {
    let Some(stabilizer) = stabilizer else {
        return FrameMotion::global(Orientation::IDENTITY);
    };
    let FrameTimestamp::Pts(pts_micros) = timestamp else {
        return Err(Error::InvalidMedia(
            "stabilization requires presentation timestamps".into(),
        ));
    };
    stabilizer.frame_motion(pts_micros)
}

// Keep calibration/container coupling in this adapter: the public calibration API is
// intentionally evolving while the media pipeline remains an independent consumer.
fn resolve_calibration(
    metadata: &InsvMetadata,
    config: &StitchConfig,
) -> Result<ResolvedCalibration> {
    CalibrationResolver::new(config.calibration_policy).resolve_metadata(
        metadata,
        &config.optical_selection(),
        OffsetSource::Current,
    )
}

fn unsupported_hdr() -> Error {
    Error::MissingCapability("stitched export produces 8-bit SDR output; Dolby, PQ and HLG input require a verified HDR tone mapper; original encoded extraction remains available".into())
}

fn validate_source_color(chapter: &crate::RecordingChapter) -> Result<()> {
    if chapter.inspection.metadata.recorded_color_mode == Some(RecordedColorMode::Dolby) {
        return Err(unsupported_hdr());
    }
    for path in chapter.inputs.paths() {
        let input = crate::stream::open_input(path)?;
        for stream in input
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
        {
            let parameters = stream.parameters();
            // SAFETY: the stream owns these parameters throughout this borrow.
            let transfer = unsafe { (*parameters.as_ptr()).color_trc };
            validate_transfer(transfer.into())?;
        }
    }
    Ok(())
}

fn validate_transfer(transfer: ffmpeg::util::color::TransferCharacteristic) -> Result<()> {
    use ffmpeg::util::color::TransferCharacteristic::{ARIB_STD_B67, SMPTE2084};
    if matches!(transfer, SMPTE2084 | ARIB_STD_B67) {
        Err(unsupported_hdr())
    } else {
        Ok(())
    }
}

fn resolve_color_lut(
    metadata: &InsvMetadata,
    conversion: ColorConversion,
) -> Result<Option<Arc<CubeLut>>> {
    // Studio's older X5 path recognizes this exact spelling in gamma_mode.
    // The legacy "log" string denotes a different curve and is insufficient.
    let gamma_is_ilog = metadata.gamma_mode.as_deref() == Some("I_Log");
    let mode_is_standard = matches!(
        metadata.recorded_color_mode,
        Some(RecordedColorMode::Standard | RecordedColorMode::Dolby)
    );
    let is_ilog = match metadata.recorded_color_mode {
        Some(RecordedColorMode::ILog) => true,
        None | Some(RecordedColorMode::Unknown) => gamma_is_ilog,
        _ => false,
    };
    match conversion {
        ColorConversion::Preserve => return Ok(None),
        _ if metadata.recorded_color_mode_invalid => {
            return Err(Error::InvalidMedia(
                "recording has malformed or conflicting capture color metadata; select Preserve to retain its encoding".into(),
            ));
        }
        ColorConversion::Auto if gamma_is_ilog && mode_is_standard => {
            return Err(Error::InvalidMedia(
                "recording has conflicting I-Log gamma and standard/Dolby color metadata".into(),
            ));
        }
        ColorConversion::Auto if !is_ilog => return Ok(None),
        ColorConversion::ILogToRec709 if mode_is_standard => {
            return Err(Error::InvalidMedia(
                "I-Log conversion conflicts with the recording's explicit standard/Dolby color mode"
                    .into(),
            ));
        }
        _ => {}
    }

    let camera = metadata
        .camera_name
        .as_deref()
        .and_then(crate::profile::camera_profile_for_name);
    if !camera.is_some_and(|profile| profile.camera == crate::CameraModel::X5) {
        return Err(Error::MissingCapability(
            "I-Log conversion currently requires an X5 recording; no matching camera LUT is selected"
                .into(),
        ));
    }
    CubeLut::load_bundled("studio-i-log-x5-rec709")
        .map(Arc::new)
        .map(Some)
}

/// One renderer for the complete lifetime of an export attempt.
///
/// Keeping conversion state in the session is important: decoded frames remain
/// in their original FFmpeg layout until the selected renderer consumes them.
/// A portable GPU session can therefore upload YUV planes directly without
/// changing the decoder or synchronizer again.
struct StitchSession {
    renderer: SessionRenderer,
    report: BackendReport,
    converters: [RgbFrameConverter; VIDEO_TRACKS],
    color_lut: Option<Arc<CubeLut>>,
    force_rgb: bool,
}

enum SessionRenderer {
    Cpu(CpuStitcher),
    #[cfg(feature = "gpu")]
    Gpu(Box<crate::gpu::GpuStitcher>),
}

enum StitchedVideoFrame {
    Rgb(PanoramaFrame),
    #[cfg(feature = "gpu")]
    Yuv420(crate::gpu::GpuYuv420Output),
}

impl StitchedVideoFrame {
    fn width(&self) -> u32 {
        match self {
            Self::Rgb(frame) => frame.width(),
            #[cfg(feature = "gpu")]
            Self::Yuv420(frame) => frame.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Rgb(frame) => frame.height(),
            #[cfg(feature = "gpu")]
            Self::Yuv420(frame) => frame.height(),
        }
    }

    fn is_gpu_yuv420(&self) -> bool {
        #[cfg(feature = "gpu")]
        {
            matches!(self, Self::Yuv420(_))
        }
        #[cfg(not(feature = "gpu"))]
        {
            false
        }
    }
}

impl StitchSession {
    fn select(
        selected: ProcessingBackend,
        requested: ProcessingBackend,
        initial_fallback: Option<GpuFailure>,
        context: &ExportContext,
    ) -> Result<Self> {
        let mut fallback = initial_fallback;
        for &attempt in backend_attempts(selected) {
            match Self::try_open(attempt, requested, fallback.clone()) {
                Ok(session) => return Ok(session),
                Err(failure) if selected == ProcessingBackend::Auto => {
                    context.emit(ExportEvent::Warning(format!(
                        "GPU stitching is unavailable; using the CPU renderer: {failure}"
                    )));
                    fallback = Some(*failure);
                }
                Err(failure) => return Err(Error::GpuUnavailable(failure)),
            }
        }

        Err(Error::MissingCapability(
            "no stitch renderer was available for the requested backend".into(),
        ))
    }

    fn try_open(
        attempt: BackendAttempt,
        requested: ProcessingBackend,
        fallback: Option<GpuFailure>,
    ) -> std::result::Result<Self, Box<GpuFailure>> {
        match attempt {
            BackendAttempt::Cpu => {
                let report = match fallback {
                    Some(failure) => BackendReport::cpu_fallback(requested, failure),
                    None => BackendReport::cpu(requested),
                };
                Ok(Self {
                    renderer: SessionRenderer::Cpu(CpuStitcher::new()),
                    report,
                    converters: std::array::from_fn(|_| RgbFrameConverter::default()),
                    color_lut: None,
                    force_rgb: false,
                })
            }
            BackendAttempt::Gpu => Self::open_gpu(requested),
        }
    }

    #[cfg(feature = "gpu")]
    fn open_gpu(requested: ProcessingBackend) -> std::result::Result<Self, Box<GpuFailure>> {
        let stitcher = crate::gpu::GpuStitcher::new().map_err(gpu_failure_from_error)?;
        let report = BackendReport::gpu(requested, stitcher.adapter_info().clone());
        Ok(Self {
            renderer: SessionRenderer::Gpu(Box::new(stitcher)),
            report,
            converters: std::array::from_fn(|_| RgbFrameConverter::default()),
            color_lut: None,
            force_rgb: false,
        })
    }

    #[cfg(not(feature = "gpu"))]
    fn open_gpu(_requested: ProcessingBackend) -> std::result::Result<Self, Box<GpuFailure>> {
        Err(gpu_session_unavailable())
    }

    fn set_color_lut(&mut self, lut: Option<Arc<CubeLut>>) {
        #[cfg(feature = "gpu")]
        if let SessionRenderer::Gpu(stitcher) = &mut self.renderer {
            stitcher.set_color_lut(lut.clone());
        }
        self.color_lut = lut;
    }

    fn stitch(
        &mut self,
        pair: SynchronizedPair,
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<PanoramaFrame> {
        for frame in &pair.frames {
            validate_transfer(frame.format.color.transfer)?;
        }
        debug_assert_eq!(
            self.report.selected,
            match &self.renderer {
                SessionRenderer::Cpu(_) => EffectiveBackend::Cpu,
                #[cfg(feature = "gpu")]
                SessionRenderer::Gpu(_) => EffectiveBackend::Gpu,
            }
        );
        match &self.renderer {
            SessionRenderer::Cpu(stitcher) => {
                let [first, second] = pair.frames;
                let lenses = [
                    self.converters[0].convert(&first)?,
                    self.converters[1].convert(&second)?,
                ];
                let panorama =
                    stitcher.stitch_with_motion(&lenses, calibration, projection, motion)?;
                if let Some(lut) = &self.color_lut {
                    let mut rgb = panorama.into_rgb8();
                    lut.apply_rgb8(&mut rgb)?;
                    PanoramaFrame::new(projection.width, projection.height, rgb)
                } else {
                    Ok(panorama)
                }
            }
            #[cfg(feature = "gpu")]
            SessionRenderer::Gpu(stitcher) => stitch_decoded_gpu(
                stitcher,
                &mut self.converters,
                pair,
                calibration,
                projection,
                motion,
            ),
        }
    }

    fn stitch_video(
        &mut self,
        pair: SynchronizedPair,
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<StitchedVideoFrame> {
        for frame in &pair.frames {
            validate_transfer(frame.format.color.transfer)?;
        }
        if self.force_rgb {
            return self
                .stitch(pair, calibration, projection, motion)
                .map(StitchedVideoFrame::Rgb);
        }
        match &self.renderer {
            SessionRenderer::Cpu(_) => self
                .stitch(pair, calibration, projection, motion)
                .map(StitchedVideoFrame::Rgb),
            #[cfg(feature = "gpu")]
            SessionRenderer::Gpu(stitcher) => stitch_decoded_gpu_video(
                stitcher,
                &mut self.converters,
                pair,
                calibration,
                projection,
                motion,
            ),
        }
    }

    fn recycle_video_frame(&self, frame: StitchedVideoFrame) {
        match (&self.renderer, frame) {
            #[cfg(feature = "gpu")]
            (SessionRenderer::Gpu(stitcher), StitchedVideoFrame::Yuv420(frame)) => {
                stitcher.recycle_yuv420_output(frame);
            }
            _ => {}
        }
    }
}

#[cfg(feature = "gpu")]
fn stitch_decoded_gpu(
    stitcher: &crate::gpu::GpuStitcher,
    converters: &mut [RgbFrameConverter; VIDEO_TRACKS],
    pair: SynchronizedPair,
    calibration: &ResolvedCalibration,
    projection: EquirectangularProjection,
    motion: &FrameMotion,
) -> Result<PanoramaFrame> {
    let [first, second] = pair.frames;
    if let (Some(first_yuv), Some(second_yuv)) = (first.gpu_yuv420()?, second.gpu_yuv420()?) {
        return stitcher.stitch_yuv420_with_motion(
            &[first_yuv, second_yuv],
            calibration,
            projection,
            motion,
        );
    }

    let lenses = [
        converters[0].convert(&first)?,
        converters[1].convert(&second)?,
    ];
    stitcher.stitch_with_motion(&lenses, calibration, projection, motion)
}

#[cfg(feature = "gpu")]
fn stitch_decoded_gpu_video(
    stitcher: &crate::gpu::GpuStitcher,
    converters: &mut [RgbFrameConverter; VIDEO_TRACKS],
    pair: SynchronizedPair,
    calibration: &ResolvedCalibration,
    projection: EquirectangularProjection,
    motion: &FrameMotion,
) -> Result<StitchedVideoFrame> {
    let [first, second] = pair.frames;
    if let (Some(first_yuv), Some(second_yuv)) = (first.gpu_yuv420()?, second.gpu_yuv420()?) {
        return stitcher
            .stitch_yuv420_to_yuv420_with_motion(
                &[first_yuv, second_yuv],
                calibration,
                projection,
                motion,
            )
            .map(StitchedVideoFrame::Yuv420);
    }

    let lenses = [
        converters[0].convert(&first)?,
        converters[1].convert(&second)?,
    ];
    stitcher
        .stitch_with_motion(&lenses, calibration, projection, motion)
        .map(StitchedVideoFrame::Rgb)
}

#[cfg(not(feature = "gpu"))]
fn gpu_session_unavailable() -> Box<GpuFailure> {
    Box::new(GpuFailure::new(
        GpuFailureCode::NotCompiled,
        GpuFailureStage::Discovery,
        "insta360-rs was built without the gpu feature",
    ))
}

#[cfg(feature = "gpu")]
fn gpu_failure_from_error(error: Error) -> Box<GpuFailure> {
    match error {
        Error::GpuUnavailable(failure) | Error::GpuProcessing(failure) => failure,
        error => Box::new(GpuFailure::new(
            GpuFailureCode::DeviceRequest,
            GpuFailureStage::Preparation,
            error.to_string(),
        )),
    }
}

#[derive(Default)]
struct RgbFrameConverter {
    scaler: Option<ffmpeg::software::scaling::Context>,
    color: Option<VideoColorInfo>,
}

impl RgbFrameConverter {
    fn convert(&mut self, decoded: &DecodedVideoFrame) -> Result<LensFrame> {
        decoded.validate_layout()?;
        let format = &decoded.format;
        let needs_scaler = self.color != Some(format.color)
            || self.scaler.as_ref().is_none_or(|scaler| {
                let input = scaler.input();
                input.format != format.pixel_format
                    || input.width != format.width
                    || input.height != format.height
            });
        if needs_scaler {
            let mut scaler = ffmpeg::software::scaling::Context::get(
                format.pixel_format,
                format.width,
                format.height,
                ffmpeg::format::Pixel::RGB24,
                format.width,
                format.height,
                ffmpeg::software::scaling::Flags::BILINEAR,
            )
            .map_err(|error| media_error("creating the RGB conversion context", error))?;
            let source_matrix = match format.color.matrix {
                ffmpeg::util::color::Space::BT709 | ffmpeg::util::color::Space::Unspecified => {
                    ffmpeg::ffi::SWS_CS_ITU709
                }
                space => crate::stream::preview_matrix(space)?,
            };
            let full_range = match format.color.range {
                ffmpeg::util::color::Range::JPEG => true,
                ffmpeg::util::color::Range::MPEG => false,
                ffmpeg::util::color::Range::Unspecified => matches!(
                    format.pixel_format,
                    ffmpeg::format::Pixel::YUVJ420P
                        | ffmpeg::format::Pixel::YUVJ422P
                        | ffmpeg::format::Pixel::YUVJ444P
                ),
            };
            set_scaler_color(&mut scaler, source_matrix, full_range, true)?;
            self.scaler = Some(scaler);
            self.color = Some(format.color);
        }

        let mut rgb =
            allocate_video_frame(ffmpeg::format::Pixel::RGB24, format.width, format.height)?;
        scale_video_frame(
            self.scaler.as_mut().expect("scaler initialized above"),
            &decoded.frame,
            &mut rgb,
        )?;
        LensFrame::new(rgb.width(), rgb.height(), copy_tightly_packed_rgb(&rgb)?)
    }
}

fn set_scaler_color(
    scaler: &mut ffmpeg::software::scaling::Context,
    matrix: i32,
    source_full_range: bool,
    output_full_range: bool,
) -> Result<()> {
    // SAFETY: Context::get created a live, exclusively borrowed SwsContext.
    // sws_getCoefficients returns FFmpeg's static four-coefficient table; this
    // call copies it into the context and retains no Rust-owned pointers.
    let status = unsafe {
        let coefficients = ffmpeg::ffi::sws_getCoefficients(matrix);
        ffmpeg::ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            coefficients,
            i32::from(source_full_range),
            coefficients,
            i32::from(output_full_range),
            0,
            1 << 16,
            1 << 16,
        )
    };
    if status < 0 {
        return Err(media_error(
            "configuring the color conversion matrix",
            ffmpeg::Error::from(status),
        ));
    }
    Ok(())
}

fn rgb_to_yuv_scaler(
    width: u32,
    height: u32,
    rec709: bool,
) -> Result<ffmpeg::software::scaling::Context> {
    let mut scaler = ffmpeg::software::scaling::Context::get(
        ffmpeg::format::Pixel::RGB24,
        width,
        height,
        ffmpeg::format::Pixel::YUV420P,
        width,
        height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .map_err(|error| media_error("creating the HEVC color converter", error))?;
    if rec709 {
        set_scaler_color(&mut scaler, ffmpeg::ffi::SWS_CS_ITU709, true, false)?;
    }
    Ok(scaler)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameTimestamp {
    Pts(i64),
    // Only writer/selection unit tests construct a missing-timestamp value.
    #[allow(dead_code)]
    Sequence(u64),
}

struct SynchronizedPair {
    timestamp: FrameTimestamp,
    frames: [DecodedVideoFrame; VIDEO_TRACKS],
}

impl SynchronizedPair {
    fn source_dimensions(&self) -> Result<(u32, u32)> {
        let first = &self.frames[0].format;
        let second = &self.frames[1].format;
        if first.width != second.width || first.height != second.height {
            return Err(Error::InvalidMedia(format!(
                "dual video tracks have different decoded sizes: {}x{} and {}x{}",
                first.width, first.height, second.width, second.height
            )));
        }
        Ok((first.width, first.height))
    }
}

/// Plane and color description captured while the owning AVFrame is alive.
///
/// This is deliberately independent from the CPU `LensFrame`: a future GPU
/// importer can inspect and upload each original plane without another decode
/// or an eager RGB conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DecodedVideoFormat {
    pixel_format: ffmpeg::format::Pixel,
    width: u32,
    height: u32,
    planes: Vec<DecodedPlaneLayout>,
    color: VideoColorInfo,
}

impl DecodedVideoFormat {
    fn from_frame(frame: &ffmpeg::frame::Video) -> Self {
        let planes = (0..frame.planes())
            .map(|index| DecodedPlaneLayout {
                width: frame.plane_width(index),
                height: frame.plane_height(index),
                stride: frame.stride(index),
            })
            .collect();
        Self {
            pixel_format: frame.format(),
            width: frame.width(),
            height: frame.height(),
            planes,
            color: VideoColorInfo {
                range: frame.color_range(),
                matrix: frame.color_space(),
                primaries: frame.color_primaries(),
                transfer: frame.color_transfer_characteristic(),
                chroma_location: frame.chroma_location(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecodedPlaneLayout {
    width: u32,
    height: u32,
    stride: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VideoColorInfo {
    range: ffmpeg::util::color::Range,
    matrix: ffmpeg::util::color::Space,
    primaries: ffmpeg::util::color::Primaries,
    transfer: ffmpeg::util::color::TransferCharacteristic,
    chroma_location: ffmpeg::util::chroma::Location,
}

struct DecodedVideoFrame {
    // The AVFrame is ref-counted by FFmpeg. Keeping it here retains both
    // software plane buffers and any future native hardware-surface payload.
    frame: ffmpeg::frame::Video,
    format: DecodedVideoFormat,
}

impl DecodedVideoFrame {
    fn new(frame: ffmpeg::frame::Video) -> Self {
        let format = DecodedVideoFormat::from_frame(&frame);
        Self { frame, format }
    }

    fn validate_layout(&self) -> Result<()> {
        let current = DecodedVideoFormat::from_frame(&self.frame);
        if current != self.format {
            return Err(Error::InvalidMedia(
                "decoded frame storage changed while it was queued".into(),
            ));
        }
        Ok(())
    }

    #[cfg(feature = "gpu")]
    fn gpu_yuv420(&self) -> Result<Option<crate::gpu::GpuYuv420Frame<'_>>> {
        use crate::gpu::{GpuChromaLocation, GpuPlane, GpuYuv420Frame, GpuYuvMatrix, GpuYuvRange};

        self.validate_layout()?;
        let is_full_range_format = self.format.pixel_format == ffmpeg::format::Pixel::YUVJ420P;
        if self.format.pixel_format != ffmpeg::format::Pixel::YUV420P && !is_full_range_format {
            return Ok(None);
        }
        if self.format.planes.len() < 3 {
            return Err(Error::InvalidMedia(
                "decoded planar YUV420 frame has fewer than three planes".into(),
            ));
        }
        let range = match self.format.color.range {
            ffmpeg::util::color::Range::JPEG => GpuYuvRange::Full,
            ffmpeg::util::color::Range::MPEG => GpuYuvRange::Limited,
            ffmpeg::util::color::Range::Unspecified if is_full_range_format => GpuYuvRange::Full,
            ffmpeg::util::color::Range::Unspecified => GpuYuvRange::Limited,
        };
        let matrix = match self.format.color.matrix {
            ffmpeg::util::color::Space::BT709 | ffmpeg::util::color::Space::Unspecified => {
                GpuYuvMatrix::Bt709
            }
            ffmpeg::util::color::Space::BT470BG | ffmpeg::util::color::Space::SMPTE170M => {
                GpuYuvMatrix::Bt601
            }
            ffmpeg::util::color::Space::BT2020NCL => GpuYuvMatrix::Bt2020,
            _ => return Ok(None),
        };
        let chroma_location = match self.format.color.chroma_location {
            ffmpeg::util::chroma::Location::Center => GpuChromaLocation::Center,
            ffmpeg::util::chroma::Location::Left => GpuChromaLocation::Left,
            ffmpeg::util::chroma::Location::Unspecified if is_full_range_format => {
                GpuChromaLocation::Center
            }
            ffmpeg::util::chroma::Location::Unspecified => GpuChromaLocation::Left,
            _ => return Ok(None),
        };
        Ok(Some(GpuYuv420Frame {
            width: self.format.width,
            height: self.format.height,
            y: GpuPlane {
                data: self.frame.data(0),
                stride: self.format.planes[0].stride,
            },
            u: GpuPlane {
                data: self.frame.data(1),
                stride: self.format.planes[1].stride,
            },
            v: GpuPlane {
                data: self.frame.data(2),
                stride: self.format.planes[2].stride,
            },
            range,
            matrix,
            chroma_location,
        }))
    }
}

fn native_pair(pair: crate::FramePair) -> SynchronizedPair {
    SynchronizedPair {
        timestamp: FrameTimestamp::Pts(pair.timestamp_micros),
        frames: [
            DecodedVideoFrame::new(pair.a),
            DecodedVideoFrame::new(pair.b),
        ],
    }
}

fn copy_tightly_packed_rgb(frame: &ffmpeg::frame::Video) -> Result<Vec<u8>> {
    let row_len = usize::try_from(frame.width())
        .ok()
        .and_then(|width| width.checked_mul(RGB_CHANNELS))
        .ok_or_else(|| Error::InvalidMedia("RGB row size overflowed".into()))?;
    let height = usize::try_from(frame.height())
        .map_err(|_| Error::InvalidMedia("RGB frame height does not fit usize".into()))?;
    if frame.stride(0) < row_len {
        return Err(Error::InvalidMedia(format!(
            "FFmpeg RGB stride {} is smaller than the {row_len}-byte row",
            frame.stride(0)
        )));
    }
    let output_len = row_len
        .checked_mul(height)
        .ok_or_else(|| Error::InvalidMedia("RGB frame size overflowed".into()))?;
    let mut output = Vec::with_capacity(output_len);
    for row in 0..height {
        let start = row
            .checked_mul(frame.stride(0))
            .ok_or_else(|| Error::InvalidMedia("RGB source offset overflowed".into()))?;
        let end = start
            .checked_add(row_len)
            .ok_or_else(|| Error::InvalidMedia("RGB source row overflowed".into()))?;
        let source = frame.data(0).get(start..end).ok_or_else(|| {
            Error::InvalidMedia("FFmpeg returned a truncated RGB frame plane".into())
        })?;
        output.extend_from_slice(source);
    }
    Ok(output)
}

fn output_projection(
    config: &StitchConfig,
    options: &ImageExportOptions,
    pair: &SynchronizedPair,
) -> Result<EquirectangularProjection> {
    let (source_width, _) = pair.source_dimensions()?;
    let projection = if let Some(width) = options.scale_width {
        EquirectangularProjection {
            width,
            height: width / 2,
        }
    } else if let Some(projection) = config.projection {
        projection
    } else {
        EquirectangularProjection {
            width: source_width
                .checked_mul(2)
                .ok_or_else(|| Error::InvalidMedia("default panorama width overflowed".into()))?,
            height: source_width,
        }
    };
    projection.validate()
}

fn video_projection(
    config: &StitchConfig,
    options: &VideoExportOptions,
    pair: &SynchronizedPair,
) -> Result<EquirectangularProjection> {
    let (source_width, _) = pair.source_dimensions()?;
    let projection = options
        .projection
        .or(config.projection)
        .unwrap_or(EquirectangularProjection {
            width: source_width
                .checked_mul(2)
                .ok_or_else(|| Error::InvalidMedia("default panorama width overflowed".into()))?,
            height: source_width,
        })
        .validate()?;
    if projection.width % 2 != 0 || projection.height % 2 != 0 {
        return Err(Error::InvalidMedia(
            "HEVC YUV420 output dimensions must be even".into(),
        ));
    }
    Ok(projection)
}

struct HevcWriter {
    output: Option<ffmpeg::format::context::Output>,
    encoder: ffmpeg::encoder::Video,
    scaler: Option<ffmpeg::software::scaling::Context>,
    width: u32,
    height: u32,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    frame_rate: f64,
    temporary: TempPath,
    final_path: PathBuf,
    rec709_output: bool,
    is_x265: bool,
    frames_submitted: u64,
    flushing: bool,
    audio_layout: Option<audio::AudioLayout>,
    audio_indices: Vec<usize>,
    audio_last_dts: Vec<Option<i64>>,
    encoder_report: super::EncoderReport,
}

#[derive(Clone, Copy)]
struct HevcEncodingPolicy {
    acceleration: MediaAcceleration,
    gpu_stitching: bool,
    direct_bt709_yuv: bool,
    converted_rec709: bool,
}

impl HevcWriter {
    #[cfg(test)]
    fn new(
        final_path: &Path,
        width: u32,
        height: u32,
        frame_rate: f64,
        quality: u8,
        policy: HevcEncodingPolicy,
    ) -> Result<Self> {
        Self::new_with_audio(final_path, width, height, frame_rate, quality, policy, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_audio(
        final_path: &Path,
        width: u32,
        height: u32,
        frame_rate: f64,
        quality: u8,
        policy: HevcEncodingPolicy,
        audio_layout: Option<audio::AudioLayout>,
    ) -> Result<Self> {
        let candidates = hevc_encoder_candidates(policy.acceleration, policy.gpu_stitching);
        if candidates.is_empty() {
            return Err(Error::MissingCapability(match policy.acceleration {
                MediaAcceleration::Hardware => {
                    "the FFmpeg runtime provides no hardware HEVC encoder".into()
                }
                MediaAcceleration::Software => {
                    "the FFmpeg runtime provides no software HEVC encoder".into()
                }
                MediaAcceleration::Auto => {
                    "the FFmpeg runtime provides no usable HEVC encoder".into()
                }
            }));
        }

        let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| crate::error::io_error(parent, error))?;
        let temporary_path = temporary_output_path(final_path);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|error| crate::error::io_error(&temporary_path, error))?;
        let temporary = TempPath::try_from_path(temporary_path.clone())
            .map_err(|error| crate::error::io_error(&temporary_path, error))?;
        let mut output = ffmpeg::format::output_as(&temporary_path, "mp4")
            .map_err(|error| media_error("creating the MP4 muxer", error))?;
        let global_header = output
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER);
        let time_base = ffmpeg::util::mathematics::rescale::TIME_BASE;
        let rate = ffmpeg::Rational::from(frame_rate);

        let (codec, encoder) = match try_in_order(candidates, |codec| {
            Self::open_encoder(
                *codec,
                width,
                height,
                frame_rate,
                quality,
                global_header,
                time_base,
                rate,
                policy,
            )
        }) {
            Ok(opened) => opened,
            Err(failures) => {
                let details = failures
                    .into_iter()
                    .map(|(codec, error)| format!("{}: {error}", codec.name()))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(Error::Media(format!(
                    "opening every eligible HEVC encoder failed: {details}"
                )));
            }
        };

        let stream_index = {
            let mut stream = output
                .add_stream(codec)
                .map_err(|error| media_error("adding the HEVC MP4 stream", error))?;
            let stream_index = stream.index();
            stream.set_time_base(time_base);
            stream.set_rate(rate);
            stream.set_avg_frame_rate(rate);
            stream.set_parameters(&encoder);
            stream_index
        };
        let audio_indices = audio_layout
            .as_ref()
            .map(|layout| layout.add_output_streams(&mut output))
            .transpose()?
            .unwrap_or_default();
        // MP4 edit lists express track offsets in the movie time base. Match
        // the video clock so the default millisecond scale does not round away
        // the copied audio's sub-millisecond delay after a cut.
        let mut muxer_options = ffmpeg::Dictionary::new();
        muxer_options.set("movie_timescale", &time_base.denominator().to_string());
        output
            .write_header_with(muxer_options)
            .map_err(|error| media_error("writing the MP4 header", error))?;
        let scaler = if policy.direct_bt709_yuv {
            None
        } else {
            Some(rgb_to_yuv_scaler(width, height, policy.converted_rec709)?)
        };
        Ok(Self {
            output: Some(output),
            encoder,
            scaler,
            width,
            height,
            stream_index,
            time_base,
            frame_rate,
            temporary,
            final_path: final_path.to_path_buf(),
            rec709_output: policy.converted_rec709,
            is_x265: codec.name() == "libx265",
            frames_submitted: 0,
            flushing: false,
            audio_last_dts: vec![None; audio_indices.len()],
            encoder_report: super::EncoderReport {
                name: codec.name().to_owned(),
                hardware: !matches!(codec.name(), "libx265" | "libkvazaar"),
                audio_tracks: audio_indices.len(),
            },
            audio_layout,
            audio_indices,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn open_encoder(
        codec: ffmpeg::Codec,
        width: u32,
        height: u32,
        frame_rate: f64,
        quality: u8,
        global_header: bool,
        time_base: ffmpeg::Rational,
        rate: ffmpeg::Rational,
        policy: HevcEncodingPolicy,
    ) -> Result<(ffmpeg::Codec, ffmpeg::encoder::Video)> {
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|error| media_error("creating the HEVC encoder", error))?;
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg::format::Pixel::YUV420P);
        encoder.set_time_base(time_base);
        encoder.set_frame_rate(Some(rate));
        encoder.set_gop((frame_rate * 2.0).round().clamp(1.0, u32::MAX as f64) as u32);
        if policy.direct_bt709_yuv || policy.converted_rec709 {
            encoder.set_colorspace(ffmpeg::util::color::Space::BT709);
            encoder.set_color_range(ffmpeg::util::color::Range::MPEG);
        }
        // The YUV matrix does not establish the RGB transfer curve or gamut.
        // In particular, preserved I-Log must not be tagged as converted Rec.709.
        if policy.converted_rec709 {
            encoder.set_color_primaries(ffmpeg::util::color::Primaries::BT709);
            encoder.set_color_transfer_characteristic(
                ffmpeg::util::color::TransferCharacteristic::BT709,
            );
        }
        if global_header {
            encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }

        let encoder = if codec.name() == "libx265" {
            let mut settings = ffmpeg::Dictionary::new();
            settings.set("preset", "medium");
            let crf = (40.0 - f64::from(quality) * 0.28).round().clamp(12.0, 40.0);
            settings.set("crf", &format!("{crf:.0}"));
            encoder
                .open_as_with(codec, settings)
                .map_err(|error| media_error("opening libx265", error))?
        } else {
            encoder.set_bit_rate(quality_target_bitrate(width, height, frame_rate, quality));
            encoder
                .open_as(codec)
                .map_err(|error| media_error("opening the HEVC encoder", error))?
        };
        Ok((codec, encoder))
    }
}

fn quality_target_bitrate(width: u32, height: u32, frame_rate: f64, quality: u8) -> usize {
    const REFERENCE_PIXELS: f64 = 1920.0 * 960.0;
    const SPATIAL_SCALING_EXPONENT: f64 = 0.62;

    let frame_pixels = f64::from(width) * f64::from(height);
    let spatial_scale = (frame_pixels / REFERENCE_PIXELS).powf(SPATIAL_SCALING_EXPONENT);
    let normalized_quality = f64::from(quality) / 100.0;
    // Hardware encoders generally expose bitrate rather than a portable CRF.
    // Weight the upper end heavily: photogrammetry presets must retain texture,
    // while low quality values should still produce genuinely compact files.
    // Resolution scales sublinearly because larger frames compress more
    // efficiently; linear bits-per-pixel scaling badly overshoots at 5.7K/8K.
    let bits_per_pixel = 0.02 + normalized_quality.powi(4) * 1.2;
    (REFERENCE_PIXELS * frame_rate * spatial_scale * bits_per_pixel)
        .round()
        .clamp(100_000.0, usize::MAX as f64) as usize
}

const HARDWARE_HEVC_ENCODERS: &[&str] = &[
    "hevc_videotoolbox",
    "hevc_mf",
    "hevc_nvenc",
    "hevc_amf",
    "hevc_vaapi",
];
const SOFTWARE_HEVC_ENCODERS: &[&str] = &["libx265", "libkvazaar"];

fn hevc_encoder_priority(
    acceleration: MediaAcceleration,
    gpu_stitching: bool,
) -> Vec<&'static str> {
    match (acceleration, gpu_stitching) {
        (MediaAcceleration::Hardware, _) => HARDWARE_HEVC_ENCODERS.to_vec(),
        (MediaAcceleration::Software, _) => SOFTWARE_HEVC_ENCODERS.to_vec(),
        (MediaAcceleration::Auto, true) => HARDWARE_HEVC_ENCODERS
            .iter()
            .chain(SOFTWARE_HEVC_ENCODERS)
            .copied()
            .collect(),
        (MediaAcceleration::Auto, false) => SOFTWARE_HEVC_ENCODERS
            .iter()
            .chain(HARDWARE_HEVC_ENCODERS)
            .copied()
            .collect(),
    }
}

fn hevc_encoder_candidates(
    acceleration: MediaAcceleration,
    gpu_stitching: bool,
) -> Vec<ffmpeg::Codec> {
    let mut candidates = hevc_encoder_priority(acceleration, gpu_stitching)
        .into_iter()
        .filter_map(ffmpeg::encoder::find_by_name)
        .collect::<Vec<_>>();
    if acceleration == MediaAcceleration::Auto {
        if let Some(codec) = ffmpeg::encoder::find(ffmpeg::codec::Id::HEVC) {
            if candidates
                .iter()
                .all(|candidate| candidate.name() != codec.name())
            {
                candidates.push(codec);
            }
        }
    }
    candidates
}

fn try_in_order<C, T, E>(
    candidates: impl IntoIterator<Item = C>,
    mut attempt: impl FnMut(&C) -> std::result::Result<T, E>,
) -> std::result::Result<T, Vec<(C, E)>> {
    let mut failures = Vec::new();
    for candidate in candidates {
        match attempt(&candidate) {
            Ok(value) => return Ok(value),
            Err(error) => failures.push((candidate, error)),
        }
    }
    Err(failures)
}

impl HevcWriter {
    fn write_audio(
        &mut self,
        chapter_index: usize,
        mut packet: ffmpeg::Packet,
        origin_micros: i64,
        end_micros: i64,
    ) -> Result<()> {
        let Some(layout) = &self.audio_layout else {
            return Ok(());
        };
        let chapter = layout.chapters.get(chapter_index).ok_or_else(|| {
            Error::InvalidMedia("audio packet refers to an unknown chapter".into())
        })?;
        let track_index = chapter
            .tracks
            .iter()
            .position(|track| track.source_index == packet.stream())
            .ok_or_else(|| Error::InvalidMedia("audio packet refers to an unknown track".into()))?;
        let output_index = self.audio_indices[track_index];
        let output = self
            .output
            .as_mut()
            .expect("writer remains open until finalization");
        let time_base = output
            .stream(output_index)
            .expect("created audio stream")
            .time_base();
        if !audio::prepare_packet(
            chapter,
            &mut packet,
            time_base,
            output_index,
            origin_micros,
            end_micros,
        )? {
            return Ok(());
        }
        let dts = packet.dts().expect("validated audio DTS");
        if self.audio_last_dts[track_index].is_some_and(|previous| previous >= dts) {
            return Err(Error::InvalidMedia(
                "audio timestamps overlap or move backwards across chapters".into(),
            ));
        }
        self.audio_last_dts[track_index] = Some(dts);
        packet
            .write_interleaved(output)
            .map_err(|error| media_error("muxing original audio", error))
    }

    fn write(
        &mut self,
        panorama: &PanoramaFrame,
        timestamp: FrameTimestamp,
        frame_index: u64,
    ) -> Result<()> {
        if panorama.width() != self.width || panorama.height() != self.height {
            return Err(Error::InvalidMedia(
                "stitched video frame dimensions changed during export".into(),
            ));
        }
        let mut rgb = allocate_video_frame(
            ffmpeg::format::Pixel::RGB24,
            panorama.width(),
            panorama.height(),
        )?;
        copy_rgb_into_frame(panorama.as_rgb8(), &mut rgb)?;
        let mut yuv = allocate_video_frame(
            ffmpeg::format::Pixel::YUV420P,
            panorama.width(),
            panorama.height(),
        )?;
        scale_video_frame(
            self.scaler.as_mut().ok_or_else(|| {
                Error::InvalidMedia("RGB frame sent to a direct-YUV HEVC writer".into())
            })?,
            &rgb,
            &mut yuv,
        )?;
        if self.rec709_output {
            yuv.set_color_range(ffmpeg::util::color::Range::MPEG);
            yuv.set_color_space(ffmpeg::util::color::Space::BT709);
            yuv.set_color_primaries(ffmpeg::util::color::Primaries::BT709);
            yuv.set_color_transfer_characteristic(
                ffmpeg::util::color::TransferCharacteristic::BT709,
            );
        }
        let pts = match timestamp {
            FrameTimestamp::Pts(value) => value,
            FrameTimestamp::Sequence(_) => {
                ((frame_index as f64 / self.frame_rate) * 1_000_000.0).round() as i64
            }
        };
        yuv.set_pts(Some(pts));
        self.encoder
            .send_frame(&yuv)
            .map_err(|error| media_error("sending a frame to the HEVC encoder", error))?;
        self.frames_submitted += 1;
        self.drain_packets()
    }

    fn write_stitched(
        &mut self,
        frame: &StitchedVideoFrame,
        timestamp: FrameTimestamp,
        frame_index: u64,
    ) -> Result<()> {
        match frame {
            StitchedVideoFrame::Rgb(panorama) => self.write(panorama, timestamp, frame_index),
            #[cfg(feature = "gpu")]
            StitchedVideoFrame::Yuv420(yuv) => self.write_yuv420(yuv, timestamp, frame_index),
        }
    }

    #[cfg(feature = "gpu")]
    fn write_yuv420(
        &mut self,
        source: &crate::gpu::GpuYuv420Output,
        timestamp: FrameTimestamp,
        frame_index: u64,
    ) -> Result<()> {
        if source.width() != self.width || source.height() != self.height {
            return Err(Error::InvalidMedia(
                "stitched GPU video frame dimensions changed during export".into(),
            ));
        }
        let mut yuv = allocate_video_frame(
            ffmpeg::format::Pixel::YUV420P,
            source.width(),
            source.height(),
        )?;
        yuv.set_color_range(ffmpeg::util::color::Range::MPEG);
        yuv.set_color_space(ffmpeg::util::color::Space::BT709);
        if self.rec709_output {
            yuv.set_color_primaries(ffmpeg::util::color::Primaries::BT709);
            yuv.set_color_transfer_characteristic(
                ffmpeg::util::color::TransferCharacteristic::BT709,
            );
        }
        let source_planes = source.planes();
        let widths = [source.width(), source.width() / 2, source.width() / 2];
        let heights = [source.height(), source.height() / 2, source.height() / 2];
        for plane_index in 0..3 {
            let row_len = widths[plane_index] as usize;
            let rows = heights[plane_index] as usize;
            let destination_stride = yuv.stride(plane_index);
            if destination_stride < row_len || source_planes[plane_index].stride < row_len {
                return Err(Error::InvalidMedia(
                    "YUV420 plane stride is smaller than its visible row".into(),
                ));
            }
            for row in 0..rows {
                let source_start = row * source_planes[plane_index].stride;
                let destination_start = row * destination_stride;
                yuv.data_mut(plane_index)[destination_start..destination_start + row_len]
                    .copy_from_slice(
                        &source_planes[plane_index].data[source_start..source_start + row_len],
                    );
            }
        }
        let pts = match timestamp {
            FrameTimestamp::Pts(value) => value,
            FrameTimestamp::Sequence(_) => {
                ((frame_index as f64 / self.frame_rate) * 1_000_000.0).round() as i64
            }
        };
        yuv.set_pts(Some(pts));
        self.encoder
            .send_frame(&yuv)
            .map_err(|error| media_error("sending a GPU YUV frame to the HEVC encoder", error))?;
        self.frames_submitted += 1;
        self.drain_packets()
    }

    fn drain_packets(&mut self) -> Result<()> {
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {}
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::ffi::EAGAIN => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(error) => return Err(media_error("receiving an HEVC packet", error)),
            }
            let output = self
                .output
                .as_mut()
                .expect("output remains open until finish");
            let output_time_base = output
                .stream(self.stream_index)
                .expect("created stream remains present")
                .time_base();
            packet.set_stream(self.stream_index);
            if self.is_x265 && self.flushing && self.frames_submitted < 3 {
                // x265 initializes m_bframeDelayTime on the third input with
                // its default B-pyramid. Flushing only one or two frames can
                // otherwise return uninitialized DTS (encoder.cpp, Encoder::encode).
                // These clips cannot contain reordered B-frames, so decode
                // and presentation order coincide. Keep normal encoder DTS
                // for every longer clip and every other encoder.
                packet.set_dts(packet.pts());
            }
            packet.rescale_ts(self.time_base, output_time_base);
            packet
                .write_interleaved(output)
                .map_err(|error| media_error("muxing an HEVC packet", error))?;
        }
        Ok(())
    }

    fn finish(mut self, context: &ExportContext) -> Result<()> {
        context.check_cancelled()?;
        self.encoder
            .send_eof()
            .map_err(|error| media_error("flushing the HEVC encoder", error))?;
        self.flushing = true;
        self.drain_packets()?;
        self.output
            .as_mut()
            .expect("output remains open until finish")
            .write_trailer()
            .map_err(|error| media_error("writing the MP4 trailer", error))?;
        // SAFETY: the muxer owns this writable AVIOContext. Destruction cannot
        // report a buffered write failure, so flush and check it before publish.
        unsafe {
            let io = (*self.output.as_mut().expect("open output").as_mut_ptr()).pb;
            if !io.is_null() {
                ffmpeg::ffi::avio_flush(io);
                if (*io).error < 0 {
                    return Err(media_error(
                        "flushing the MP4 output",
                        ffmpeg::Error::from((*io).error),
                    ));
                }
            }
        }
        drop(self.output.take());
        context.check_cancelled()?;
        publish_output(self.temporary, &self.final_path)?;
        Ok(())
    }
}

fn copy_rgb_into_frame(rgb: &[u8], frame: &mut ffmpeg::frame::Video) -> Result<()> {
    let row_len = usize::try_from(frame.width())
        .ok()
        .and_then(|width| width.checked_mul(RGB_CHANNELS))
        .ok_or_else(|| Error::InvalidMedia("RGB row size overflowed".into()))?;
    let height = frame.height() as usize;
    let expected = row_len
        .checked_mul(height)
        .ok_or_else(|| Error::InvalidMedia("RGB frame size overflowed".into()))?;
    if rgb.len() != expected {
        return Err(Error::InvalidMedia(format!(
            "stitched RGB frame has {} bytes, expected {expected}",
            rgb.len()
        )));
    }
    let stride = frame.stride(0);
    if stride < row_len {
        return Err(Error::InvalidMedia(
            "FFmpeg allocated an undersized RGB row".into(),
        ));
    }
    for row in 0..height {
        let source_start = row * row_len;
        let destination_start = row * stride;
        frame.data_mut(0)[destination_start..destination_start + row_len]
            .copy_from_slice(&rgb[source_start..source_start + row_len]);
    }
    Ok(())
}

fn write_image_atomically(
    output: &Path,
    panorama: &crate::PanoramaFrame,
    options: &ImageExportOptions,
    context: &ExportContext,
) -> Result<()> {
    context.check_cancelled()?;
    if output.exists() {
        return Err(Error::InvalidMedia(format!(
            "refusing to overwrite existing output {}",
            output.display()
        )));
    }
    let temporary = temporary_output_path(output);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| crate::error::io_error(&temporary, error))?;
    let guard = TempPath::try_from_path(temporary.clone())
        .map_err(|error| crate::error::io_error(&temporary, error))?;
    let mut writer = BufWriter::new(file);
    match options.format {
        ImageFormat::Png => image::codecs::png::PngEncoder::new(&mut writer)
            .write_image(
                panorama.as_rgb8(),
                panorama.width(),
                panorama.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|error| Error::Media(format!("encoding PNG output failed: {error}")))?,
        ImageFormat::Jpeg => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, options.quality)
                .write_image(
                    panorama.as_rgb8(),
                    panorama.width(),
                    panorama.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|error| Error::Media(format!("encoding JPEG output failed: {error}")))?
        }
    }
    writer
        .flush()
        .map_err(|error| crate::error::io_error(&temporary, error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| crate::error::io_error(&temporary, error))?;
    drop(writer);
    context.check_cancelled()?;
    publish_output(guard, output)
}

fn publish_output(temporary: TempPath, output: &Path) -> Result<()> {
    // A separate existence check cannot prevent an output created while encoding
    // from being replaced. The platform's no-replace operation claims the final
    // name atomically, and TempPath cleans the staged artifact on failure.
    temporary.persist_noclobber(output).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            Error::InvalidMedia(format!(
                "refusing to overwrite existing output {}",
                output.display()
            ))
        } else {
            crate::error::io_error(output, error.error)
        }
    })
}

#[derive(Default)]
struct CreatedOutputs {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl CreatedOutputs {
    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CreatedOutputs {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionKind {
    Indices,
    Timestamps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectionTarget {
    value: u64,
    output_key: u64,
}

struct SelectionPlan {
    kind: SelectionKind,
    targets: Vec<SelectionTarget>,
    next: usize,
}

impl SelectionPlan {
    fn new(selection: FrameSelection) -> Result<Self> {
        let (kind, values) = match selection {
            FrameSelection::Indices(values) => (SelectionKind::Indices, values),
            FrameSelection::Timestamps(values) => (
                SelectionKind::Timestamps,
                values
                    .into_iter()
                    .map(|duration| duration_to_i64_micros(duration).map(|value| value as u64))
                    .collect::<Result<Vec<_>>>()?,
            ),
            FrameSelection::SampledRange {
                start,
                end,
                frames_per_second_milli,
            } => {
                if start > end {
                    return Err(Error::InvalidMedia(
                        "sampled frame range starts after it ends".into(),
                    ));
                }
                if frames_per_second_milli == 0 {
                    return Err(Error::InvalidMedia(
                        "sampled frame rate must be greater than zero".into(),
                    ));
                }
                if frames_per_second_milli > 1_000_000_000 {
                    return Err(Error::InvalidMedia("sampled frame rate is too high".into()));
                }
                let start = duration_to_i64_micros(start)? as u64;
                let end = duration_to_i64_micros(end)? as u64;
                let rate = u128::from(frames_per_second_milli);
                let count = u128::from(end - start) * rate / 1_000_000_000 + 1;
                if count > MAX_SELECTIONS as u128 {
                    return Err(Error::InvalidMedia(format!(
                        "frame selection exceeds the {MAX_SELECTIONS}-frame safety limit"
                    )));
                }
                // Round each rational target independently, as FFmpeg rounds
                // presentation timestamps. Repeatedly adding a truncated period
                // accumulates a full frame of error over long recordings.
                let values = (0..count)
                    .map(|index| start + ((index * 1_000_000_000 + rate / 2) / rate) as u64)
                    .collect();
                (SelectionKind::Timestamps, values)
            }
        };

        if values.is_empty() {
            return Err(Error::InvalidMedia(
                "frame selection must contain at least one frame".into(),
            ));
        }
        if values.len() > MAX_SELECTIONS {
            return Err(Error::InvalidMedia(format!(
                "frame selection exceeds the {MAX_SELECTIONS}-frame safety limit"
            )));
        }
        let mut values = values;
        values.sort_unstable();
        values.dedup();
        let targets = values
            .into_iter()
            .map(|value| SelectionTarget {
                value,
                output_key: value,
            })
            .collect();
        Ok(Self {
            kind,
            targets,
            next: 0,
        })
    }

    fn len(&self) -> usize {
        self.targets.len()
    }

    fn remaining(&self) -> usize {
        self.targets.len() - self.next
    }

    fn is_complete(&self) -> bool {
        self.next == self.targets.len()
    }

    fn first_seek_timestamp(&self) -> Option<i64> {
        (self.kind == SelectionKind::Timestamps && self.targets[0].value > 0)
            .then(|| self.targets[0].value as i64)
    }

    fn matches(&mut self, frame_index: u64, timestamp: FrameTimestamp) -> Result<Vec<String>> {
        let current = match self.kind {
            SelectionKind::Indices => frame_index,
            SelectionKind::Timestamps => match timestamp {
                FrameTimestamp::Pts(value) if value >= 0 => value as u64,
                FrameTimestamp::Pts(_) => return Ok(Vec::new()),
                FrameTimestamp::Sequence(_) => {
                    return Err(Error::InvalidMedia(
                        "timestamp selection requires presentation timestamps on both tracks"
                            .into(),
                    ));
                }
            },
        };

        let mut matches = Vec::new();
        while let Some(target) = self.targets.get(self.next) {
            if current < target.value {
                break;
            }
            if self.kind == SelectionKind::Indices && current != target.value {
                return Err(Error::InvalidMedia(format!(
                    "decoded video skipped requested frame index {}",
                    target.value
                )));
            }
            matches.push(match self.kind {
                SelectionKind::Indices => format!("{:08}", target.output_key),
                SelectionKind::Timestamps => format!("{:016}", target.output_key),
            });
            self.next += 1;
        }
        Ok(matches)
    }
}

fn duration_to_i64_micros(duration: Duration) -> Result<i64> {
    i64::try_from(duration.as_micros())
        .map_err(|_| Error::InvalidMedia("video export timestamp is too large".into()))
}

fn duration_from_timestamp(timestamp: FrameTimestamp) -> Option<Duration> {
    match timestamp {
        FrameTimestamp::Pts(value) if value >= 0 => Some(Duration::from_micros(value as u64)),
        _ => None,
    }
}

fn emit_progress(
    context: &ExportContext,
    phase: ExportPhase,
    completed: u64,
    total: u64,
    media_time: Option<Duration>,
    started: Instant,
) {
    let elapsed = started.elapsed();
    let estimated_remaining = if completed == 0 || completed >= total {
        None
    } else {
        elapsed
            .checked_div(u32::try_from(completed).unwrap_or(u32::MAX))
            .and_then(|per_frame| per_frame.checked_mul((total - completed) as u32))
    };
    context.emit(ExportEvent::Progress(ExportProgress {
        phase,
        completed,
        total: Some(total),
        media_time,
        elapsed,
        estimated_remaining,
    }));
}

fn emit_video_progress(
    context: &ExportContext,
    phase: ExportPhase,
    completed: u64,
    total: Option<u64>,
    media_time: Option<Duration>,
    started: Instant,
) {
    let elapsed = started.elapsed();
    let estimated_remaining = total.and_then(|total| {
        if completed == 0 || completed >= total {
            None
        } else {
            elapsed
                .checked_div(u32::try_from(completed).unwrap_or(u32::MAX))
                .and_then(|per_frame| per_frame.checked_mul((total - completed) as u32))
        }
    });
    context.emit(ExportEvent::Progress(ExportProgress {
        phase,
        completed,
        total,
        media_time,
        elapsed,
        estimated_remaining,
    }));
}

fn media_error(action: &str, error: ffmpeg::Error) -> Error {
    Error::Media(format!("{action} failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stabilization;
    use crate::{Environment, Housing, PanoramaFrame};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn test_context() -> ExportContext {
        let (events, _) = crossbeam_channel::bounded(32);
        ExportContext {
            cancel: Arc::new(AtomicBool::new(false)),
            events,
        }
    }

    #[test]
    fn ilog_selection_requires_the_recorded_profile_and_matching_camera() {
        let mut metadata = InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            ..InsvMetadata::default()
        };
        assert!(resolve_color_lut(&metadata, ColorConversion::Auto)
            .unwrap()
            .is_none());
        metadata.gamma_mode = Some("log".into());
        assert!(resolve_color_lut(&metadata, ColorConversion::Auto)
            .unwrap()
            .is_none());
        assert!(resolve_color_lut(&metadata, ColorConversion::ILogToRec709)
            .unwrap()
            .is_some());
        metadata.gamma_mode = Some("I_Log".into());
        assert!(resolve_color_lut(&metadata, ColorConversion::Auto)
            .unwrap()
            .is_some());
        metadata.recorded_color_mode = Some(RecordedColorMode::Standard);
        assert!(matches!(
            resolve_color_lut(&metadata, ColorConversion::Auto),
            Err(Error::InvalidMedia(_))
        ));
        metadata.gamma_mode = None;
        for mode in [RecordedColorMode::Standard, RecordedColorMode::Dolby] {
            metadata.recorded_color_mode = Some(mode);
            assert!(resolve_color_lut(&metadata, ColorConversion::Auto)
                .unwrap()
                .is_none());
            assert!(matches!(
                resolve_color_lut(&metadata, ColorConversion::ILogToRec709),
                Err(Error::InvalidMedia(_))
            ));
        }
        metadata.recorded_color_mode = Some(RecordedColorMode::ILog);
        assert!(resolve_color_lut(&metadata, ColorConversion::Preserve)
            .unwrap()
            .is_none());
        let lut = resolve_color_lut(&metadata, ColorConversion::Auto)
            .unwrap()
            .unwrap();
        assert_eq!(lut.size(), 65);
        assert_ne!(lut.sample([0.5; 3]), [0.5; 3]);
        metadata.camera_name = Some("Insta360 X6".into());
        assert!(matches!(
            resolve_color_lut(&metadata, ColorConversion::Auto),
            Err(Error::MissingCapability(_))
        ));
    }

    #[test]
    fn invalid_capture_color_cannot_fall_back_to_the_gamma_marker() {
        let metadata = InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            gamma_mode: Some("I_Log".into()),
            recorded_color_mode_invalid: true,
            ..InsvMetadata::default()
        };
        for policy in [ColorConversion::Auto, ColorConversion::ILogToRec709] {
            assert!(matches!(
                resolve_color_lut(&metadata, policy),
                Err(Error::InvalidMedia(_))
            ));
        }
        assert!(resolve_color_lut(&metadata, ColorConversion::Preserve)
            .unwrap()
            .is_none());
    }

    #[test]
    fn converted_video_uses_bt709_matrix_and_limited_range() {
        ffmpeg::init().unwrap();
        let mut rgb = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGB24, 32, 16);
        let stride = rgb.stride(0);
        for row in rgb.data_mut(0).chunks_exact_mut(stride).take(16) {
            for pixel in row[..32 * 3].chunks_exact_mut(3) {
                pixel.copy_from_slice(&[255, 0, 0]);
            }
        }
        let mut yuv = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, 32, 16);
        rgb_to_yuv_scaler(32, 16, true)
            .unwrap()
            .run(&rgb, &mut yuv)
            .unwrap();
        // BT.709 limited-range red: Y=16+219*0.2126, Cb=128-224*0.2126/1.8556,
        // Cr=128+224*(1-0.2126)/1.5748. BT.601 would give Y approximately 81.
        for (plane, expected) in [63_u8, 102, 240].into_iter().enumerate() {
            assert!(yuv.data(plane)[0].abs_diff(expected) <= 1);
        }
        yuv.set_color_range(ffmpeg::util::color::Range::MPEG);
        yuv.set_color_space(ffmpeg::util::color::Space::BT709);
        let decoded = DecodedVideoFrame::new(yuv);
        let recovered = RgbFrameConverter::default().convert(&decoded).unwrap();
        for (actual, expected) in recovered.as_rgb8()[..3].iter().zip([255_u8, 0, 0]) {
            assert!(
                actual.abs_diff(expected) <= 2,
                "BT.709 conversion did not recover red"
            );
        }
    }

    fn color_test_pair() -> SynchronizedPair {
        SynchronizedPair {
            timestamp: FrameTimestamp::Pts(0),
            frames: std::array::from_fn(|_| {
                let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGB24, 32, 32);
                let stride = frame.stride(0);
                for row in frame.data_mut(0).chunks_exact_mut(stride).take(32) {
                    for pixel in row[..32 * 3].chunks_exact_mut(3) {
                        pixel.copy_from_slice(&[64, 96, 128]);
                    }
                }
                DecodedVideoFrame::new(frame)
            }),
        }
    }

    #[test]
    fn media_session_applies_the_selected_asset_to_images_and_video() {
        ffmpeg::init().unwrap();
        let calibration = crate::calibration::synthetic_dual_fisheye_calibration(32, 32).unwrap();
        let projection = EquirectangularProjection {
            width: 64,
            height: 32,
        };
        let mut session =
            StitchSession::try_open(BackendAttempt::Cpu, ProcessingBackend::Cpu, None).unwrap();
        let baseline = session
            .stitch(
                color_test_pair(),
                &calibration,
                projection,
                &FrameMotion::global(Orientation::IDENTITY).unwrap(),
            )
            .unwrap();
        let metadata = InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            recorded_color_mode: Some(RecordedColorMode::ILog),
            ..InsvMetadata::default()
        };
        let lut = resolve_color_lut(&metadata, ColorConversion::Auto)
            .unwrap()
            .unwrap();
        let mut expected = baseline.into_rgb8();
        lut.apply_rgb8(&mut expected).unwrap();
        assert!(expected
            .chunks_exact(3)
            .any(|pixel| pixel != [0, 0, 0] && pixel != [64, 96, 128]));
        session.set_color_lut(Some(lut));
        let image = session
            .stitch(
                color_test_pair(),
                &calibration,
                projection,
                &FrameMotion::global(Orientation::IDENTITY).unwrap(),
            )
            .unwrap();
        assert_eq!(image.as_rgb8(), expected);
        let video = session
            .stitch_video(
                color_test_pair(),
                &calibration,
                projection,
                &FrameMotion::global(Orientation::IDENTITY).unwrap(),
            )
            .unwrap();
        match video {
            StitchedVideoFrame::Rgb(frame) => assert_eq!(frame.as_rgb8(), expected),
            #[cfg(feature = "gpu")]
            StitchedVideoFrame::Yuv420(_) => panic!("CPU session returns RGB"),
        }
    }

    #[test]
    fn normalizes_and_deduplicates_index_selection() {
        let mut plan =
            SelectionPlan::new(FrameSelection::Indices(vec![7, 2, 7])).expect("valid selection");
        assert!(plan
            .matches(1, FrameTimestamp::Sequence(1))
            .unwrap()
            .is_empty());
        assert_eq!(
            plan.matches(2, FrameTimestamp::Sequence(2)).unwrap(),
            ["00000002"]
        );
        assert_eq!(
            plan.matches(7, FrameTimestamp::Sequence(7)).unwrap(),
            ["00000007"]
        );
        assert!(plan.is_complete());
    }

    #[test]
    fn timestamp_selection_uses_first_frame_at_or_after_each_target() {
        let mut plan = SelectionPlan::new(FrameSelection::Timestamps(vec![
            Duration::from_micros(1_000),
            Duration::from_micros(1_500),
        ]))
        .expect("valid selection");
        assert!(plan
            .matches(0, FrameTimestamp::Pts(999))
            .unwrap()
            .is_empty());
        assert_eq!(
            plan.matches(1, FrameTimestamp::Pts(2_000)).unwrap(),
            ["0000000000001000", "0000000000001500"]
        );
    }

    #[test]
    fn sampled_ranges_do_not_accumulate_rounding_error() {
        let plan = SelectionPlan::new(FrameSelection::SampledRange {
            start: Duration::from_secs(10),
            end: Duration::from_secs(3_610),
            frames_per_second_milli: 30_000,
        })
        .expect("one hour at 30 FPS");
        assert_eq!(plan.len(), 108_001);
        assert_eq!(plan.targets[1].value, 10_033_333);
        assert_eq!(plan.targets[2].value, 10_066_667);
        assert_eq!(plan.targets[108_000].value, 3_610_000_000);

        let fractional = SelectionPlan::new(FrameSelection::SampledRange {
            start: Duration::ZERO,
            end: Duration::from_secs(1_000),
            frames_per_second_milli: 29_970,
        })
        .expect("fractional frame rate");
        assert_eq!(fractional.len(), 29_971);
        assert_eq!(fractional.targets[29_970].value, 1_000_000_000);
    }

    #[test]
    fn selections_reject_out_of_range_timestamps_and_excessive_counts() {
        assert!(
            SelectionPlan::new(FrameSelection::Timestamps(vec![Duration::from_micros(
                i64::MAX as u64 + 1
            ),]))
            .is_err()
        );
        assert!(SelectionPlan::new(FrameSelection::SampledRange {
            start: Duration::ZERO,
            end: Duration::from_secs(1_000_000),
            frames_per_second_milli: 30_000,
        })
        .is_err());
    }

    #[test]
    fn video_range_discards_preroll_stops_exclusively_and_rebases_first_frame() {
        let mut range = VideoRange::new(
            Some(Duration::from_secs(10)),
            Some(Duration::from_secs(2)),
            Some(Duration::from_secs(20)),
        )
        .expect("valid range");

        assert_eq!(range.seek_timestamp(), Some(10_000_000));
        assert_eq!(
            range.effective_duration(Some(Duration::from_secs(20))),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            range.classify(FrameTimestamp::Pts(9_999_999)).unwrap(),
            VideoRangeDecision::Before
        );
        assert_eq!(
            range.classify(FrameTimestamp::Pts(10_010_000)).unwrap(),
            VideoRangeDecision::Include(FrameTimestamp::Pts(0))
        );
        assert_eq!(
            range.classify(FrameTimestamp::Pts(10_510_000)).unwrap(),
            VideoRangeDecision::Include(FrameTimestamp::Pts(500_000))
        );
        assert_eq!(
            range.classify(FrameTimestamp::Pts(12_000_000)).unwrap(),
            VideoRangeDecision::End
        );
    }

    #[test]
    fn video_range_validates_empty_outside_and_timestampless_intervals() {
        assert!(
            VideoRange::new(None, Some(Duration::ZERO), Some(Duration::from_secs(10))).is_err()
        );
        assert!(VideoRange::new(
            Some(Duration::from_secs(10)),
            None,
            Some(Duration::from_secs(10))
        )
        .is_err());

        let mut range = VideoRange::new(
            Some(Duration::from_secs(1)),
            Some(Duration::from_secs(1)),
            None,
        )
        .expect("range without known source duration");
        assert!(range.classify(FrameTimestamp::Sequence(0)).is_err());
    }

    #[test]
    fn backend_attempts_keep_explicit_requests_strict() {
        assert_eq!(
            backend_attempts(ProcessingBackend::Cpu),
            [BackendAttempt::Cpu]
        );
        assert_eq!(
            backend_attempts(ProcessingBackend::Gpu),
            [BackendAttempt::Gpu]
        );
        assert_eq!(
            backend_attempts(ProcessingBackend::Auto),
            [BackendAttempt::Gpu, BackendAttempt::Cpu]
        );
    }

    #[test]
    fn automatic_backend_restarts_the_whole_operation_after_gpu_failure() {
        let context = test_context();
        let mut attempts = Vec::new();
        let selected = run_with_backend_fallback(
            ProcessingBackend::Auto,
            &context,
            "test export",
            |backend, requested, fallback| {
                attempts.push(backend);
                assert_eq!(requested, ProcessingBackend::Auto);
                if backend == ProcessingBackend::Gpu {
                    assert!(fallback.is_none());
                    return Err(Error::GpuProcessing(Box::new(GpuFailure::new(
                        GpuFailureCode::Submission,
                        GpuFailureStage::Dispatch,
                        "injected failure",
                    ))));
                }
                assert_eq!(
                    fallback.as_ref().map(|failure| failure.code),
                    Some(GpuFailureCode::Submission)
                );
                Ok(backend)
            },
        )
        .expect("CPU retry succeeds");

        assert_eq!(selected, ProcessingBackend::Cpu);
        assert_eq!(attempts, [ProcessingBackend::Gpu, ProcessingBackend::Cpu]);
    }

    #[test]
    fn automatic_backend_does_not_retry_non_gpu_failures() {
        let context = test_context();
        let mut attempts = 0;
        let error = run_with_backend_fallback(
            ProcessingBackend::Auto,
            &context,
            "test export",
            |_backend, _requested, _fallback| -> Result<()> {
                attempts += 1;
                Err(Error::InvalidMedia("injected invalid input".into()))
            },
        )
        .expect_err("invalid input must not fall back");

        assert!(matches!(error, Error::InvalidMedia(_)));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn failed_frame_attempt_removes_only_outputs_it_created() {
        let directory = tempfile::tempdir().expect("temp directory");
        let tracked = directory.path().join("tracked.png");
        let unrelated = directory.path().join("unrelated.png");
        std::fs::write(&tracked, b"attempt").expect("tracked output");
        std::fs::write(&unrelated, b"user").expect("unrelated output");

        {
            let mut outputs = CreatedOutputs::default();
            outputs.track(tracked.clone());
        }

        assert!(!tracked.exists());
        assert_eq!(std::fs::read(unrelated).expect("unrelated file"), b"user");
    }

    #[test]
    fn successful_frame_attempt_commits_tracked_outputs() {
        let directory = tempfile::tempdir().expect("temp directory");
        let tracked = directory.path().join("tracked.png");
        std::fs::write(&tracked, b"complete").expect("tracked output");

        {
            let mut outputs = CreatedOutputs::default();
            outputs.track(tracked.clone());
            outputs.commit();
        }

        assert_eq!(
            std::fs::read(tracked).expect("committed output"),
            b"complete"
        );
    }

    #[test]
    fn decoded_frame_retains_plane_layout_and_color_metadata() {
        let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, 64, 32);
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);
        frame.set_color_space(ffmpeg::util::color::Space::BT709);
        frame.set_color_primaries(ffmpeg::util::color::Primaries::BT709);
        frame.set_color_transfer_characteristic(ffmpeg::util::color::TransferCharacteristic::BT709);

        let decoded = DecodedVideoFrame::new(frame);
        assert_eq!(decoded.format.pixel_format, ffmpeg::format::Pixel::YUV420P);
        assert_eq!((decoded.format.width, decoded.format.height), (64, 32));
        assert_eq!(decoded.format.planes.len(), 3);
        assert_eq!(decoded.format.planes[0].width, 64);
        assert_eq!(decoded.format.planes[0].height, 32);
        assert_eq!(decoded.format.planes[1].width, 32);
        assert_eq!(decoded.format.planes[1].height, 16);
        assert!(decoded.format.planes.iter().all(|plane| plane.stride > 0));
        assert_eq!(decoded.format.color.range, ffmpeg::util::color::Range::MPEG);
        assert_eq!(
            decoded.format.color.matrix,
            ffmpeg::util::color::Space::BT709
        );
        assert_eq!(
            decoded.format.color.primaries,
            ffmpeg::util::color::Primaries::BT709
        );
        assert_eq!(
            decoded.format.color.transfer,
            ffmpeg::util::color::TransferCharacteristic::BT709
        );
        decoded.validate_layout().expect("retained AVFrame layout");
    }

    #[test]
    fn atomic_png_write_leaves_only_the_committed_file() {
        let directory = tempfile::tempdir().expect("temp directory");
        let output = directory.path().join("frame.png");
        let panorama = PanoramaFrame::new(2, 1, vec![255, 0, 0, 0, 255, 0]).expect("panorama");

        write_image_atomically(
            &output,
            &panorama,
            &ImageExportOptions::default(),
            &test_context(),
        )
        .expect("image output");

        assert!(output.is_file());
        assert!(!temporary_output_path(&output).exists());
        let decoded = image::open(output).expect("decode output").to_rgb8();
        assert_eq!(decoded.as_raw(), panorama.as_rgb8());
    }

    #[test]
    fn atomic_publication_preserves_a_destination_created_while_encoding() {
        let directory = tempfile::tempdir().expect("temp directory");
        let output = directory.path().join("panorama.mp4");
        let staging = temporary_output_path(&output);
        std::fs::write(&staging, b"encoded video").expect("staged video");
        let guard = TempPath::try_from_path(staging.clone()).expect("temporary ownership");
        std::fs::write(&output, b"another export").expect("competing output");
        assert!(matches!(
            publish_output(guard, &output),
            Err(Error::InvalidMedia(_))
        ));
        assert_eq!(
            std::fs::read(&output).expect("existing output"),
            b"another export"
        );
        assert!(!staging.exists());
    }

    #[test]
    fn atomic_publication_has_exactly_one_winner_for_concurrent_exports() {
        let directory = tempfile::tempdir().expect("temp directory");
        let output = directory.path().join("frame.png");
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let jobs = [b"first".as_slice(), b"second".as_slice()].map(|bytes| {
                let output = &output;
                let barrier = &barrier;
                let directory = directory.path();
                scope.spawn(move || {
                    let staging = tempfile::NamedTempFile::new_in(directory).expect("staged file");
                    std::fs::write(staging.path(), bytes).expect("complete image");
                    barrier.wait();
                    publish_output(staging.into_temp_path(), output).map(|()| bytes)
                })
            });
            let mut winners = jobs
                .into_iter()
                .filter_map(|job| job.join().expect("export worker").ok());
            let winner = winners.next().expect("one publication succeeds");
            assert!(winners.next().is_none());
            assert_eq!(std::fs::read(&output).expect("published image"), winner);
        });
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("directory")
                .count(),
            1
        );
    }

    #[test]
    fn cpu_media_converter_refuses_unsupported_color_matrices() {
        let mut frame = allocate_video_frame(ffmpeg::format::Pixel::YUV420P, 2, 2).unwrap();
        frame.set_color_space(ffmpeg::util::color::Space::BT2020CL);
        let decoded = DecodedVideoFrame::new(frame);
        assert!(matches!(
            RgbFrameConverter::default().convert(&decoded),
            Err(Error::MissingCapability(_))
        ));
    }

    #[test]
    fn hevc_encoder_priorities_preserve_acceleration_policy() {
        assert_eq!(
            hevc_encoder_priority(MediaAcceleration::Hardware, false),
            HARDWARE_HEVC_ENCODERS
        );
        assert_eq!(
            hevc_encoder_priority(MediaAcceleration::Software, true),
            SOFTWARE_HEVC_ENCODERS
        );

        let gpu_auto = hevc_encoder_priority(MediaAcceleration::Auto, true);
        assert_eq!(
            &gpu_auto[..HARDWARE_HEVC_ENCODERS.len()],
            HARDWARE_HEVC_ENCODERS
        );
        assert_eq!(
            &gpu_auto[HARDWARE_HEVC_ENCODERS.len()..],
            SOFTWARE_HEVC_ENCODERS
        );

        let cpu_auto = hevc_encoder_priority(MediaAcceleration::Auto, false);
        assert_eq!(
            &cpu_auto[..SOFTWARE_HEVC_ENCODERS.len()],
            SOFTWARE_HEVC_ENCODERS
        );
        assert_eq!(
            &cpu_auto[SOFTWARE_HEVC_ENCODERS.len()..],
            HARDWARE_HEVC_ENCODERS
        );
    }

    #[test]
    fn bitrate_quality_preserves_photogrammetry_detail_at_high_settings() {
        let low = quality_target_bitrate(1920, 960, 30_000.0 / 1_001.0, 50);
        let photogrammetry = quality_target_bitrate(1920, 960, 30_000.0 / 1_001.0, 85);
        let maximum = quality_target_bitrate(1920, 960, 30_000.0 / 1_001.0, 100);

        assert!(low < photogrammetry);
        assert!(photogrammetry < maximum);
        assert!((35_000_000..=36_500_000).contains(&photogrammetry));

        let full_resolution = quality_target_bitrate(5760, 2880, 30_000.0 / 1_001.0, 85);
        assert!((138_000_000..=141_000_000).contains(&full_resolution));
    }

    #[test]
    fn encoder_attempts_continue_in_priority_order_until_one_opens() {
        let mut attempted = Vec::new();
        let selected = try_in_order(["first", "second", "third"], |candidate| {
            attempted.push(*candidate);
            if *candidate == "third" {
                Ok(*candidate)
            } else {
                Err(format!("{candidate} unavailable"))
            }
        })
        .expect("third candidate opens");

        assert_eq!(selected, "third");
        assert_eq!(attempted, ["first", "second", "third"]);
    }

    #[test]
    fn hevc_writer_creates_an_atomic_mp4_when_an_encoder_is_available() {
        ffmpeg::init().expect("FFmpeg initialization");
        if hevc_encoder_candidates(MediaAcceleration::Software, false).is_empty() {
            return;
        }
        let directory = tempfile::tempdir().expect("temp directory");
        let output = directory.path().join("stitched.mp4");
        let panorama =
            PanoramaFrame::new(64, 32, vec![64; 64 * 32 * RGB_CHANNELS]).expect("panorama");
        let context = test_context();
        let mut writer = HevcWriter::new(
            &output,
            64,
            32,
            30.0,
            80,
            HevcEncodingPolicy {
                acceleration: MediaAcceleration::Software,
                gpu_stitching: false,
                direct_bt709_yuv: false,
                converted_rec709: false,
            },
        )
        .expect("HEVC writer");
        for frame in 0..3 {
            writer
                .write(&panorama, FrameTimestamp::Pts(frame * 33_333), frame as u64)
                .expect("HEVC frame");
        }
        writer.finish(&context).expect("finish MP4");

        assert!(output.is_file());
        assert!(!temporary_output_path(&output).exists());
        let input = ffmpeg::format::input(&output).expect("open encoded MP4");
        let video = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .expect("video stream");
        assert_eq!(video.parameters().id(), ffmpeg::codec::Id::HEVC);
    }

    #[test]
    fn decodes_first_synchronized_pair_from_configured_sample() {
        let Ok(path) = std::env::var("INSTA360_RS_X5_SAMPLE") else {
            return;
        };
        let sequence = crate::RecordingSequence::single(InputSet::discover(path).unwrap()).unwrap();
        let mut reader = crate::PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let pair = native_pair(
            reader
                .next_pair(&std::sync::atomic::AtomicBool::new(false))
                .unwrap()
                .unwrap(),
        );
        for frame in &pair.frames {
            assert_eq!((frame.format.width, frame.format.height), (2_880, 2_880));
            assert!(!frame.format.planes.is_empty());
        }
    }

    #[test]
    fn exports_small_stitched_png_from_configured_sample() {
        let Ok(path) = std::env::var("INSTA360_RS_X5_SAMPLE") else {
            return;
        };
        let directory = tempfile::tempdir().expect("temp directory");
        let config = StitchConfig {
            housing: Housing::None,
            environment: Environment::Air,
            stabilization: Stabilization::DirectionLock,
            projection: Some(EquirectangularProjection {
                width: 640,
                height: 320,
            }),
            ..StitchConfig::default()
        };
        let result = crate::media::Exporter::new(
            InputSet::new(vec![PathBuf::from(path)]).expect("input set"),
            config,
        )
        .expect("exporter")
        .export_frames(
            directory.path(),
            FrameSelection::Indices(vec![0]),
            ImageExportOptions::default(),
        )
        .wait()
        .expect("stitched frame export");

        assert_eq!(result.frames_written, 1);
        assert_eq!(result.outputs.len(), 1);
        let decoded = image::open(&result.outputs[0])
            .expect("decode stitched output")
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (640, 320));
    }

    #[test]
    fn exports_short_stitched_hevc_from_configured_sample() {
        let (Ok(input_path), Ok(output_path)) = (
            std::env::var("INSTA360_RS_X5_SAMPLE"),
            std::env::var("INSTA360_RS_X5_VIDEO_SMOKE_OUTPUT"),
        ) else {
            return;
        };
        let input_path = PathBuf::from(input_path);
        let output_path = PathBuf::from(output_path);
        let width = std::env::var("INSTA360_RS_X5_VIDEO_SMOKE_WIDTH")
            .map(|value| value.parse::<u32>().expect("valid smoke-test width"))
            .unwrap_or(640);
        let start_seconds = std::env::var("INSTA360_RS_X5_VIDEO_SMOKE_START")
            .map(|value| value.parse::<f64>().expect("valid smoke-test start"))
            .unwrap_or(10.0);
        let duration_seconds = std::env::var("INSTA360_RS_X5_VIDEO_SMOKE_DURATION")
            .map(|value| value.parse::<f64>().expect("valid smoke-test duration"))
            .unwrap_or(1.0);
        let acceleration = match std::env::var("INSTA360_RS_X5_VIDEO_SMOKE_ACCELERATION").as_deref()
        {
            Ok("auto") => crate::MediaAcceleration::Auto,
            Ok("hardware") => crate::MediaAcceleration::Hardware,
            Ok("software") | Err(_) => crate::MediaAcceleration::Software,
            Ok(value) => panic!("unsupported smoke-test acceleration {value:?}"),
        };
        let backend = match std::env::var("INSTA360_RS_X5_VIDEO_SMOKE_BACKEND").as_deref() {
            Ok("auto") => ProcessingBackend::Auto,
            Ok("cpu") => ProcessingBackend::Cpu,
            Ok("gpu") => ProcessingBackend::Gpu,
            Ok(value) => panic!("unsupported smoke-test backend {value:?}"),
            Err(_) if std::env::var_os("INSTA360_RS_X5_VIDEO_SMOKE_GPU").is_some() => {
                ProcessingBackend::Gpu
            }
            Err(_) => ProcessingBackend::Auto,
        };
        let config = StitchConfig {
            housing: Housing::Auto,
            environment: if std::env::var_os("INSTA360_RS_X5_VIDEO_SMOKE_UNDERWATER").is_some() {
                Environment::Underwater
            } else {
                Environment::Auto
            },
            stabilization: Stabilization::DirectionLock,
            projection: Some(EquirectangularProjection {
                width,
                height: width / 2,
            }),
            backend,
            ..StitchConfig::default()
        };
        let result = crate::media::Exporter::new(
            InputSet::new(vec![input_path]).expect("input set"),
            config,
        )
        .expect("exporter")
        .export_video(
            output_path.clone(),
            VideoExportOptions {
                quality: 85,
                audio: AudioPolicy::Drop,
                acceleration,
                projection: None,
                start: Some(Duration::from_secs_f64(start_seconds)),
                duration: Some(Duration::from_secs_f64(duration_seconds)),
            },
        )
        .wait()
        .expect("bounded stitched video export");

        let expected_frames = duration_seconds * 30_000.0 / 1_001.0;
        assert!((result.frames_written as f64 - expected_frames).abs() <= 2.0);
        if let ProcessingBackend::Cpu | ProcessingBackend::Gpu = backend {
            assert_eq!(
                result.backend.selected,
                match backend {
                    ProcessingBackend::Cpu => EffectiveBackend::Cpu,
                    ProcessingBackend::Gpu => EffectiveBackend::Gpu,
                    ProcessingBackend::Auto => unreachable!(),
                }
            );
        }
        eprintln!(
            "backend={:?} frames={} elapsed={:.3}s throughput={:.2}fps",
            result.backend.selected,
            result.frames_written,
            result.elapsed.as_secs_f64(),
            result.frames_written as f64 / result.elapsed.as_secs_f64()
        );
        assert!(!temporary_output_path(&output_path).exists());
        let output = ffmpeg::format::input(&output_path).expect("open stitched output");
        let video = output
            .streams()
            .best(ffmpeg::media::Type::Video)
            .expect("HEVC video stream");
        assert_eq!(video.parameters().id(), ffmpeg::codec::Id::HEVC);
    }
}
