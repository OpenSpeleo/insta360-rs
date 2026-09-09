//! Optional high-level media decoding and export.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::{
    BackendReport, EquirectangularProjection, ExportResult, FrameSelection, GpuAdapterInfo,
    ImageExportOptions, InputSet, RecordingSequence, Result, StitchConfig, VideoExportOptions,
};

#[cfg(feature = "cli")]
pub mod cli;
mod pipeline;
mod pipeline_impl;
mod stabilization;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ExportPhase {
    Probing,
    Decoding,
    Stitching,
    Encoding,
    Finalizing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportProgress {
    pub phase: ExportPhase,
    pub completed: u64,
    pub total: Option<u64>,
    pub media_time: Option<Duration>,
    pub elapsed: Duration,
    pub estimated_remaining: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ExportEvent {
    Progress(ExportProgress),
    /// Emitted when a renderer is prepared and pinned for an export attempt.
    /// `Auto` may emit it again after restarting a failed GPU attempt on CPU.
    BackendSelected(Box<BackendReport>),
    /// The encoder successfully opened for this attempt (not merely compiled in).
    EncoderSelected(EncoderReport),
    /// Describes the established motion profile and timing strategy for this attempt.
    StabilizationPrepared(String),
    Warning(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderReport {
    pub name: String,
    pub hardware: bool,
    pub audio_tracks: usize,
}

/// Validated source and output properties for an export panel. Encoder names
/// are candidates; `EncoderSelected` reports the one that actually opens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoPreflight {
    pub projection: EquirectangularProjection,
    pub duration: Duration,
    pub frame_rate: f64,
    pub chapter_count: usize,
    pub audio_tracks: usize,
    pub encoder_candidates: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCapabilities {
    pub image_export: bool,
    pub video_export: bool,
    pub gpu_compiled: bool,
    pub gpu_available: bool,
    pub gpu_adapters: Vec<GpuAdapterInfo>,
    pub gpu_unavailable_reason: Option<String>,
    pub hevc_encoders: Vec<String>,
}

impl MediaCapabilities {
    pub fn detect() -> Self {
        let initialized = ffmpeg_next::init().is_ok();
        let hevc_encoders = if initialized {
            [
                "hevc_videotoolbox",
                "hevc_mf",
                "hevc_nvenc",
                "hevc_amf",
                "hevc_vaapi",
                "libkvazaar",
                "libx265",
            ]
            .into_iter()
            .filter(|name| ffmpeg_next::encoder::find_by_name(name).is_some())
            .map(str::to_owned)
            .collect()
        } else {
            Vec::new()
        };

        #[cfg(feature = "gpu")]
        let gpu_adapters = crate::gpu::available_adapters();
        #[cfg(not(feature = "gpu"))]
        let gpu_adapters = Vec::new();
        let gpu_available = !gpu_adapters.is_empty();
        let gpu_unavailable_reason = if gpu_available {
            None
        } else if cfg!(feature = "gpu") {
            Some("no compatible GPU adapter was found".to_owned())
        } else {
            Some("this build was compiled without GPU support".to_owned())
        };

        Self {
            image_export: initialized,
            video_export: !hevc_encoders.is_empty(),
            gpu_compiled: cfg!(feature = "gpu"),
            gpu_available,
            gpu_adapters,
            gpu_unavailable_reason,
            hevc_encoders,
        }
    }
}

/// Configured high-level exporter.
#[derive(Clone, Debug)]
pub struct Exporter {
    inputs: InputSet,
    sequence: Option<RecordingSequence>,
    config: StitchConfig,
}

impl Exporter {
    pub fn new(inputs: InputSet, config: StitchConfig) -> Result<Self> {
        if let Some(projection) = config.projection {
            projection.validate()?;
        }
        Ok(Self {
            inputs,
            sequence: None,
            config,
        })
    }

    /// Configures one output across ordered, validated recording chapters.
    pub fn from_sequence(sequence: RecordingSequence, config: StitchConfig) -> Result<Self> {
        sequence.require_complete()?;
        let inputs = sequence
            .chapters
            .first()
            .ok_or_else(|| crate::Error::InvalidMedia("recording sequence is empty".into()))?
            .inputs
            .clone();
        let mut exporter = Self::new(inputs, config)?;
        exporter.sequence = Some(sequence);
        Ok(exporter)
    }

    /// Resolves camera/calibration/timing and every implemented video option
    /// without writing output. Call off the application's UI thread.
    pub fn preflight_video(&self, options: &VideoExportOptions) -> Result<VideoPreflight> {
        let sequence = self
            .sequence
            .clone()
            .map(Ok)
            .unwrap_or_else(|| RecordingSequence::single(self.inputs.clone()))?;
        pipeline_impl::preflight_video(&sequence, &self.config, options)
    }

    pub fn export_frames(
        &self,
        output_dir: impl AsRef<Path>,
        selection: FrameSelection,
        options: ImageExportOptions,
    ) -> ExportJob {
        let inputs = self.inputs.clone();
        let config = self.config.clone();
        let output_dir = output_dir.as_ref().to_path_buf();
        let multiple_chapters = self
            .sequence
            .as_ref()
            .is_some_and(|sequence| sequence.chapters.len() > 1);
        spawn_export(move |context| {
            if multiple_chapters {
                return Err(crate::Error::MissingCapability("stitched still export currently accepts one chapter; use paired stream access for sequence fisheye images".into()));
            }
            pipeline::export_frames(inputs, config, output_dir, selection, options, context)
        })
    }

    pub fn export_video(&self, output: impl AsRef<Path>, options: VideoExportOptions) -> ExportJob {
        let inputs = self.inputs.clone();
        let config = self.config.clone();
        let output = output.as_ref().to_path_buf();
        let sequence = self.sequence.clone();
        spawn_export(move |context| {
            let sequence = sequence
                .map(Ok)
                .unwrap_or_else(|| RecordingSequence::single(inputs))?;
            pipeline::export_video(sequence, config, output, options, context)
        })
    }
}

pub struct ExportJob {
    cancel: Arc<AtomicBool>,
    events: Receiver<ExportEvent>,
    worker: Option<JoinHandle<Result<ExportResult>>>,
}

impl std::fmt::Debug for ExportJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExportJob")
            .field("cancelled", &self.cancel.load(Ordering::Relaxed))
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

impl ExportJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn try_event(&self) -> Option<ExportEvent> {
        self.events.try_recv().ok()
    }

    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<ExportEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    pub fn wait(mut self) -> Result<ExportResult> {
        let worker = self.worker.take().ok_or_else(|| {
            crate::Error::Media("export worker result was already consumed".into())
        })?;
        worker
            .join()
            .map_err(|_| crate::Error::Media("export worker panicked".into()))?
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            self.cancel();
        }
    }
}

#[derive(Clone)]
pub(crate) struct ExportContext {
    cancel: Arc<AtomicBool>,
    events: Sender<ExportEvent>,
}

impl ExportContext {
    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err(crate::Error::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn emit(&self, event: ExportEvent) {
        let _ = self.events.try_send(event);
    }
}

fn spawn_export(
    operation: impl FnOnce(ExportContext) -> Result<ExportResult> + Send + 'static,
) -> ExportJob {
    const EVENT_CAPACITY: usize = 32;
    let cancel = Arc::new(AtomicBool::new(false));
    let (events_tx, events_rx) = crossbeam_channel::bounded(EVENT_CAPACITY);
    let context = ExportContext {
        cancel: Arc::clone(&cancel),
        events: events_tx,
    };
    let worker = std::thread::Builder::new()
        .name("insta360-export".into())
        .spawn(move || operation(context))
        .expect("failed to spawn insta360 export worker");

    ExportJob {
        cancel,
        events: events_rx,
        worker: Some(worker),
    }
}

pub(crate) fn temporary_output_path(output: &Path) -> PathBuf {
    let mut temporary = output.as_os_str().to_owned();
    temporary.push(".insta360-rs-part");
    PathBuf::from(temporary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_output_is_a_sibling_with_a_distinct_name() {
        let output = Path::new("/tmp/panorama.mp4");
        assert_eq!(
            temporary_output_path(output),
            PathBuf::from("/tmp/panorama.mp4.insta360-rs-part")
        );
    }

    #[test]
    fn dropping_running_job_requests_cancellation() {
        let job = spawn_export(|context| {
            while context.check_cancelled().is_ok() {
                std::thread::yield_now();
            }
            Err(crate::Error::Cancelled)
        });
        let cancel = Arc::clone(&job.cancel);
        drop(job);
        assert!(cancel.load(Ordering::Acquire));
    }
}
