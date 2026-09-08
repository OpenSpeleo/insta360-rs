use std::path::PathBuf;
use std::time::Instant;

use crate::{
    ExportResult, FrameSelection, ImageExportOptions, InputSet, Result, StitchConfig,
    VideoExportOptions,
};

use super::{ExportContext, ExportEvent, ExportPhase, ExportProgress};

pub(super) fn export_frames(
    inputs: InputSet,
    config: StitchConfig,
    output_dir: PathBuf,
    selection: FrameSelection,
    options: ImageExportOptions,
    context: ExportContext,
) -> Result<ExportResult> {
    let started = Instant::now();
    context.check_cancelled()?;
    context.emit(ExportEvent::Progress(ExportProgress {
        phase: ExportPhase::Probing,
        completed: 0,
        total: None,
        media_time: None,
        elapsed: started.elapsed(),
        estimated_remaining: None,
    }));
    super::pipeline_impl::export_frames(
        inputs, config, output_dir, selection, options, context, started,
    )
}

pub(super) fn export_video(
    inputs: InputSet,
    config: StitchConfig,
    output: PathBuf,
    options: VideoExportOptions,
    context: ExportContext,
) -> Result<ExportResult> {
    let started = Instant::now();
    context.check_cancelled()?;
    context.emit(ExportEvent::Progress(ExportProgress {
        phase: ExportPhase::Probing,
        completed: 0,
        total: None,
        media_time: None,
        elapsed: started.elapsed(),
        estimated_remaining: None,
    }));
    super::pipeline_impl::export_video(inputs, config, output, options, context, started)
}
