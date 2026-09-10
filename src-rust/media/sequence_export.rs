//! One renderer and muxer for available, validated camera recording chapters.

use super::*;
use crate::media::VideoPreflight;
use crate::paired::{FramePair, PairedReader};
use crate::{RecordingSequence, Stabilization};

pub(in crate::media) fn preflight_video(
    sequence: &RecordingSequence,
    config: &StitchConfig,
    options: &VideoExportOptions,
) -> Result<VideoPreflight> {
    let mut report = inspect_video(sequence, config, options)?;
    let mut previous = None;
    for chapter in &sequence.chapters {
        let file = File::open(&chapter.inputs.paths()[0])
            .map_err(|error| crate::error::io_error(&chapter.inputs.paths()[0], error))?;
        let mut reader = crate::InsvReader::new(file)?;
        let current = FileStabilizer::from_reader_continuing(
            &mut reader,
            &chapter.inspection,
            config,
            previous.as_ref(),
        )?;
        if let Some(stabilizer) = &current {
            report
                .warnings
                .extend(stabilizer.warnings().iter().cloned());
        }
        previous = current;
    }
    report.warnings.sort();
    report.warnings.dedup();
    Ok(report)
}

fn inspect_video(
    sequence: &RecordingSequence,
    config: &StitchConfig,
    options: &VideoExportOptions,
) -> Result<VideoPreflight> {
    let first = sequence
        .chapters
        .first()
        .ok_or_else(|| Error::InvalidMedia("recording sequence is empty".into()))?;
    if !(1..=100).contains(&options.quality) {
        return Err(Error::InvalidMedia(
            "HEVC quality must be between 1 and 100".into(),
        ));
    }
    if config.stabilization == Stabilization::Off
        && config.rolling_shutter == crate::RollingShutterCorrection::Required
    {
        return Err(Error::InvalidMedia(
            "required rolling-shutter correction conflicts with disabled stabilization".into(),
        ));
    }
    let first_width = first
        .inspection
        .video_tracks
        .first()
        .ok_or_else(|| Error::InvalidMedia("recording has no video tracks".into()))?
        .width;
    let projection = options
        .projection
        .or(config.projection)
        .unwrap_or(crate::EquirectangularProjection {
            width: first_width
                .checked_mul(2)
                .ok_or_else(|| Error::InvalidMedia("projection width overflow".into()))?,
            height: first_width,
        })
        .validate()?;
    if projection.width % 2 != 0 || projection.height % 2 != 0 {
        return Err(Error::InvalidMedia(
            "HEVC YUV420 output dimensions must be even".into(),
        ));
    }
    let frame_rate = first
        .inspection
        .fps
        .filter(|fps| fps.is_finite() && *fps > 0.0)
        .ok_or_else(|| {
            Error::InvalidMedia("HEVC export requires a known positive source frame rate".into())
        })?;
    for chapter in &sequence.chapters {
        if chapter.inputs.paths().len() != 1 {
            return Err(Error::MissingCapability(
                "stitched export requires single-file dual-track chapters".into(),
            ));
        }
        validate_x5_source(&SourceData {
            inspection: chapter.inspection.clone(),
            stabilizer: None,
        })?;
        resolve_calibration(&chapter.inspection.metadata, config)?.validate_for_stitching()?;
        resolve_color_lut(&chapter.inspection.metadata, config.color_conversion)?;
        if chapter
            .inspection
            .fps
            .is_none_or(|fps| (fps - frame_rate).abs() > 1e-9)
        {
            return Err(Error::MissingCapability(
                "source frame rate changes between recording chapters".into(),
            ));
        }
    }
    let range = VideoRange::new(options.start, options.duration, Some(sequence.duration))?;
    let duration = range
        .effective_duration(Some(sequence.duration))
        .expect("known source duration");
    let encoder_candidates = hevc_encoder_candidates(
        options.acceleration,
        config.backend != ProcessingBackend::Cpu,
    )
    .into_iter()
    .map(|codec| codec.name().to_owned())
    .collect::<Vec<_>>();
    if encoder_candidates.is_empty() {
        return Err(Error::MissingCapability(
            "no eligible HEVC encoder is available for the selected acceleration policy".into(),
        ));
    }
    if config.backend == ProcessingBackend::Gpu
        && !crate::media::MediaCapabilities::detect().gpu_available
    {
        return Err(Error::MissingCapability(
            "the requested GPU stitch backend is unavailable".into(),
        ));
    }
    let audio_tracks = if options.audio == AudioPolicy::Copy {
        audio::AudioLayout::inspect(sequence)?.track_count()
    } else {
        0
    };
    let mut warnings = sequence.warnings.clone();
    if options.audio == AudioPolicy::Copy {
        warnings.push(if audio_tracks == 0 { "The recording contains no audio; the exported video will be silent.".into() }
            else { "Original audio is copied without re-encoding; cuts use complete compressed packets at recording and chapter boundaries.".into() });
    }
    Ok(VideoPreflight {
        projection,
        duration,
        frame_rate,
        chapter_count: sequence.chapters.len(),
        audio_tracks,
        encoder_candidates,
        warnings,
    })
}

pub(super) fn export_video(
    sequence: RecordingSequence,
    config: StitchConfig,
    output: PathBuf,
    options: VideoExportOptions,
    context: ExportContext,
    started: Instant,
) -> Result<ExportResult> {
    let requested = config.backend;
    run_with_backend_fallback(
        requested,
        &context,
        "video export",
        |selected, reported, fallback| {
            let mut config = config.clone();
            config.backend = selected;
            export_attempt(
                &sequence, &config, &output, &options, &context, started, reported, fallback,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn export_attempt(
    sequence: &RecordingSequence,
    config: &StitchConfig,
    output: &Path,
    options: &VideoExportOptions,
    context: &ExportContext,
    started: Instant,
    requested: ProcessingBackend,
    fallback: Option<GpuFailure>,
) -> Result<ExportResult> {
    context.check_cancelled()?;
    let first = sequence
        .chapters
        .first()
        .ok_or_else(|| Error::InvalidMedia("recording sequence is empty".into()))?;
    validate_video_export_config(&first.inputs, output, options)?;
    ffmpeg::init().map_err(|error| media_error("initializing FFmpeg", error))?;
    // Runtime GPU opening uses the typed failure path for whole-job Auto retry.
    let mut stitcher = StitchSession::select(config.backend, requested, fallback, context)?;
    context.emit(ExportEvent::BackendSelected(Box::new(
        stitcher.report.clone(),
    )));
    let report = inspect_video(sequence, config, options)?;
    for warning in &report.warnings {
        context.emit(ExportEvent::Warning(warning.clone()));
    }
    let mut range = VideoRange::new(options.start, options.duration, Some(sequence.duration))?;
    let end = options
        .start
        .unwrap_or_default()
        .checked_add(report.duration)
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
        .ok_or_else(|| Error::InvalidMedia("video interval overflow".into()))?;
    let total = Some((report.duration.as_secs_f64() * report.frame_rate).ceil() as u64);
    let mut reader = PairedReader::open(sequence, options.start.unwrap_or_default())?;
    let mut audio_layout = if options.audio == AudioPolicy::Copy {
        Some(audio::AudioLayout::inspect(sequence)?)
    } else {
        None
    };
    if audio_layout
        .as_ref()
        .is_some_and(|layout| layout.track_count() > 0)
    {
        reader.enable_audio();
    }
    let mut writer: Option<HevcWriter> = None;
    let mut prepared_count = 0;
    let mut stabilizer = None;
    let mut calibration = None;
    let mut frames_written = 0_u64;
    let mut progress = ProgressGate::new();
    while let Some(pair) = reader.next_pair(&context.cancel)? {
        context.check_cancelled()?;
        let queued_audio = reader.take_audio_packets();
        let output_timestamp = match range.classify(FrameTimestamp::Pts(pair.timestamp_micros))? {
            VideoRangeDecision::Before => continue,
            VideoRangeDecision::End => {
                if let Some(writer) = &mut writer {
                    for (chapter, packet) in queued_audio {
                        writer.write_audio(
                            chapter,
                            packet,
                            range
                                .first_included_timestamp
                                .expect("encoded frame origin"),
                            end,
                        )?;
                    }
                }
                break;
            }
            VideoRangeDecision::Include(timestamp) => timestamp,
        };
        while prepared_count <= pair.chapter_index {
            context.check_cancelled()?;
            let chapter = &sequence.chapters[prepared_count];
            let file = File::open(&chapter.inputs.paths()[0])
                .map_err(|error| crate::error::io_error(&chapter.inputs.paths()[0], error))?;
            let mut metadata_reader = crate::InsvReader::new(file)?;
            let current = FileStabilizer::from_reader_continuing(
                &mut metadata_reader,
                &chapter.inspection,
                config,
                stabilizer.as_ref(),
            )?;
            if let Some(current) = &current {
                context.emit(ExportEvent::StabilizationPrepared(current.diagnostics()));
                for warning in current.warnings() {
                    context.emit(ExportEvent::Warning(warning.clone()));
                }
            }
            stabilizer = current;
            calibration = Some(resolve_calibration(&chapter.inspection.metadata, config)?);
            stitcher.set_color_lut(resolve_color_lut(
                &chapter.inspection.metadata,
                config.color_conversion,
            )?);
            prepared_count += 1;
        }
        let media_time = Duration::from_micros(
            u64::try_from(pair.timestamp_micros)
                .map_err(|_| Error::InvalidMedia("negative recording frame time".into()))?,
        );
        if progress.ready() {
            emit_video_progress(
                context,
                ExportPhase::Stitching,
                frames_written,
                total,
                Some(media_time),
                started,
            );
        }
        let motion = stitch_motion(
            stabilizer.as_ref(),
            FrameTimestamp::Pts(pair.source_timestamp_micros),
        )?;
        let decoded = native_pair(pair);
        let projection = video_projection(config, options, &decoded)?;
        let panorama = stitcher.stitch_video(
            decoded,
            calibration.as_ref().expect("prepared calibration"),
            projection,
            &motion,
        )?;
        context.check_cancelled()?;
        let writer = match &mut writer {
            Some(writer) => writer,
            slot @ None => {
                let opened = HevcWriter::new_with_audio(
                    output,
                    panorama.width(),
                    panorama.height(),
                    report.frame_rate,
                    options.quality,
                    HevcEncodingPolicy {
                        acceleration: options.acceleration,
                        gpu_stitching: stitcher.report.selected == EffectiveBackend::Gpu,
                        direct_bt709_yuv: panorama.is_gpu_yuv420(),
                        converted_rec709: stitcher.color_lut.is_some(),
                    },
                    audio_layout.take(),
                )?;
                context.emit(ExportEvent::EncoderSelected(opened.encoder_report.clone()));
                slot.insert(opened)
            }
        };
        writer.write_stitched(&panorama, output_timestamp, frames_written)?;
        stitcher.recycle_video_frame(panorama);
        for (chapter, packet) in queued_audio {
            writer.write_audio(
                chapter,
                packet,
                range
                    .first_included_timestamp
                    .expect("encoded frame origin"),
                end,
            )?;
        }
        frames_written = frames_written
            .checked_add(1)
            .ok_or_else(|| Error::InvalidMedia("frame counter overflow".into()))?;
    }
    let mut writer = writer.ok_or_else(|| {
        Error::InvalidMedia("requested video interval contains no decodable frames".into())
    })?;
    reader.finish_audio(Duration::from_micros(end as u64), &context.cancel)?;
    for (chapter, packet) in reader.take_audio_packets() {
        writer.write_audio(
            chapter,
            packet,
            range
                .first_included_timestamp
                .expect("encoded frame origin"),
            end,
        )?;
    }
    emit_video_progress(
        context,
        ExportPhase::Finalizing,
        frames_written,
        total,
        None,
        started,
    );
    writer.finish(context)?;
    Ok(ExportResult {
        outputs: vec![output.to_path_buf()],
        frames_written,
        elapsed: started.elapsed(),
        backend: stitcher.report,
    })
}

fn native_pair(pair: FramePair) -> SynchronizedPair {
    SynchronizedPair {
        timestamp: FrameTimestamp::Pts(pair.timestamp_micros),
        frames: [
            DecodedVideoFrame::new(pair.a),
            DecodedVideoFrame::new(pair.b),
        ],
    }
}

struct ProgressGate {
    last: Option<Instant>,
}
impl ProgressGate {
    fn new() -> Self {
        Self { last: None }
    }
    fn ready(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last
            .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(100))
        {
            self.last = Some(now);
            true
        } else {
            false
        }
    }
}
