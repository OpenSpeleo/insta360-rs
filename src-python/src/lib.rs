use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use insta360_rs::media::{
    ExportPhase as CoreExportPhase, ExportProgress as CoreExportProgress, MediaCapabilities,
};
use insta360_rs::{
    probe as probe_media, AudioPolicy as CoreAudioPolicy, BackendReport as CoreBackendReport,
    CameraModel, ColorConversion as CoreColorConversion, EffectiveBackend as CoreEffectiveBackend,
    Error as CoreError, ExportEvent, ExportJob as CoreExportJob, ExportResult as CoreExportResult,
    Exporter, ExtractionReport as CoreExtractionReport, FrameSelection,
    GpuAdapterInfo as CoreGpuAdapterInfo, GpuFailure as CoreGpuFailure, ImageExportOptions,
    ImageFormat as CoreImageFormat, InputSet, MediaAcceleration as CoreMediaAcceleration,
    ProcessingBackend as CoreProcessingBackend,
    RollingShutterCorrection as CoreRollingShutterCorrection, Stabilization as CoreStabilization,
    StitchConfig as CoreStitchConfig, VideoExportOptions,
};
use insta360_rs::{
    DecodedVideoFrame as CoreDecodedVideoFrame, EncodedPacket as CoreEncodedPacket,
    MediaSource as CoreMediaSource, MediaStream as CoreMediaStream,
    PacketReader as CorePacketReader, StreamInfo as CoreStreamInfo, StreamKind,
    StreamSideData as CoreStreamSideData, VideoFrameReader as CoreVideoFrameReader,
};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

create_exception!(_native, Insta360Error, PyException);
create_exception!(_native, Insta360IOError, Insta360Error);
create_exception!(_native, InvalidMediaError, Insta360Error);
create_exception!(_native, UnsupportedCameraError, Insta360Error);
create_exception!(_native, MissingCalibrationError, Insta360Error);
create_exception!(_native, AmbiguousOpticalSetupError, Insta360Error);
create_exception!(_native, ConflictingOpticsError, Insta360Error);
create_exception!(_native, MissingCapabilityError, Insta360Error);
create_exception!(_native, GpuUnavailableError, MissingCapabilityError);
create_exception!(_native, CancelledError, Insta360Error);
create_exception!(_native, MediaProcessingError, Insta360Error);
create_exception!(_native, GpuProcessingError, MediaProcessingError);

mod optical_config;
use optical_config::*;

#[pyclass(
    name = "Stabilization",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyStabilization {
    Off,
    FlowState,
    DirectionLock,
}

impl From<PyStabilization> for CoreStabilization {
    fn from(value: PyStabilization) -> Self {
        match value {
            PyStabilization::Off => Self::Off,
            PyStabilization::FlowState => Self::FlowState,
            PyStabilization::DirectionLock => Self::DirectionLock,
        }
    }
}

#[pyclass(
    name = "RollingShutterCorrection",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyRollingShutterCorrection {
    Auto,
    Off,
    Required,
}

impl From<PyRollingShutterCorrection> for CoreRollingShutterCorrection {
    fn from(value: PyRollingShutterCorrection) -> Self {
        match value {
            PyRollingShutterCorrection::Auto => Self::Auto,
            PyRollingShutterCorrection::Off => Self::Off,
            PyRollingShutterCorrection::Required => Self::Required,
        }
    }
}

#[pyclass(
    name = "ProcessingBackend",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyProcessingBackend {
    Auto,
    Cpu,
    Gpu,
}

impl From<PyProcessingBackend> for CoreProcessingBackend {
    fn from(value: PyProcessingBackend) -> Self {
        match value {
            PyProcessingBackend::Auto => Self::Auto,
            PyProcessingBackend::Cpu => Self::Cpu,
            PyProcessingBackend::Gpu => Self::Gpu,
        }
    }
}

impl From<CoreProcessingBackend> for PyProcessingBackend {
    fn from(value: CoreProcessingBackend) -> Self {
        match value {
            CoreProcessingBackend::Auto => Self::Auto,
            CoreProcessingBackend::Cpu => Self::Cpu,
            CoreProcessingBackend::Gpu => Self::Gpu,
            _ => Self::Auto,
        }
    }
}

/// Conversion from the recording's color encoding to the exported image encoding.
#[pyclass(
    name = "ColorConversion",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyColorConversion {
    Auto,
    Preserve,
    ILogToRec709,
}

impl From<PyColorConversion> for CoreColorConversion {
    fn from(value: PyColorConversion) -> Self {
        match value {
            PyColorConversion::Auto => Self::Auto,
            PyColorConversion::Preserve => Self::Preserve,
            PyColorConversion::ILogToRec709 => Self::ILogToRec709,
        }
    }
}

#[pyclass(
    name = "EffectiveBackend",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyEffectiveBackend {
    Cpu,
    Gpu,
    Unknown,
}

impl From<CoreEffectiveBackend> for PyEffectiveBackend {
    fn from(value: CoreEffectiveBackend) -> Self {
        match value {
            CoreEffectiveBackend::Cpu => Self::Cpu,
            CoreEffectiveBackend::Gpu => Self::Gpu,
            _ => Self::Unknown,
        }
    }
}

#[pyclass(
    name = "ImageFormat",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyImageFormat {
    Png,
    Jpeg,
}

impl From<PyImageFormat> for CoreImageFormat {
    fn from(value: PyImageFormat) -> Self {
        match value {
            PyImageFormat::Png => Self::Png,
            PyImageFormat::Jpeg => Self::Jpeg,
        }
    }
}

#[pyclass(
    name = "AudioPolicy",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyAudioPolicy {
    Copy,
    Drop,
}

impl From<PyAudioPolicy> for CoreAudioPolicy {
    fn from(value: PyAudioPolicy) -> Self {
        match value {
            PyAudioPolicy::Copy => Self::Copy,
            PyAudioPolicy::Drop => Self::Drop,
        }
    }
}

#[pyclass(
    name = "MediaAcceleration",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyMediaAcceleration {
    Auto,
    Software,
    Hardware,
}

impl From<PyMediaAcceleration> for CoreMediaAcceleration {
    fn from(value: PyMediaAcceleration) -> Self {
        match value {
            PyMediaAcceleration::Auto => Self::Auto,
            PyMediaAcceleration::Software => Self::Software,
            PyMediaAcceleration::Hardware => Self::Hardware,
        }
    }
}

#[pyclass(
    name = "ExportPhase",
    module = "insta360_rs._native",
    eq,
    eq_int,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PyExportPhase {
    Probing,
    Decoding,
    Stitching,
    Encoding,
    Finalizing,
    Unknown,
}

impl From<CoreExportPhase> for PyExportPhase {
    fn from(value: CoreExportPhase) -> Self {
        match value {
            CoreExportPhase::Probing => Self::Probing,
            CoreExportPhase::Decoding => Self::Decoding,
            CoreExportPhase::Stitching => Self::Stitching,
            CoreExportPhase::Encoding => Self::Encoding,
            CoreExportPhase::Finalizing => Self::Finalizing,
            _ => Self::Unknown,
        }
    }
}

/// User-facing stitching configuration.
#[pyclass(name = "StitchConfig", module = "insta360_rs._native", from_py_object)]
#[derive(Clone, Debug)]
struct PyStitchConfig {
    #[pyo3(get, set)]
    housing: PyHousing,
    #[pyo3(get, set)]
    environment: PyEnvironment,
    #[pyo3(get, set)]
    lens_accessory: PyLensAccessory,
    #[pyo3(get, set)]
    mounting_accessory: PyMountingAccessory,
    #[pyo3(get, set)]
    underwater_color: PyUnderwaterColorOptions,
    #[pyo3(get, set)]
    stabilization: PyStabilization,
    #[pyo3(get, set)]
    rolling_shutter: PyRollingShutterCorrection,
    #[pyo3(get, set)]
    backend: PyProcessingBackend,
    #[pyo3(get, set)]
    color_conversion: PyColorConversion,
    #[pyo3(get, set)]
    width: Option<u32>,
    #[pyo3(get, set)]
    height: Option<u32>,
}

#[pymethods]
impl PyStitchConfig {
    #[new]
    #[pyo3(signature = (*, housing=None, environment=None, lens_accessory=None, mounting_accessory=None, underwater_color=None, stabilization=None, rolling_shutter=None, backend=None, color_conversion=None, width=None, height=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        housing: Option<PyHousing>,
        environment: Option<PyEnvironment>,
        lens_accessory: Option<PyLensAccessory>,
        mounting_accessory: Option<PyMountingAccessory>,
        underwater_color: Option<PyUnderwaterColorOptions>,
        stabilization: Option<PyStabilization>,
        rolling_shutter: Option<PyRollingShutterCorrection>,
        backend: Option<PyProcessingBackend>,
        color_conversion: Option<PyColorConversion>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> PyResult<Self> {
        let config = Self {
            housing: housing.unwrap_or(PyHousing::Auto),
            environment: environment.unwrap_or(PyEnvironment::Auto),
            lens_accessory: lens_accessory.unwrap_or(PyLensAccessory::Auto),
            mounting_accessory: mounting_accessory.unwrap_or(PyMountingAccessory::Auto),
            underwater_color: underwater_color.unwrap_or_default(),
            stabilization: stabilization.unwrap_or(PyStabilization::DirectionLock),
            rolling_shutter: rolling_shutter.unwrap_or(PyRollingShutterCorrection::Auto),
            backend: backend.unwrap_or(PyProcessingBackend::Auto),
            color_conversion: color_conversion.unwrap_or(PyColorConversion::Auto),
            width,
            height,
        };
        config.to_core().map_err(to_py_error)?;
        Ok(config)
    }

    #[staticmethod]
    #[pyo3(signature = (*, housing=None, mounting_accessory=None, underwater_color=None, rolling_shutter=None, backend=None, color_conversion=None, width=None, height=None))]
    #[allow(clippy::too_many_arguments)]
    fn underwater_photogrammetry(
        housing: Option<PyHousing>,
        mounting_accessory: Option<PyMountingAccessory>,
        underwater_color: Option<PyUnderwaterColorOptions>,
        rolling_shutter: Option<PyRollingShutterCorrection>,
        backend: Option<PyProcessingBackend>,
        color_conversion: Option<PyColorConversion>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> PyResult<Self> {
        let config = Self {
            housing: housing.unwrap_or(PyHousing::Auto),
            environment: PyEnvironment::Underwater,
            lens_accessory: PyLensAccessory::Auto,
            mounting_accessory: mounting_accessory.unwrap_or(PyMountingAccessory::Auto),
            underwater_color: underwater_color.unwrap_or_default(),
            stabilization: PyStabilization::DirectionLock,
            rolling_shutter: rolling_shutter.unwrap_or(PyRollingShutterCorrection::Auto),
            backend: backend.unwrap_or(PyProcessingBackend::Auto),
            color_conversion: color_conversion.unwrap_or(PyColorConversion::Auto),
            width,
            height,
        };
        let core = config.to_core().map_err(to_py_error)?;
        CoreStitchConfig::underwater_photogrammetry(core.housing).map_err(to_py_error)?;
        Ok(config)
    }

    fn __repr__(&self) -> String {
        format!(
            "StitchConfig(housing={:?}, environment={:?}, lens_accessory={:?}, mounting_accessory={:?}, underwater_color={:?}, stabilization={:?}, rolling_shutter={:?}, backend={:?}, color_conversion={:?}, width={:?}, height={:?})",
            self.housing, self.environment, self.lens_accessory, self.mounting_accessory, self.underwater_color, self.stabilization, self.rolling_shutter, self.backend, self.color_conversion, self.width, self.height
        )
    }
}

impl PyStitchConfig {
    fn to_core(&self) -> insta360_rs::Result<CoreStitchConfig> {
        let projection = match (self.width, self.height) {
            (None, None) => None,
            (Some(width), Some(height)) => {
                Some(insta360_rs::EquirectangularProjection { width, height }.validate()?)
            }
            _ => {
                return Err(CoreError::InvalidMedia(
                    "output width and height must be specified together".into(),
                ));
            }
        };

        Ok(CoreStitchConfig {
            housing: self.housing.into(),
            environment: self.environment.into(),
            lens_accessory: self.lens_accessory.into(),
            mounting_accessory: self.mounting_accessory.into(),
            underwater_color: self.underwater_color.to_core()?,
            stabilization: self.stabilization.into(),
            rolling_shutter: self.rolling_shutter.into(),
            backend: self.backend.into(),
            color_conversion: self.color_conversion.into(),
            projection,
            ..CoreStitchConfig::default()
        })
    }
}

#[pyclass(
    name = "VideoTrackInfo",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyVideoTrackInfo {
    #[pyo3(get)]
    index: usize,
    #[pyo3(get)]
    width: u32,
    #[pyo3(get)]
    height: u32,
    #[pyo3(get)]
    codec: String,
}

#[pyclass(
    name = "TrailerInfo",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyTrailerInfo {
    #[pyo3(get)]
    offset: u64,
    #[pyo3(get)]
    size: u64,
    #[pyo3(get)]
    version: u8,
    #[pyo3(get)]
    record_count: u32,
}

#[pyclass(
    name = "MediaInfo",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyMediaInfo {
    #[pyo3(get)]
    optics: PyOpticalInspection,
    #[pyo3(get)]
    inputs: Vec<PathBuf>,
    #[pyo3(get)]
    camera: String,
    #[pyo3(get)]
    camera_name: Option<String>,
    #[pyo3(get)]
    serial: Option<String>,
    #[pyo3(get)]
    firmware: Option<String>,
    #[pyo3(get)]
    duration_seconds: Option<f64>,
    #[pyo3(get)]
    fps: Option<f64>,
    #[pyo3(get)]
    video_tracks: Vec<PyVideoTrackInfo>,
    #[pyo3(get)]
    // Vec<u8> becomes Python bytes in PyO3; this API promises a list of versions.
    offset_versions: Vec<u32>,
    #[pyo3(get)]
    optical_profiles: Vec<String>,
    #[pyo3(get)]
    gyro_sample_count: u64,
    #[pyo3(get)]
    exposure_sample_count: u64,
    #[pyo3(get)]
    trailer: PyTrailerInfo,
}

impl From<insta360_rs::MediaInfo> for PyMediaInfo {
    fn from(value: insta360_rs::MediaInfo) -> Self {
        let camera = match &value.camera {
            CameraModel::One => "ONE".to_owned(),
            CameraModel::OneR => "ONE R".to_owned(),
            CameraModel::OneRS => "ONE RS".to_owned(),
            CameraModel::X4Air => "X4 Air".to_owned(),
            CameraModel::X1 => "X1".to_owned(),
            CameraModel::X2 => "X2".to_owned(),
            CameraModel::X3 => "X3".to_owned(),
            CameraModel::X4 => "X4".to_owned(),
            CameraModel::X5 => "X5".to_owned(),
            CameraModel::X6 => "X6".to_owned(),
            CameraModel::Unknown(name) => name.clone(),
            _ => "unknown".to_owned(),
        };
        let video_tracks = value
            .video_tracks
            .into_iter()
            .map(|track| PyVideoTrackInfo {
                index: track.index,
                width: track.width,
                height: track.height,
                codec: track.codec,
            })
            .collect();
        Self {
            inputs: value.inputs,
            camera,
            optics: value.optics.into(),
            camera_name: value.camera_name,
            serial: value.serial,
            firmware: value.firmware,
            duration_seconds: value.duration.map(|duration| duration.as_secs_f64()),
            fps: value.fps,
            video_tracks,
            offset_versions: value.offset_versions.into_iter().map(u32::from).collect(),
            optical_profiles: value.optical_profiles,
            gyro_sample_count: value.gyro_sample_count,
            exposure_sample_count: value.exposure_sample_count,
            trailer: PyTrailerInfo {
                offset: value.trailer.offset,
                size: value.trailer.size,
                version: value.trailer.version,
                record_count: value.trailer.record_count,
            },
        }
    }
}

#[pyclass(
    name = "GpuAdapterInfo",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyGpuAdapterInfo {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    backend: String,
    #[pyo3(get)]
    device_type: String,
    #[pyo3(get)]
    vendor: u32,
    #[pyo3(get)]
    device: u32,
    #[pyo3(get)]
    driver: String,
    #[pyo3(get)]
    driver_info: String,
}

impl From<CoreGpuAdapterInfo> for PyGpuAdapterInfo {
    fn from(value: CoreGpuAdapterInfo) -> Self {
        Self {
            name: value.name,
            backend: value.backend,
            device_type: value.device_type,
            vendor: value.vendor,
            device: value.device,
            driver: value.driver,
            driver_info: value.driver_info,
        }
    }
}

#[pyclass(
    name = "GpuFailure",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyGpuFailure {
    #[pyo3(get)]
    code: String,
    #[pyo3(get)]
    stage: String,
    #[pyo3(get)]
    message: String,
    #[pyo3(get)]
    adapter: Option<PyGpuAdapterInfo>,
}

impl From<CoreGpuFailure> for PyGpuFailure {
    fn from(value: CoreGpuFailure) -> Self {
        Self {
            code: value.code.as_str().to_owned(),
            stage: value.stage.as_str().to_owned(),
            message: value.message,
            adapter: value.adapter.map(Into::into),
        }
    }
}

#[pyclass(
    name = "BackendReport",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyBackendReport {
    #[pyo3(get)]
    requested: PyProcessingBackend,
    #[pyo3(get)]
    selected: PyEffectiveBackend,
    #[pyo3(get)]
    adapter: Option<PyGpuAdapterInfo>,
    #[pyo3(get)]
    fallback: Option<PyGpuFailure>,
}

impl From<CoreBackendReport> for PyBackendReport {
    fn from(value: CoreBackendReport) -> Self {
        Self {
            requested: value.requested.into(),
            selected: value.selected.into(),
            adapter: value.adapter.map(Into::into),
            fallback: value.fallback.map(Into::into),
        }
    }
}

#[pyclass(
    name = "ExportResult",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyExportResult {
    #[pyo3(get)]
    optics: Option<PyOpticalResolution>,
    #[pyo3(get)]
    outputs: Vec<PathBuf>,
    #[pyo3(get)]
    frames_written: u64,
    #[pyo3(get)]
    elapsed_seconds: f64,
    #[pyo3(get)]
    backend: PyBackendReport,
}

impl From<CoreExportResult> for PyExportResult {
    fn from(value: CoreExportResult) -> Self {
        Self {
            optics: value.optics.map(Into::into),
            outputs: value.outputs,
            frames_written: value.frames_written,
            elapsed_seconds: value.elapsed.as_secs_f64(),
            backend: value.backend.into(),
        }
    }
}

#[pyclass(
    name = "ExtractionReport",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyExtractionReport {
    #[pyo3(get)]
    output_dir: PathBuf,
    #[pyo3(get)]
    manifest_path: PathBuf,
    #[pyo3(get)]
    input_count: usize,
    #[pyo3(get)]
    stream_count: usize,
    #[pyo3(get)]
    record_count: usize,
    #[pyo3(get)]
    files: Vec<PathBuf>,
    #[pyo3(get)]
    warnings: Vec<String>,
}

impl From<CoreExtractionReport> for PyExtractionReport {
    fn from(value: CoreExtractionReport) -> Self {
        Self {
            output_dir: value.output_dir,
            manifest_path: value.manifest_path,
            input_count: value.input_count,
            stream_count: value.stream_count,
            record_count: value.record_count,
            files: value.files,
            warnings: value.warnings,
        }
    }
}

#[pyclass(
    name = "StreamInfo",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyStreamInfo {
    #[pyo3(get)]
    input_index: usize,
    #[pyo3(get)]
    stream_index: usize,
    #[pyo3(get)]
    kind: &'static str,
    #[pyo3(get)]
    codec: String,
    #[pyo3(get)]
    codec_id: i32,
    #[pyo3(get)]
    time_base: (i32, i32),
    #[pyo3(get)]
    start_time: Option<i64>,
    #[pyo3(get)]
    duration: Option<i64>,
    #[pyo3(get)]
    width: u32,
    #[pyo3(get)]
    height: u32,
    codec_extradata: Vec<u8>,
}

#[pymethods]
impl PyStreamInfo {
    #[getter]
    fn codec_extradata<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.codec_extradata)
    }
}

impl From<CoreStreamInfo> for PyStreamInfo {
    fn from(info: CoreStreamInfo) -> Self {
        Self {
            input_index: info.input_index,
            stream_index: info.stream_index,
            kind: match info.kind {
                StreamKind::Video => "video",
                StreamKind::Audio => "audio",
                StreamKind::Data => "data",
                StreamKind::Subtitle => "subtitle",
                StreamKind::Attachment => "attachment",
                _ => "unknown",
            },
            codec: info.codec,
            codec_id: info.codec_id,
            time_base: (info.time_base.numerator, info.time_base.denominator),
            start_time: info.start_time,
            duration: info.duration,
            width: info.width,
            height: info.height,
            codec_extradata: info.codec_extradata,
        }
    }
}

#[pyclass(
    name = "MediaSource",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
struct PyMediaSource {
    inner: CoreMediaSource,
}

#[pymethods]
impl PyMediaSource {
    #[getter]
    fn streams(&self) -> Vec<PyMediaStream> {
        self.inner
            .streams()
            .iter()
            .cloned()
            .map(|inner| PyMediaStream { inner })
            .collect()
    }

    #[getter]
    fn video_streams(&self) -> Vec<PyMediaStream> {
        self.inner
            .video_streams()
            .cloned()
            .map(|inner| PyMediaStream { inner })
            .collect()
    }
}

#[pyclass(
    name = "MediaStream",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
struct PyMediaStream {
    inner: CoreMediaStream,
}

#[pymethods]
impl PyMediaStream {
    #[getter]
    fn info(&self) -> PyStreamInfo {
        self.inner.info().clone().into()
    }

    #[getter]
    fn source_path(&self) -> PathBuf {
        self.inner.source_path().to_path_buf()
    }

    fn open_packets(&self, py: Python<'_>) -> PyResult<PyPacketReader> {
        py.detach(|| self.inner.open_packets())
            .map(|reader| PyPacketReader {
                reader: Mutex::new(reader),
            })
            .map_err(to_py_error)
    }

    fn open_video(&self, py: Python<'_>) -> PyResult<PyVideoFrameReader> {
        py.detach(|| self.inner.open_video())
            .map(|reader| PyVideoFrameReader {
                reader: Mutex::new(reader),
            })
            .map_err(to_py_error)
    }
}

#[pyclass(
    name = "StreamSideData",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyStreamSideData {
    #[pyo3(get)]
    kind: i32,
    data: Vec<u8>,
}

#[pymethods]
impl PyStreamSideData {
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.data)
    }
}

impl From<CoreStreamSideData> for PyStreamSideData {
    fn from(value: CoreStreamSideData) -> Self {
        Self {
            kind: value.kind,
            data: value.data,
        }
    }
}

#[pyclass(
    name = "EncodedPacket",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
struct PyEncodedPacket {
    #[pyo3(get)]
    input_index: usize,
    #[pyo3(get)]
    stream_index: usize,
    data: Vec<u8>,
    #[pyo3(get)]
    pts: Option<i64>,
    #[pyo3(get)]
    dts: Option<i64>,
    #[pyo3(get)]
    duration: i64,
    #[pyo3(get)]
    time_base: (i32, i32),
    #[pyo3(get)]
    flags: i32,
    #[pyo3(get)]
    key_frame: bool,
    #[pyo3(get)]
    corrupt: bool,
    #[pyo3(get)]
    source_position: Option<i64>,
    #[pyo3(get)]
    side_data: Vec<PyStreamSideData>,
}

#[pymethods]
impl PyEncodedPacket {
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.data)
    }
}

impl From<CoreEncodedPacket> for PyEncodedPacket {
    fn from(packet: CoreEncodedPacket) -> Self {
        Self {
            input_index: packet.input_index,
            stream_index: packet.stream_index,
            data: packet.data,
            pts: packet.pts,
            dts: packet.dts,
            duration: packet.duration,
            time_base: (packet.time_base.numerator, packet.time_base.denominator),
            flags: packet.flags,
            key_frame: packet.key_frame,
            corrupt: packet.corrupt,
            source_position: packet.source_position,
            side_data: packet.side_data.into_iter().map(Into::into).collect(),
        }
    }
}

#[pyclass(
    name = "DecodedVideoFrame",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
struct PyDecodedVideoFrame {
    data: Vec<u8>,
    #[pyo3(get)]
    width: u32,
    #[pyo3(get)]
    height: u32,
    #[pyo3(get)]
    timestamp_seconds: Option<f64>,
    #[pyo3(get)]
    pts: Option<i64>,
    #[pyo3(get)]
    time_base: (i32, i32),
}

#[pymethods]
impl PyDecodedVideoFrame {
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.data)
    }
}

impl From<CoreDecodedVideoFrame> for PyDecodedVideoFrame {
    fn from(frame: CoreDecodedVideoFrame) -> Self {
        Self {
            data: frame.data,
            width: frame.width,
            height: frame.height,
            timestamp_seconds: frame.timestamp.map(|timestamp| timestamp.as_secs_f64()),
            pts: frame.pts,
            time_base: (frame.time_base.numerator, frame.time_base.denominator),
        }
    }
}

#[pyclass(
    name = "PacketReader",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
struct PyPacketReader {
    reader: Mutex<CorePacketReader>,
}

#[pymethods]
impl PyPacketReader {
    #[getter]
    fn info(&self, py: Python<'_>) -> PyResult<PyStreamInfo> {
        with_stream_reader(py, &self.reader, |reader| Ok(reader.info().clone())).map(Into::into)
    }

    fn read_packet(&self, py: Python<'_>) -> PyResult<Option<PyEncodedPacket>> {
        with_stream_reader(py, &self.reader, CorePacketReader::read_packet)
            .map(|packet| packet.map(Into::into))
    }

    fn seek(&self, py: Python<'_>, seconds: f64) -> PyResult<()> {
        let position = duration_from_seconds(seconds, "packet seek time").map_err(to_py_error)?;
        with_stream_reader(py, &self.reader, |reader| reader.seek(position))
    }
}

#[pyclass(
    name = "VideoFrameReader",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
struct PyVideoFrameReader {
    reader: Mutex<CoreVideoFrameReader>,
}

#[pymethods]
impl PyVideoFrameReader {
    #[getter]
    fn info(&self, py: Python<'_>) -> PyResult<PyStreamInfo> {
        with_stream_reader(py, &self.reader, |reader| Ok(reader.info().clone())).map(Into::into)
    }

    fn read_frame(&self, py: Python<'_>) -> PyResult<Option<PyDecodedVideoFrame>> {
        with_stream_reader(py, &self.reader, CoreVideoFrameReader::read_frame)
            .map(|frame| frame.map(Into::into))
    }

    fn seek(&self, py: Python<'_>, seconds: f64) -> PyResult<()> {
        let position = duration_from_seconds(seconds, "video seek time").map_err(to_py_error)?;
        with_stream_reader(py, &self.reader, |reader| reader.seek(position))
    }

    fn frame_at(&self, py: Python<'_>, seconds: f64) -> PyResult<Option<PyDecodedVideoFrame>> {
        let position = duration_from_seconds(seconds, "video frame time").map_err(to_py_error)?;
        with_stream_reader(py, &self.reader, |reader| reader.frame_at(position))
            .map(|frame| frame.map(Into::into))
    }
}

fn with_stream_reader<R, T, F>(py: Python<'_>, reader: &Mutex<R>, operation: F) -> PyResult<T>
where
    R: Send,
    T: Send,
    F: FnOnce(&mut R) -> insta360_rs::Result<T> + Send,
{
    // Acquire the mutex after releasing the GIL: another Python thread may
    // already be reading, and must be free to reacquire the GIL on completion.
    py.detach(|| {
        let mut reader = reader
            .lock()
            .map_err(|_| CoreError::Media("stream reader state is unavailable".into()))?;
        operation(&mut reader)
    })
    .map_err(to_py_error)
}

#[pyclass(
    name = "ExportProgress",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyExportProgress {
    #[pyo3(get)]
    phase: PyExportPhase,
    #[pyo3(get)]
    completed: u64,
    #[pyo3(get)]
    total: Option<u64>,
    #[pyo3(get)]
    media_time_seconds: Option<f64>,
    #[pyo3(get)]
    elapsed_seconds: f64,
    #[pyo3(get)]
    estimated_remaining_seconds: Option<f64>,
}

impl From<CoreExportProgress> for PyExportProgress {
    fn from(value: CoreExportProgress) -> Self {
        Self {
            phase: value.phase.into(),
            completed: value.completed,
            total: value.total,
            media_time_seconds: value.media_time.map(|duration| duration.as_secs_f64()),
            elapsed_seconds: value.elapsed.as_secs_f64(),
            estimated_remaining_seconds: value
                .estimated_remaining
                .map(|duration| duration.as_secs_f64()),
        }
    }
}

#[pyclass(
    name = "Capabilities",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
struct PyCapabilities {
    #[pyo3(get)]
    underwater_ai_compiled: bool,
    #[pyo3(get)]
    image_export: bool,
    #[pyo3(get)]
    video_export: bool,
    #[pyo3(get)]
    gpu_compiled: bool,
    #[pyo3(get)]
    gpu_available: bool,
    #[pyo3(get)]
    gpu_adapters: Vec<PyGpuAdapterInfo>,
    #[pyo3(get)]
    gpu_unavailable_reason: Option<String>,
    #[pyo3(get)]
    hevc_encoders: Vec<String>,
}

impl From<MediaCapabilities> for PyCapabilities {
    fn from(value: MediaCapabilities) -> Self {
        Self {
            underwater_ai_compiled: value.underwater_ai_compiled,
            image_export: value.image_export,
            video_export: value.video_export,
            gpu_compiled: value.gpu_compiled,
            gpu_available: value.gpu_available,
            gpu_adapters: value.gpu_adapters.into_iter().map(Into::into).collect(),
            gpu_unavailable_reason: value.gpu_unavailable_reason,
            hevc_encoders: value.hevc_encoders,
        }
    }
}

struct JobState {
    job: Option<CoreExportJob>,
    result_claimed: bool,
    latest_progress: Option<PyExportProgress>,
    backend: Option<PyBackendReport>,
    stabilization: Option<String>,
    warnings: Vec<String>,
}

#[pyclass(name = "ExportJob", module = "insta360_rs._native")]
struct PyExportJob {
    state: Mutex<JobState>,
}

#[pymethods]
impl PyExportJob {
    fn cancel(&self, py: Python<'_>) -> PyResult<()> {
        self.with_state(py, |state| {
            if let Some(job) = &state.job {
                job.cancel();
            }
            Ok(())
        })
    }

    fn is_finished(&self, py: Python<'_>) -> PyResult<bool> {
        self.with_state(py, |state| {
            Ok(state.job.as_ref().is_none_or(CoreExportJob::is_finished))
        })
    }

    fn progress(&self, py: Python<'_>) -> PyResult<Option<PyExportProgress>> {
        self.with_state(py, |state| {
            Self::drain_events(state);
            Ok(state.latest_progress.clone())
        })
    }

    fn backend(&self, py: Python<'_>) -> PyResult<Option<PyBackendReport>> {
        self.with_state(py, |state| {
            Self::drain_events(state);
            Ok(state.backend.clone())
        })
    }

    /// Selected file motion profile, timing map, and readout status, once prepared.
    fn stabilization(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_state(py, |state| {
            Self::drain_events(state);
            Ok(state.stabilization.clone())
        })
    }

    fn take_warnings(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        self.with_state(py, |state| {
            Self::drain_events(state);
            Ok(std::mem::take(&mut state.warnings))
        })
    }

    fn wait(&self, py: Python<'_>) -> PyResult<PyExportResult> {
        py.detach(|| {
            {
                let mut state = self.lock_state()?;
                if state.result_claimed {
                    return Err(PyRuntimeError::new_err(
                        "this export job's result has already been consumed",
                    ));
                }
                state.result_claimed = true;
            }

            loop {
                let finished_job = {
                    let mut state = self.lock_state()?;
                    let finished = state.job.as_ref().is_some_and(CoreExportJob::is_finished);
                    // Check completion before draining so the final events are
                    // retained even when the worker finishes during a poll.
                    Self::drain_events(&mut state);
                    if finished {
                        state.job.take()
                    } else {
                        None
                    }
                };
                if let Some(job) = finished_job {
                    let result = job.wait().map_err(to_py_error)?;
                    let mut state = self.lock_state()?;
                    state.backend = Some(result.backend.clone().into());
                    // A completed export can fill the bounded event channel
                    // before Python polls it. Recover its final snapshot from
                    // the authoritative result when that event was dropped.
                    if state.latest_progress.as_ref().is_none_or(|progress| {
                        progress.phase != PyExportPhase::Finalizing
                            || progress.completed != result.frames_written
                    }) {
                        state.latest_progress = Some(PyExportProgress {
                            phase: PyExportPhase::Finalizing,
                            completed: result.frames_written,
                            total: Some(result.frames_written),
                            media_time_seconds: None,
                            elapsed_seconds: result.elapsed.as_secs_f64(),
                            estimated_remaining_seconds: None,
                        });
                    }
                    return Ok(result.into());
                }
                // Keep the job available to cancellation and polling threads
                // while waiting, without holding its mutex or the GIL.
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match self.is_finished(py) {
            Ok(finished) => format!("ExportJob(finished={finished})"),
            Err(_) => "ExportJob(state=<unavailable>)".to_owned(),
        }
    }
}

impl PyExportJob {
    fn new(job: CoreExportJob) -> Self {
        Self {
            state: Mutex::new(JobState {
                job: Some(job),
                result_claimed: false,
                latest_progress: None,
                backend: None,
                stabilization: None,
                warnings: Vec::new(),
            }),
        }
    }

    fn lock_state(&self) -> PyResult<std::sync::MutexGuard<'_, JobState>> {
        self.state
            .lock()
            .map_err(|_| PyRuntimeError::new_err("export job state is unavailable"))
    }

    fn with_state<T: Send>(
        &self,
        py: Python<'_>,
        operation: impl FnOnce(&mut JobState) -> PyResult<T> + Send,
    ) -> PyResult<T> {
        py.detach(|| {
            let mut state = self.lock_state()?;
            operation(&mut state)
        })
    }

    fn drain_events(state: &mut JobState) {
        let events = state
            .job
            .as_ref()
            .map(|job| std::iter::from_fn(|| job.try_event()).collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            match event {
                ExportEvent::Progress(progress) => {
                    state.latest_progress = Some(progress.into());
                }
                ExportEvent::BackendSelected(report) => {
                    state.backend = Some((*report).into());
                }
                ExportEvent::StabilizationPrepared(description) => {
                    state.stabilization = Some(description);
                }
                ExportEvent::Warning(warning) => state.warnings.push(warning),
                _ => {}
            }
        }
    }
}

#[pyfunction(name = "_probe")]
fn py_probe(py: Python<'_>, inputs: Vec<PathBuf>) -> PyResult<PyMediaInfo> {
    py.detach(move || {
        let inputs = make_input_set(inputs)?;
        probe_media(&inputs)
    })
    .map(PyMediaInfo::from)
    .map_err(to_py_error)
}

#[pyfunction(name = "_extract")]
fn py_extract(
    py: Python<'_>,
    inputs: Vec<PathBuf>,
    output_dir: PathBuf,
) -> PyResult<PyExtractionReport> {
    py.detach(move || {
        let inputs = make_input_set(inputs)?;
        insta360_rs::extract(&inputs, output_dir)
    })
    .map(PyExtractionReport::from)
    .map_err(to_py_error)
}

#[pyfunction(name = "_open_media")]
fn py_open_media(py: Python<'_>, inputs: Vec<PathBuf>) -> PyResult<PyMediaSource> {
    py.detach(move || CoreMediaSource::open(make_input_set(inputs)?))
        .map(|inner| PyMediaSource { inner })
        .map_err(to_py_error)
}

#[allow(clippy::too_many_arguments)]
#[pyfunction(name = "_export_video")]
fn py_export_video(
    py: Python<'_>,
    inputs: Vec<PathBuf>,
    output: PathBuf,
    config: Option<PyStitchConfig>,
    quality: u8,
    audio: PyAudioPolicy,
    start: Option<f64>,
    duration: Option<f64>,
    acceleration: Option<PyMediaAcceleration>,
) -> PyResult<PyExportResult> {
    let job = start_video_job(
        inputs,
        output,
        config,
        quality,
        audio,
        start,
        duration,
        acceleration,
    )?;
    py.detach(move || job.wait())
        .map(PyExportResult::from)
        .map_err(to_py_error)
}

#[allow(clippy::too_many_arguments)]
#[pyfunction(name = "_start_export_video")]
fn py_start_export_video(
    inputs: Vec<PathBuf>,
    output: PathBuf,
    config: Option<PyStitchConfig>,
    quality: u8,
    audio: PyAudioPolicy,
    start: Option<f64>,
    duration: Option<f64>,
    acceleration: Option<PyMediaAcceleration>,
) -> PyResult<PyExportJob> {
    start_video_job(
        inputs,
        output,
        config,
        quality,
        audio,
        start,
        duration,
        acceleration,
    )
    .map(PyExportJob::new)
}

#[allow(clippy::too_many_arguments)]
#[pyfunction(name = "_export_frames")]
fn py_export_frames(
    py: Python<'_>,
    inputs: Vec<PathBuf>,
    output_dir: PathBuf,
    config: Option<PyStitchConfig>,
    indices: Option<Vec<u64>>,
    timestamps: Option<Vec<f64>>,
    start: Option<f64>,
    end: Option<f64>,
    frames_per_second: Option<f64>,
    image_format: PyImageFormat,
    quality: u8,
    scale_width: Option<u32>,
) -> PyResult<PyExportResult> {
    let request = FrameExportRequest {
        inputs,
        output_dir,
        config,
        indices,
        timestamps,
        start,
        end,
        frames_per_second,
        image_format,
        quality,
        scale_width,
    };
    let job = start_frames_job(request)?;
    py.detach(move || job.wait())
        .map(PyExportResult::from)
        .map_err(to_py_error)
}

#[allow(clippy::too_many_arguments)]
#[pyfunction(name = "_start_export_frames")]
fn py_start_export_frames(
    inputs: Vec<PathBuf>,
    output_dir: PathBuf,
    config: Option<PyStitchConfig>,
    indices: Option<Vec<u64>>,
    timestamps: Option<Vec<f64>>,
    start: Option<f64>,
    end: Option<f64>,
    frames_per_second: Option<f64>,
    image_format: PyImageFormat,
    quality: u8,
    scale_width: Option<u32>,
) -> PyResult<PyExportJob> {
    start_frames_job(FrameExportRequest {
        inputs,
        output_dir,
        config,
        indices,
        timestamps,
        start,
        end,
        frames_per_second,
        image_format,
        quality,
        scale_width,
    })
    .map(PyExportJob::new)
}

#[pyfunction]
fn capabilities(py: Python<'_>) -> PyCapabilities {
    py.detach(MediaCapabilities::detect).into()
}

#[pyfunction]
fn mnn_runtime_version() -> PyResult<&'static str> {
    insta360_rs::underwater::mnn_runtime_version().map_err(to_py_error)
}

#[allow(clippy::too_many_arguments)]
fn start_video_job(
    inputs: Vec<PathBuf>,
    output: PathBuf,
    config: Option<PyStitchConfig>,
    quality: u8,
    audio: PyAudioPolicy,
    start: Option<f64>,
    duration: Option<f64>,
    acceleration: Option<PyMediaAcceleration>,
) -> PyResult<CoreExportJob> {
    validate_quality(quality)?;
    let (start, duration) = make_video_interval(start, duration).map_err(to_py_error)?;
    let exporter = make_exporter(inputs, config)?;
    Ok(exporter.export_video(
        output,
        VideoExportOptions {
            quality,
            audio: audio.into(),
            acceleration: core_media_acceleration(acceleration),
            projection: None,
            start,
            duration,
        },
    ))
}

fn core_media_acceleration(acceleration: Option<PyMediaAcceleration>) -> CoreMediaAcceleration {
    acceleration.unwrap_or(PyMediaAcceleration::Auto).into()
}

fn make_video_interval(
    start: Option<f64>,
    duration: Option<f64>,
) -> insta360_rs::Result<(Option<Duration>, Option<Duration>)> {
    let start = start
        .map(|seconds| duration_from_seconds(seconds, "video start"))
        .transpose()?;
    let duration = duration
        .map(|seconds| duration_from_seconds(seconds, "video duration"))
        .transpose()?;
    if duration.is_some_and(|duration| duration.is_zero()) {
        return Err(CoreError::InvalidMedia(
            "video duration must be greater than zero".into(),
        ));
    }
    Ok((start, duration))
}

struct FrameExportRequest {
    inputs: Vec<PathBuf>,
    output_dir: PathBuf,
    config: Option<PyStitchConfig>,
    indices: Option<Vec<u64>>,
    timestamps: Option<Vec<f64>>,
    start: Option<f64>,
    end: Option<f64>,
    frames_per_second: Option<f64>,
    image_format: PyImageFormat,
    quality: u8,
    scale_width: Option<u32>,
}

fn start_frames_job(request: FrameExportRequest) -> PyResult<CoreExportJob> {
    validate_quality(request.quality)?;
    if request.scale_width == Some(0) {
        return Err(to_py_error(CoreError::InvalidMedia(
            "image scale width must be greater than zero".into(),
        )));
    }
    let selection = make_frame_selection(
        request.indices,
        request.timestamps,
        request.start,
        request.end,
        request.frames_per_second,
    )
    .map_err(to_py_error)?;
    let exporter = make_exporter(request.inputs, request.config)?;
    Ok(exporter.export_frames(
        request.output_dir,
        selection,
        ImageExportOptions {
            format: request.image_format.into(),
            quality: request.quality,
            scale_width: request.scale_width,
        },
    ))
}

fn make_exporter(inputs: Vec<PathBuf>, config: Option<PyStitchConfig>) -> PyResult<Exporter> {
    let inputs = make_input_set(inputs).map_err(to_py_error)?;
    let config = config
        .unwrap_or_else(default_py_config)
        .to_core()
        .map_err(to_py_error)?;
    Exporter::new(inputs, config).map_err(to_py_error)
}

fn default_py_config() -> PyStitchConfig {
    PyStitchConfig {
        housing: PyHousing::Auto,
        environment: PyEnvironment::Auto,
        lens_accessory: PyLensAccessory::Auto,
        mounting_accessory: PyMountingAccessory::Auto,
        underwater_color: PyUnderwaterColorOptions::default(),
        stabilization: PyStabilization::DirectionLock,
        rolling_shutter: PyRollingShutterCorrection::Auto,
        backend: PyProcessingBackend::Auto,
        color_conversion: PyColorConversion::Auto,
        width: None,
        height: None,
    }
}

fn make_input_set(paths: Vec<PathBuf>) -> insta360_rs::Result<InputSet> {
    if paths.len() == 1 {
        InputSet::discover(&paths[0])
    } else {
        InputSet::new(paths)
    }
}

fn make_frame_selection(
    indices: Option<Vec<u64>>,
    timestamps: Option<Vec<f64>>,
    start: Option<f64>,
    end: Option<f64>,
    frames_per_second: Option<f64>,
) -> insta360_rs::Result<FrameSelection> {
    let has_range = start.is_some() || end.is_some() || frames_per_second.is_some();
    let modes =
        usize::from(indices.is_some()) + usize::from(timestamps.is_some()) + usize::from(has_range);
    if modes != 1 {
        return Err(CoreError::InvalidMedia(
            "choose exactly one frame selection: indices, timestamps, or start/end/fps".into(),
        ));
    }

    if let Some(indices) = indices {
        if indices.is_empty() {
            return Err(CoreError::InvalidMedia(
                "frame indices must not be empty".into(),
            ));
        }
        return Ok(FrameSelection::Indices(indices));
    }

    if let Some(timestamps) = timestamps {
        if timestamps.is_empty() {
            return Err(CoreError::InvalidMedia(
                "frame timestamps must not be empty".into(),
            ));
        }
        let timestamps = timestamps
            .into_iter()
            .map(|seconds| duration_from_seconds(seconds, "frame timestamp"))
            .collect::<insta360_rs::Result<Vec<_>>>()?;
        return Ok(FrameSelection::Timestamps(timestamps));
    }

    let (start, end, frames_per_second) = match (start, end, frames_per_second) {
        (Some(start), Some(end), Some(fps)) => (start, end, fps),
        _ => {
            return Err(CoreError::InvalidMedia(
                "sampled frame selection requires start, end, and fps".into(),
            ));
        }
    };
    if end <= start {
        return Err(CoreError::InvalidMedia(
            "sampled frame selection end must be greater than start".into(),
        ));
    }
    if !frames_per_second.is_finite() || frames_per_second <= 0.0 {
        return Err(CoreError::InvalidMedia(
            "sampled frame selection fps must be positive and finite".into(),
        ));
    }
    let frames_per_second_milli = (frames_per_second * 1_000.0).round();
    if !(1.0..=f64::from(u32::MAX)).contains(&frames_per_second_milli) {
        return Err(CoreError::InvalidMedia(
            "sampled frame selection fps is outside the supported range".into(),
        ));
    }
    Ok(FrameSelection::SampledRange {
        start: duration_from_seconds(start, "sample range start")?,
        end: duration_from_seconds(end, "sample range end")?,
        frames_per_second_milli: frames_per_second_milli as u32,
    })
}

fn duration_from_seconds(seconds: f64, label: &str) -> insta360_rs::Result<Duration> {
    Duration::try_from_secs_f64(seconds).map_err(|_| {
        CoreError::InvalidMedia(format!(
            "{label} must be non-negative, finite, and representable"
        ))
    })
}

fn validate_quality(quality: u8) -> PyResult<()> {
    if !(1..=100).contains(&quality) {
        return Err(to_py_error(CoreError::InvalidMedia(
            "quality must be between 1 and 100".into(),
        )));
    }
    Ok(())
}

fn to_py_error(error: CoreError) -> PyErr {
    let message = error.to_string();
    match error {
        CoreError::Io { .. } => Insta360IOError::new_err(message),
        CoreError::InvalidMedia(_) => InvalidMediaError::new_err(message),
        CoreError::UnsupportedCamera(_) => UnsupportedCameraError::new_err(message),
        CoreError::MissingCalibration(_) => MissingCalibrationError::new_err(message),
        CoreError::ConflictingOptics { .. } => ConflictingOpticsError::new_err(message),
        CoreError::AmbiguousOpticalSetup { .. } => AmbiguousOpticalSetupError::new_err(message),
        CoreError::MissingCapability(_) => MissingCapabilityError::new_err(message),
        CoreError::GpuUnavailable(_) => GpuUnavailableError::new_err(message),
        CoreError::GpuProcessing(_) => GpuProcessingError::new_err(message),
        CoreError::Cancelled => CancelledError::new_err(message),
        CoreError::Media(_) => MediaProcessingError::new_err(message),
        _ => Insta360Error::new_err(message),
    }
}

/// Native implementation for the `insta360_rs` Python package.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add(
        "ConflictingOpticsError",
        module.py().get_type::<ConflictingOpticsError>(),
    )?;
    module.add("Insta360Error", module.py().get_type::<Insta360Error>())?;
    module.add("Insta360IOError", module.py().get_type::<Insta360IOError>())?;
    module.add(
        "InvalidMediaError",
        module.py().get_type::<InvalidMediaError>(),
    )?;
    module.add(
        "UnsupportedCameraError",
        module.py().get_type::<UnsupportedCameraError>(),
    )?;
    module.add(
        "MissingCalibrationError",
        module.py().get_type::<MissingCalibrationError>(),
    )?;
    module.add(
        "AmbiguousOpticalSetupError",
        module.py().get_type::<AmbiguousOpticalSetupError>(),
    )?;
    module.add(
        "MissingCapabilityError",
        module.py().get_type::<MissingCapabilityError>(),
    )?;
    module.add(
        "GpuUnavailableError",
        module.py().get_type::<GpuUnavailableError>(),
    )?;
    module.add("CancelledError", module.py().get_type::<CancelledError>())?;
    module.add(
        "MediaProcessingError",
        module.py().get_type::<MediaProcessingError>(),
    )?;
    module.add(
        "GpuProcessingError",
        module.py().get_type::<GpuProcessingError>(),
    )?;

    optical_config::register(module)?;
    module.add_class::<PyStabilization>()?;
    module.add_class::<PyRollingShutterCorrection>()?;
    module.add_class::<PyProcessingBackend>()?;
    module.add_class::<PyColorConversion>()?;
    module.add_class::<PyEffectiveBackend>()?;
    module.add_class::<PyImageFormat>()?;
    module.add_class::<PyAudioPolicy>()?;
    module.add_class::<PyMediaAcceleration>()?;
    module.add_class::<PyExportPhase>()?;
    module.add_class::<PyStitchConfig>()?;
    module.add_class::<PyVideoTrackInfo>()?;
    module.add_class::<PyTrailerInfo>()?;
    module.add_class::<PyMediaInfo>()?;
    module.add_class::<PyGpuAdapterInfo>()?;
    module.add_class::<PyGpuFailure>()?;
    module.add_class::<PyBackendReport>()?;
    module.add_class::<PyExportResult>()?;
    module.add_class::<PyExtractionReport>()?;
    module.add_class::<PyStreamInfo>()?;
    module.add_class::<PyMediaSource>()?;
    module.add_class::<PyMediaStream>()?;
    module.add_class::<PyStreamSideData>()?;
    module.add_class::<PyEncodedPacket>()?;
    module.add_class::<PyDecodedVideoFrame>()?;
    module.add_class::<PyPacketReader>()?;
    module.add_class::<PyVideoFrameReader>()?;
    module.add_class::<PyExportProgress>()?;
    module.add_class::<PyCapabilities>()?;
    module.add_class::<PyExportJob>()?;

    module.add_function(wrap_pyfunction!(py_probe, module)?)?;
    module.add_function(wrap_pyfunction!(py_extract, module)?)?;
    module.add_function(wrap_pyfunction!(py_open_media, module)?)?;
    module.add_function(wrap_pyfunction!(py_export_video, module)?)?;
    module.add_function(wrap_pyfunction!(py_start_export_video, module)?)?;
    module.add_function(wrap_pyfunction!(py_export_frames, module)?)?;
    module.add_function(wrap_pyfunction!(py_start_export_frames, module)?)?;
    module.add_function(wrap_pyfunction!(capabilities, module)?)?;
    module.add_function(wrap_pyfunction!(mnn_runtime_version, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_conversion_defaults_and_explicit_choices_reach_core() {
        assert_eq!(
            default_py_config().to_core().unwrap().color_conversion,
            CoreColorConversion::Auto
        );
        for (choice, expected) in [
            (PyColorConversion::Auto, CoreColorConversion::Auto),
            (PyColorConversion::Preserve, CoreColorConversion::Preserve),
            (
                PyColorConversion::ILogToRec709,
                CoreColorConversion::ILogToRec709,
            ),
        ] {
            let config = PyStitchConfig {
                color_conversion: choice,
                ..default_py_config()
            };
            assert_eq!(config.to_core().unwrap().color_conversion, expected);
            assert!(config
                .__repr__()
                .contains(&format!("color_conversion={choice:?}")));
        }
    }

    #[test]
    fn frame_selection_requires_exactly_one_mode() {
        assert!(make_frame_selection(None, None, None, None, None).is_err());
        assert!(make_frame_selection(Some(vec![1]), Some(vec![1.0]), None, None, None).is_err());
    }

    #[test]
    fn sampled_selection_uses_milliframes_per_second() {
        let selection = make_frame_selection(None, None, Some(1.0), Some(2.0), Some(1.5))
            .expect("valid selection");
        assert_eq!(
            selection,
            FrameSelection::SampledRange {
                start: Duration::from_secs(1),
                end: Duration::from_secs(2),
                frames_per_second_milli: 1_500,
            }
        );
    }

    #[test]
    fn video_interval_converts_seconds_and_rejects_zero_duration() {
        assert_eq!(
            make_video_interval(Some(2.5), Some(60.0)).expect("valid video interval"),
            (
                Some(Duration::from_millis(2_500)),
                Some(Duration::from_secs(60))
            )
        );
        assert!(make_video_interval(None, Some(0.0)).is_err());
    }

    #[test]
    fn media_acceleration_maps_to_core_policy() {
        assert_eq!(core_media_acceleration(None), CoreMediaAcceleration::Auto);
        assert_eq!(
            core_media_acceleration(Some(PyMediaAcceleration::Software)),
            CoreMediaAcceleration::Software
        );
        assert_eq!(
            core_media_acceleration(Some(PyMediaAcceleration::Hardware)),
            CoreMediaAcceleration::Hardware
        );
    }
}
