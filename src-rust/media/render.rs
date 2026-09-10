//! Reusable, file-aware rendering without decoding, encoding or output ownership.

use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg::Rescale;
use serde::{Deserialize, Serialize};

use super::*;
use crate::optics::OpticalResolution;
use crate::paired::{DecodedLayout, FramePair};
use crate::{RecordingSequence, UnderwaterColorMode, UnderwaterColorOptions};

/// Native dimensions of one decoded lens, including packed-file separation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameDimensions {
    pub width: u32,
    pub height: u32,
}

impl FrameDimensions {
    fn validate(self) -> Result<Self> {
        let area = u64::from(self.width) * u64::from(self.height);
        if self.width == 0 || self.height == 0 || area > crate::stream::MAX_FRAME_PIXELS {
            return Err(Error::InvalidMedia(
                "frame dimensions exceed the bounded media limit".into(),
            ));
        }
        Ok(self)
    }

    fn scaled(self, width: Option<u32>) -> Result<Self> {
        self.validate()?;
        if width == Some(0) {
            return Err(Error::InvalidMedia("image width must be positive".into()));
        }
        let width = width.unwrap_or(self.width).min(self.width) & !1;
        let height =
            (u64::from(self.height) * u64::from(width) / u64::from(self.width)) as u32 & !1;
        Self { width, height }.validate()
    }

    fn panorama(self) -> Result<EquirectangularProjection> {
        EquirectangularProjection {
            width: self
                .width
                .checked_mul(2)
                .ok_or_else(|| Error::InvalidMedia("panorama width overflow".into()))?,
            height: self.width,
        }
        .validate()
    }
}

/// Inspects every chapter's declared decoded layout without loading models or motion.
/// Decoder viability and processing capabilities are checked separately by preflight.
pub fn inspect_frame_dimensions(sequence: &RecordingSequence) -> Result<Vec<FrameDimensions>> {
    if sequence.chapters.is_empty() {
        return Err(Error::InvalidMedia("recording sequence is empty".into()));
    }
    sequence
        .chapters
        .iter()
        .map(|chapter| {
            let layout = DecodedLayout::inspect(chapter)?;
            FrameDimensions {
                width: layout.lens_width(chapter),
                height: chapter.inspection.video_tracks[0].height,
            }
            .validate()
        })
        .collect()
}

/// Effective preparation for one chapter and output projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRenderInfo {
    pub projection: EquirectangularProjection,
    pub optics: Option<OpticalResolution>,
    pub calibration: FrameCalibrationInfo,
    pub warnings: Vec<String>,
}

/// Provenance copied from the actual resolved geometry, without exporting raw offsets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameCalibrationInfo {
    pub camera_model: Option<crate::CameraModel>,
    pub offset_version: u8,
    pub offset_source: OffsetSource,
    pub profile_name: Option<String>,
    pub polynomial_projection: [Option<crate::NormalizedPolynomialProjection>; 2],
}

impl From<&ResolvedCalibration> for FrameCalibrationInfo {
    fn from(calibration: &ResolvedCalibration) -> Self {
        Self {
            camera_model: calibration.camera_model.clone(),
            offset_version: calibration.offset_version,
            offset_source: calibration.offset_source,
            profile_name: calibration.profile_name.clone(),
            polynomial_projection: calibration
                .lenses
                .each_ref()
                .map(|lens| lens.polynomial_projection),
        }
    }
}

/// An owned RGB panorama from the exact borrowed input pair.
pub struct RenderedFrame {
    pub frame: PanoramaFrame,
    pub info: FrameRenderInfo,
    pub backend: BackendReport,
}

pub(super) struct PreparedChapter {
    pub(super) index: usize,
    pub(super) calibration: ResolvedCalibration,
    pub(super) stabilizer: Option<FileStabilizer>,
    pub(super) color_lut: Option<Arc<CubeLut>>,
}

/// Retains only the active chapter. Backward seeks replay metadata preparation,
/// never video decoding, so direction-lock continuity matches sequential export.
pub(super) struct PreparedRecording {
    sequence: RecordingSequence,
    config: StitchConfig,
    current: Option<PreparedChapter>,
}

impl PreparedRecording {
    pub(super) fn new(sequence: RecordingSequence, config: StitchConfig) -> Self {
        Self {
            sequence,
            config,
            current: None,
        }
    }

    pub(super) fn prepare(
        &mut self,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<&PreparedChapter> {
        self.prepare_with(index, cancel, |_| {})
    }

    pub(super) fn prepare_with(
        &mut self,
        index: usize,
        cancel: &AtomicBool,
        mut prepared: impl FnMut(&PreparedChapter),
    ) -> Result<&PreparedChapter> {
        check_cancel(cancel)?;
        if index >= self.sequence.chapters.len() {
            return Err(Error::InvalidMedia(
                "pair chapter is outside the recording".into(),
            ));
        }
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.index > index)
        {
            self.current = None;
        }
        let start = self.current.as_ref().map_or(0, |current| current.index + 1);
        for current_index in start..=index {
            check_cancel(cancel)?;
            let chapter = &self.sequence.chapters[current_index];
            validate_source_color(chapter)?;
            let calibration = resolve_calibration(&chapter.inspection.metadata, &self.config)?;
            calibration.validate_for_stitching()?;
            let color_lut =
                resolve_color_lut(&chapter.inspection.metadata, self.config.color_conversion)?;
            let file = File::open(&chapter.inputs.paths()[0])
                .map_err(|error| crate::error::io_error(&chapter.inputs.paths()[0], error))?;
            let mut reader = crate::InsvReader::new(file)?;
            let stabilizer = FileStabilizer::from_reader_continuing(
                &mut reader,
                &chapter.inspection,
                &self.config,
                self.current
                    .as_ref()
                    .and_then(|previous| previous.stabilizer.as_ref()),
            )?;
            check_cancel(cancel)?;
            let current = PreparedChapter {
                index: current_index,
                calibration,
                stabilizer,
                color_lut,
            };
            prepared(&current);
            self.current = Some(current);
        }
        Ok(self.current.as_ref().expect("requested chapter prepared"))
    }
}

/// Prepared recording renderer for exact frames supplied by a paired reader.
///
/// No files are written and no frames are decoded or re-seeked here. Keep the
/// session on its owning processing thread; scalers and GPU resources are reused.
/// Every render resets independent-still color history, while file motion always
/// preserves preceding chapter continuity. Resolution-reduced previews may differ
/// photometrically from full-size stills and continuous video.
pub struct RecordingFrameRenderer {
    prepared: PreparedRecording,
    dimensions: Vec<FrameDimensions>,
    stitcher: StitchSession,
    underwater: underwater_color::UnderwaterProcessor,
    frame_rate: Option<f64>,
}

impl RecordingFrameRenderer {
    pub fn new(sequence: RecordingSequence, config: StitchConfig) -> Result<Self> {
        let (events, _) = crossbeam_channel::bounded(1);
        let context = ExportContext {
            cancel: Arc::new(AtomicBool::new(false)),
            events,
        };
        let requested = config.backend;
        Self::for_attempt(sequence, config, requested, None, &context)
    }

    pub(super) fn for_attempt(
        sequence: RecordingSequence,
        config: StitchConfig,
        requested: ProcessingBackend,
        fallback: Option<GpuFailure>,
        context: &ExportContext,
    ) -> Result<Self> {
        let dimensions = inspect_frame_dimensions(&sequence)?;
        if let Some(projection) = config.projection {
            validate_projection(projection)?;
        }
        let frame_rate = sequence.chapters[0].inspection.fps;
        let underwater =
            underwater_color::UnderwaterProcessor::new(config.underwater_color, frame_rate)?;
        let stitcher = StitchSession::select(config.backend, requested, fallback, context)?;
        Ok(Self {
            prepared: PreparedRecording::new(sequence, config),
            dimensions,
            stitcher,
            underwater,
            frame_rate,
        })
    }

    /// Validates all chapters, motion, requested backend, dimensions and required
    /// color resources without opening an encoder or creating an output file.
    pub fn preflight(
        sequence: &RecordingSequence,
        config: &StitchConfig,
        projection: Option<EquirectangularProjection>,
        cancel: &AtomicBool,
    ) -> Result<Vec<FrameRenderInfo>> {
        check_cancel(cancel)?;
        let mut session = Self::new(sequence.clone(), config.clone())?;
        let mut reports = Vec::with_capacity(sequence.chapters.len());
        for (index, chapter) in sequence.chapters.iter().enumerate() {
            check_cancel(cancel)?;
            crate::PairedReader::validate_chapter(chapter)?;
            let projection = projection
                .or(config.projection)
                .map(Ok)
                .unwrap_or_else(|| session.dimensions[index].panorama())?;
            reports.push(session.prepare(index, projection, cancel)?);
        }
        Ok(reports)
    }

    pub(super) fn prepare(
        &mut self,
        index: usize,
        projection: EquirectangularProjection,
        cancel: &AtomicBool,
    ) -> Result<FrameRenderInfo> {
        validate_projection(projection)?;
        let frame_rate = self
            .prepared
            .sequence
            .chapters
            .get(index)
            .ok_or_else(|| Error::InvalidMedia("pair chapter is outside the recording".into()))?
            .inspection
            .fps;
        if frame_rate != self.frame_rate {
            self.underwater = underwater_color::UnderwaterProcessor::new(
                self.prepared.config.underwater_color,
                frame_rate,
            )?;
            self.frame_rate = frame_rate;
        }
        // Verify resources before allocating panorama-sized rendering buffers.
        self.underwater
            .prepare(projection.width, projection.height)?;
        let mut warnings = self.prepared.sequence.warnings.clone();
        let current = self.prepared.prepare(index, cancel)?;
        self.stitcher.set_color_lut(current.color_lut.clone());
        if let Some(stabilizer) = &current.stabilizer {
            warnings.extend(stabilizer.warnings().iter().cloned());
        }
        warnings.sort();
        warnings.dedup();
        Ok(FrameRenderInfo {
            projection,
            optics: current.calibration.optical_resolution.clone(),
            calibration: (&current.calibration).into(),
            warnings,
        })
    }

    pub(super) fn report(&self) -> &BackendReport {
        &self.stitcher.report
    }

    pub(super) fn emit_preparation(&self, context: &ExportContext) {
        if let Some(stabilizer) = self
            .prepared
            .current
            .as_ref()
            .and_then(|chapter| chapter.stabilizer.as_ref())
        {
            context.emit(ExportEvent::StabilizationPrepared(stabilizer.diagnostics()));
            for warning in stabilizer.warnings() {
                context.emit(ExportEvent::Warning(warning.clone()));
            }
        }
    }

    /// Renders one unpublished still. Auto retries only typed GPU failures on
    /// CPU; explicit GPU remains strict. The borrowed source is never modified.
    pub fn render(
        &mut self,
        pair: &FramePair,
        projection: EquirectangularProjection,
        cancel: &AtomicBool,
    ) -> Result<RenderedFrame> {
        render_unpublished(self.prepared.config.backend, cancel, |fallback| {
            if let Some(failure) = fallback {
                self.stitcher = StitchSession::try_open(
                    BackendAttempt::Cpu,
                    ProcessingBackend::Auto,
                    Some(failure),
                )
                .map_err(Error::GpuUnavailable)?;
            }
            self.render_strict(pair, projection, cancel)
        })
    }

    /// Does not retry a failed frame. Batch callers can roll back owned outputs
    /// and restart the complete attempt on CPU after a typed GPU failure.
    pub fn render_strict(
        &mut self,
        pair: &FramePair,
        projection: EquirectangularProjection,
        cancel: &AtomicBool,
    ) -> Result<RenderedFrame> {
        check_cancel(cancel)?;
        validate_pair(pair, &self.dimensions, &self.prepared.sequence)?;
        let info = self.prepare(pair.chapter_index, projection, cancel)?;
        let current = self.prepared.current.as_ref().expect("prepared above");
        let motion = stitch_motion(
            current.stabilizer.as_ref(),
            FrameTimestamp::Pts(pair.source_timestamp_micros),
        )?;
        let panorama =
            self.stitcher
                .stitch(native_pair(pair), &current.calibration, projection, &motion)?;
        check_cancel(cancel)?;
        self.underwater.reset();
        let frame = self.underwater.process(panorama, pair.timestamp_micros)?;
        check_cancel(cancel)?;
        Ok(RenderedFrame {
            frame,
            info,
            backend: self.stitcher.report.clone(),
        })
    }
}

/// Color-only native lens processing, independent of optical calibration/motion.
/// Resizes without upscaling, converts declared source matrix/range to packed
/// RGB8, applies I-Log then underwater restoration, and resets each lens image.
pub struct NativeColorProcessor {
    sequence: RecordingSequence,
    dimensions: Vec<FrameDimensions>,
    conversion: ColorConversion,
    options: UnderwaterColorOptions,
    current: Option<usize>,
    lut: Option<Arc<CubeLut>>,
    converters: [RgbFrameConverter; 2],
    underwater: underwater_color::UnderwaterProcessor,
    frame_rate: Option<f64>,
}

impl NativeColorProcessor {
    pub fn new(
        sequence: RecordingSequence,
        conversion: ColorConversion,
        options: UnderwaterColorOptions,
    ) -> Result<Self> {
        let dimensions = inspect_frame_dimensions(&sequence)?;
        let frame_rate = sequence.chapters[0].inspection.fps;
        let underwater = underwater_color::UnderwaterProcessor::new(options, frame_rate)?;
        Ok(Self {
            sequence,
            dimensions,
            conversion,
            options,
            current: None,
            lut: None,
            converters: [RgbFrameConverter::default(), RgbFrameConverter::default()],
            underwater,
            frame_rate,
        })
    }

    /// True only when a LUT actually resolves or restoration is enabled. An
    /// application may retain its direct native encoder path when this is false.
    pub fn requires_processing(&mut self, chapter_index: usize) -> Result<bool> {
        if self.current != Some(chapter_index) {
            let chapter = self.sequence.chapters.get(chapter_index).ok_or_else(|| {
                Error::InvalidMedia("pair chapter is outside the recording".into())
            })?;
            let lut = resolve_color_lut(&chapter.inspection.metadata, self.conversion)?;
            if lut.is_some() || self.options.mode != UnderwaterColorMode::Off {
                validate_source_color(chapter)?;
            }
            if chapter.inspection.fps != self.frame_rate {
                self.underwater = underwater_color::UnderwaterProcessor::new(
                    self.options,
                    chapter.inspection.fps,
                )?;
                self.frame_rate = chapter.inspection.fps;
            }
            self.lut = lut;
            self.current = Some(chapter_index);
        }
        Ok(self.lut.is_some() || self.options.mode != UnderwaterColorMode::Off)
    }

    /// Validates every chapter and selected output size, including complete color
    /// resources. No camera calibration, motion profile or video encoder is needed.
    pub fn preflight(
        &mut self,
        width: Option<u32>,
        cancel: &AtomicBool,
    ) -> Result<Vec<FrameDimensions>> {
        let mut dimensions = Vec::with_capacity(self.dimensions.len());
        for index in 0..self.dimensions.len() {
            check_cancel(cancel)?;
            crate::PairedReader::validate_chapter(&self.sequence.chapters[index])?;
            self.requires_processing(index)?;
            let size = self.dimensions[index].scaled(width)?;
            self.underwater.prepare(size.width, size.height)?;
            dimensions.push(size);
        }
        check_cancel(cancel)?;
        Ok(dimensions)
    }

    pub fn process(
        &mut self,
        pair: &FramePair,
        width: Option<u32>,
        cancel: &AtomicBool,
    ) -> Result<[LensFrame; 2]> {
        check_cancel(cancel)?;
        validate_pair(pair, &self.dimensions, &self.sequence)?;
        let processing = self.requires_processing(pair.chapter_index)?;
        let size = self.dimensions[pair.chapter_index].scaled(width)?;
        self.underwater.prepare(size.width, size.height)?;
        let mut lenses = Vec::with_capacity(2);
        for (index, source) in [&pair.a, &pair.b].into_iter().enumerate() {
            check_cancel(cancel)?;
            if processing {
                validate_transfer(source.color_transfer_characteristic())?;
            }
            let frame = self.converters[index].convert_scaled(
                &DecodedVideoFrame::borrow(source),
                size.width,
                size.height,
            )?;
            let mut rgb = frame.into_rgb8();
            if let Some(lut) = &self.lut {
                lut.apply_rgb8(&mut rgb)?;
            }
            self.underwater.reset();
            self.underwater.process_rgb8(
                &mut rgb,
                size.width,
                size.height,
                pair.timestamp_micros,
            )?;
            lenses.push(LensFrame::new(size.width, size.height, rgb)?);
        }
        check_cancel(cancel)?;
        Ok(lenses
            .try_into()
            .unwrap_or_else(|_| unreachable!("two lenses processed")))
    }
}

fn validate_projection(projection: EquirectangularProjection) -> Result<()> {
    projection.validate()?;
    FrameDimensions {
        width: projection.width,
        height: projection.height,
    }
    .validate()?;
    Ok(())
}

fn validate_pair(
    pair: &FramePair,
    dimensions: &[FrameDimensions],
    sequence: &RecordingSequence,
) -> Result<()> {
    let dimensions = dimensions
        .get(pair.chapter_index)
        .ok_or_else(|| Error::InvalidMedia("pair chapter is outside the recording".into()))?;
    let chapter = sequence
        .chapters
        .get(pair.chapter_index)
        .ok_or_else(|| Error::InvalidMedia("pair chapter is outside the recording".into()))?;
    let start = chapter.timeline_start.as_micros();
    let end = start + chapter.duration.as_micros();
    if pair.timestamp_micros < 0
        || (pair.timestamp_micros as u128) < start
        || (pair.timestamp_micros as u128) >= end
    {
        return Err(Error::InvalidMedia(
            "pair recording timestamp is outside its chapter".into(),
        ));
    }
    for (frame, pts) in [(&pair.a, pair.a_pts), (&pair.b, pair.b_pts)] {
        if frame.pts() != Some(pts) {
            return Err(Error::InvalidMedia(
                "native frame PTS disagrees with the pair identity".into(),
            ));
        }
        if (frame.width(), frame.height()) != (dimensions.width, dimensions.height) {
            return Err(Error::InvalidMedia(
                "pair dimensions differ from the recording chapter".into(),
            ));
        }
    }
    for time_base in [pair.a_time_base, pair.b_time_base] {
        if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
            return Err(Error::InvalidMedia(
                "pair has an invalid native time base".into(),
            ));
        }
    }
    let a = i128::from(pair.a_pts)
        * i128::from(pair.a_time_base.numerator())
        * i128::from(pair.b_time_base.denominator());
    let b = i128::from(pair.b_pts)
        * i128::from(pair.b_time_base.numerator())
        * i128::from(pair.a_time_base.denominator());
    if a != b
        || pair.timestamp_micros < 0
        || pair.a_pts.rescale(pair.a_time_base, (1, 1_000_000)) != pair.source_timestamp_micros
    {
        return Err(Error::InvalidMedia(
            "pair timestamps do not describe one simultaneous source frame".into(),
        ));
    }
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn render_unpublished<T>(
    requested: ProcessingBackend,
    cancel: &AtomicBool,
    mut render: impl FnMut(Option<GpuFailure>) -> Result<T>,
) -> Result<T> {
    check_cancel(cancel)?;
    match render(None) {
        Err(error) if requested == ProcessingBackend::Auto => {
            let failure = take_gpu_failure(error)?;
            check_cancel(cancel)?;
            render(Some(failure))
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_error() -> Error {
        Error::GpuProcessing(Box::new(GpuFailure::new(
            crate::GpuFailureCode::DeviceRequest,
            crate::GpuFailureStage::Preparation,
            "injected device failure",
        )))
    }

    #[test]
    fn unpublished_retry_is_auto_only_gpu_only_and_cancellable() {
        let cancel = AtomicBool::new(false);
        let mut attempts = 0;
        let output = render_unpublished(ProcessingBackend::Auto, &cancel, |fallback| {
            attempts += 1;
            if attempts == 1 {
                assert!(fallback.is_none());
                Err(gpu_error())
            } else {
                assert!(fallback.is_some());
                Ok(vec![12, 34, 56])
            }
        })
        .unwrap();
        assert_eq!(output, [12, 34, 56]);
        assert_eq!(attempts, 2);
        for requested in [ProcessingBackend::Cpu, ProcessingBackend::Gpu] {
            let mut attempts = 0;
            let result: Result<()> = render_unpublished(requested, &cancel, |_| {
                attempts += 1;
                Err(gpu_error())
            });
            assert!(matches!(result, Err(Error::GpuProcessing(_))));
            assert_eq!(attempts, 1);
        }
        let mut attempts = 0;
        let result: Result<()> = render_unpublished(ProcessingBackend::Auto, &cancel, |_| {
            attempts += 1;
            Err(Error::InvalidMedia("invalid input".into()))
        });
        assert!(matches!(result, Err(Error::InvalidMedia(_))));
        assert_eq!(attempts, 1);
        let mut attempts = 0;
        let result: Result<()> = render_unpublished(ProcessingBackend::Auto, &cancel, |_| {
            attempts += 1;
            cancel.store(true, Ordering::Release);
            Err(gpu_error())
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(attempts, 1);
    }
}
