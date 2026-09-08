use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Product family recorded by Insta360 camera metadata.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CameraModel {
    /// Insta360 ONE X (internally called One2).
    X1,
    /// Insta360 ONE X2.
    X2,
    /// Insta360 X3.
    X3,
    /// Insta360 X4.
    X4,
    /// Insta360 X5 (internally called A3).
    X5,
    /// Insta360 X6 (internally called C9).
    X6,
    /// Camera name not represented by the portable profile registry.
    Unknown(String),
}

/// Optical accessory or environment encoded by a lens calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OpticalSetup {
    /// Require recorded metadata to select an unambiguous setup.
    StrictAuto,
    /// Bare lenses used in air.
    BareAir,
    /// Bare lenses used underwater.
    BareUnderwater,
    /// Legacy waterproof housing.
    WaterproofCase,
    /// Legacy dive housing used in air.
    DiveCaseAir,
    /// Legacy dive housing used underwater.
    DiveCaseUnderwater,
    /// Full-invisible dive housing used in air.
    InvisibleDiveCaseAir,
    /// Full-invisible dive housing used underwater.
    InvisibleDiveCaseUnderwater,
    /// Clip-on curved lens guard.
    ClipOnLensGuard,
    /// Adhesive spherical lens guard.
    AdhesiveSphereLensGuard,
    /// Standard/plastic A-grade lens protector.
    ProtectorA,
    /// Premium/glass S-grade lens protector.
    ProtectorS,
    /// Average curve used for automatic A/S protector selection.
    ProtectorAS,
    /// ND16 neutral-density filter.
    Nd16,
    /// ND32 neutral-density filter.
    Nd32,
    /// ND64 neutral-density filter.
    Nd64,
    /// ND128 neutral-density filter.
    Nd128,
}

impl OpticalSetup {
    /// Returns the embedded optical profile name used by recorded metadata.
    pub fn profile_name(&self) -> Option<&'static str> {
        match self {
            Self::StrictAuto => None,
            Self::BareAir => Some("bare"),
            Self::BareUnderwater => Some("BareUnderwater"),
            Self::WaterproofCase => Some("Waterproof"),
            Self::DiveCaseAir => Some("DivingAir"),
            Self::DiveCaseUnderwater => Some("DivingWater"),
            Self::InvisibleDiveCaseAir => Some("InvisibleDiveAir"),
            Self::InvisibleDiveCaseUnderwater => Some("InvisibleDiveWater"),
            Self::ClipOnLensGuard => Some("Protect"),
            Self::AdhesiveSphereLensGuard => Some("SphereProtect"),
            Self::ProtectorA => Some("ProtectorA"),
            Self::ProtectorS => Some("ProtectorS"),
            Self::ProtectorAS => Some("ProtectorAS"),
            Self::Nd16 => Some("ND16"),
            Self::Nd32 => Some("ND32"),
            Self::Nd64 => Some("ND64"),
            Self::Nd128 => Some("ND128"),
        }
    }
}

/// Version of the native projection encoded by an Insta360 lens offset.
///
/// The generation number is independent of both the camera generation and the
/// INSV trailer version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProjectionGeneration {
    /// Radius-based polynomial pinhole V1.
    V1,
    /// Four-coefficient polynomial pinhole V2.
    V2,
    /// Unified omnidirectional radtan V3.
    V3,
    /// Thirteen-coefficient unified omnidirectional V6.
    V6,
}

impl ProjectionGeneration {
    /// Converts a serialized offset version into a supported generation.
    pub const fn from_offset_version(version: u8) -> Option<Self> {
        match version {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            3 => Some(Self::V3),
            6 => Some(Self::V6),
            _ => None,
        }
    }

    /// Returns the integer stored in an offset's version field.
    pub const fn offset_version(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
            Self::V6 => 6,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CalibrationPolicy {
    #[default]
    PreferNewest,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Stabilization {
    Off,
    FlowState,
    #[default]
    DirectionLock,
}

/// Whether to correct rotation during each sensor's frame readout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RollingShutterCorrection {
    /// Correct when a supported readout profile exists; report any omission.
    #[default]
    Auto,
    /// Apply only the global stabilization rotation.
    Off,
    /// Fail unless sensor readout correction can be established.
    Required,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProcessingBackend {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

/// The renderer that actually produced an output.
///
/// This is distinct from [`ProcessingBackend`]: an `Auto` request resolves to
/// either `Cpu` or `Gpu` before an export publishes output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EffectiveBackend {
    #[default]
    Cpu,
    Gpu,
}

/// Portable adapter information that does not expose graphics-API types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuAdapterInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub vendor: u32,
    pub device: u32,
    pub driver: String,
    pub driver_info: String,
}

/// Stable category for a GPU selection or execution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GpuFailureCode {
    NotCompiled,
    NoCompatibleAdapter,
    AdapterNotFound,
    UnsupportedLimits,
    DeviceRequest,
    ShaderValidation,
    OutOfMemory,
    DeviceLost,
    Submission,
    Readback,
}

impl GpuFailureCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotCompiled => "not_compiled",
            Self::NoCompatibleAdapter => "no_compatible_adapter",
            Self::AdapterNotFound => "adapter_not_found",
            Self::UnsupportedLimits => "unsupported_limits",
            Self::DeviceRequest => "device_request",
            Self::ShaderValidation => "shader_validation",
            Self::OutOfMemory => "out_of_memory",
            Self::DeviceLost => "device_lost",
            Self::Submission => "submission",
            Self::Readback => "readback",
        }
    }
}

/// Stage at which a GPU failure occurred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GpuFailureStage {
    Discovery,
    Preparation,
    Dispatch,
    Readback,
}

impl GpuFailureStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Preparation => "preparation",
            Self::Dispatch => "dispatch",
            Self::Readback => "readback",
        }
    }
}

/// Backend-neutral diagnostic retained when GPU selection or execution fails.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuFailure {
    pub code: GpuFailureCode,
    pub stage: GpuFailureStage,
    pub message: String,
    pub adapter: Option<GpuAdapterInfo>,
}

impl GpuFailure {
    pub fn new(code: GpuFailureCode, stage: GpuFailureStage, message: impl Into<String>) -> Self {
        Self {
            code,
            stage,
            message: message.into(),
            adapter: None,
        }
    }

    pub fn with_adapter(mut self, adapter: GpuAdapterInfo) -> Self {
        self.adapter = Some(adapter);
        self
    }
}

impl std::fmt::Display for GpuFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} during GPU {}: {}",
            self.code.as_str(),
            self.stage.as_str(),
            self.message
        )
    }
}

/// Records the requested renderer, the renderer actually used, and any
/// non-fatal automatic fallback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendReport {
    pub requested: ProcessingBackend,
    pub selected: EffectiveBackend,
    pub adapter: Option<GpuAdapterInfo>,
    pub fallback: Option<GpuFailure>,
}

impl BackendReport {
    pub fn cpu(requested: ProcessingBackend) -> Self {
        Self {
            requested,
            selected: EffectiveBackend::Cpu,
            adapter: None,
            fallback: None,
        }
    }

    pub fn gpu(requested: ProcessingBackend, adapter: GpuAdapterInfo) -> Self {
        Self {
            requested,
            selected: EffectiveBackend::Gpu,
            adapter: Some(adapter),
            fallback: None,
        }
    }

    pub fn cpu_fallback(requested: ProcessingBackend, failure: GpuFailure) -> Self {
        Self {
            requested,
            selected: EffectiveBackend::Cpu,
            adapter: None,
            fallback: Some(failure),
        }
    }
}

impl Default for BackendReport {
    fn default() -> Self {
        Self::cpu(ProcessingBackend::Auto)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SeamMode {
    #[default]
    Fixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquirectangularProjection {
    pub width: u32,
    pub height: u32,
}

impl EquirectangularProjection {
    pub fn validate(self) -> crate::Result<Self> {
        if self.width == 0 || self.height == 0 || self.height.checked_mul(2) != Some(self.width) {
            return Err(crate::Error::InvalidMedia(
                "equirectangular output must have a non-zero 2:1 size".into(),
            ));
        }
        Ok(self)
    }
}

/// Controls conversion from a camera's recorded color profile to display RGB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ColorConversion {
    /// Convert positively identified, supported I-Log recordings to Rec.709.
    #[default]
    Auto,
    /// Keep the recorded color encoding for downstream grading.
    Preserve,
    /// Explicitly treat the input as I-Log using its camera's bundled Rec.709 LUT.
    ILogToRec709,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StitchConfig {
    pub optical_setup: OpticalSetup,
    pub calibration_policy: CalibrationPolicy,
    pub stabilization: Stabilization,
    #[serde(default)]
    pub rolling_shutter: RollingShutterCorrection,
    pub seam_mode: SeamMode,
    pub backend: ProcessingBackend,
    pub projection: Option<EquirectangularProjection>,
    /// Camera-specific I-Log conversion, applied after stitching.
    #[serde(default)]
    pub color_conversion: ColorConversion,
}

impl Default for StitchConfig {
    fn default() -> Self {
        Self {
            optical_setup: OpticalSetup::StrictAuto,
            calibration_policy: CalibrationPolicy::PreferNewest,
            stabilization: Stabilization::DirectionLock,
            rolling_shutter: RollingShutterCorrection::Auto,
            seam_mode: SeamMode::Fixed,
            backend: ProcessingBackend::Auto,
            projection: None,
            color_conversion: ColorConversion::Auto,
        }
    }
}

impl StitchConfig {
    pub fn underwater_photogrammetry(optical_setup: OpticalSetup) -> crate::Result<Self> {
        if !matches!(
            optical_setup,
            OpticalSetup::BareUnderwater
                | OpticalSetup::WaterproofCase
                | OpticalSetup::DiveCaseUnderwater
                | OpticalSetup::InvisibleDiveCaseUnderwater
        ) {
            return Err(crate::Error::InvalidMedia(
                "underwater photogrammetry requires an underwater optical setup".into(),
            ));
        }
        Ok(Self {
            optical_setup,
            ..Self::default()
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoTrackInfo {
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub codec: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub inputs: Vec<PathBuf>,
    pub camera: CameraModel,
    pub camera_name: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub duration: Option<Duration>,
    pub fps: Option<f64>,
    pub video_tracks: Vec<VideoTrackInfo>,
    pub offset_versions: Vec<u8>,
    pub optical_profiles: Vec<String>,
    pub gyro_sample_count: u64,
    pub exposure_sample_count: u64,
    pub trailer: TrailerInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrailerInfo {
    pub offset: u64,
    pub size: u64,
    pub version: u8,
    pub record_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FrameSelection {
    Indices(Vec<u64>),
    Timestamps(Vec<Duration>),
    SampledRange {
        start: Duration,
        end: Duration,
        frames_per_second_milli: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ImageFormat {
    Png,
    Jpeg,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageExportOptions {
    pub format: ImageFormat,
    pub quality: u8,
    pub scale_width: Option<u32>,
}

impl Default for ImageExportOptions {
    fn default() -> Self {
        Self {
            format: ImageFormat::Png,
            quality: 95,
            scale_width: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AudioPolicy {
    #[default]
    Copy,
    Drop,
}

/// Decoder/encoder hardware acceleration policy, independent of stitching.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MediaAcceleration {
    #[default]
    Auto,
    Software,
    Hardware,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoExportOptions {
    pub quality: u8,
    pub audio: AudioPolicy,
    #[serde(default)]
    pub acceleration: MediaAcceleration,
    pub projection: Option<EquirectangularProjection>,
    /// Source-relative start of the exported interval. `None` starts at zero.
    #[serde(default)]
    pub start: Option<Duration>,
    /// Length of the exported interval. `None` exports through end of source.
    #[serde(default)]
    pub duration: Option<Duration>,
}

impl Default for VideoExportOptions {
    fn default() -> Self {
        Self {
            quality: 90,
            audio: AudioPolicy::Copy,
            acceleration: MediaAcceleration::Auto,
            projection: None,
            start: None,
            duration: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportResult {
    pub outputs: Vec<PathBuf>,
    pub frames_written: u64,
    pub elapsed: Duration,
    /// Backend selection used for every frame in this result.
    #[serde(default)]
    pub backend: BackendReport,
}
