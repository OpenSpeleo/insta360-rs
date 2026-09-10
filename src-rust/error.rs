use std::path::PathBuf;

/// Errors returned by `insta360-rs`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("I/O error while accessing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid Insta360 media: {0}")]
    InvalidMedia(String),
    #[error("unsupported camera model: {0}")]
    UnsupportedCamera(String),
    #[error("recording does not contain a usable calibration: {0}")]
    MissingCalibration(String),
    #[error("optical setup is ambiguous; choose one of: {candidates:?}")]
    AmbiguousOpticalSetup { candidates: Vec<String> },
    #[error("conflicting optical selection {selection:?}: {reason}")]
    ConflictingOptics {
        selection: crate::OpticalSelection,
        reason: String,
    },
    #[error("requested capability is unavailable: {0}")]
    MissingCapability(String),
    #[error("GPU capability is unavailable: {0}")]
    GpuUnavailable(Box<crate::GpuFailure>),
    #[error("GPU processing failed: {0}")]
    GpuProcessing(Box<crate::GpuFailure>),
    #[error("operation was cancelled")]
    Cancelled,
    #[error("media processing failed: {0}")]
    Media(String),
    #[error("asset loading failed: {0}")]
    Asset(#[from] crate::assets::AssetError),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
