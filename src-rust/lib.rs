//! Portable parsing, calibration, and stitching primitives for Insta360 media.
//!
//! The crate deliberately keeps vendor binaries and application-framework
//! types out of its public API. Media decoding and encoding are optional.

pub mod assets;
pub mod calibration;
pub mod color;
pub mod container;
pub mod error;
#[cfg(feature = "media")]
pub mod extraction;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod motion;
pub mod profile;
pub mod stitch;
#[cfg(feature = "media")]
pub mod stream;
pub mod telemetry;
pub mod timing;
pub mod types;

#[cfg(feature = "media")]
pub mod media;

pub use calibration::{CalibrationResolver, ParsedLens, ResolvedCalibration, ResolvedLensGeometry};
pub use container::{probe, InputSet, InsvReader};
pub use error::{Error, Result};
#[cfg(feature = "media")]
pub use extraction::{extract, ExtractionReport};
pub use motion::{
    AttitudeTrack, FrameMotion, FusionDiagnostics, FusionOptions, MotionSample, Orientation,
    ReadoutDirection, ReadoutPoseTable, Stabilizer,
};
pub use stitch::{CpuStitcher, LensFrame, PanoramaFrame, StitchEngine};
#[cfg(feature = "media")]
pub use stream::{
    DecodedVideoFrame, EncodedPacket, MediaSource, MediaStream, PacketReader, StreamInfo,
    StreamKind, StreamSideData, StreamTimeBase, VideoFrameReader,
};
pub use telemetry::ExposureSample;
pub use types::*;

#[cfg(feature = "media")]
pub use media::{ExportEvent, ExportJob, Exporter};
