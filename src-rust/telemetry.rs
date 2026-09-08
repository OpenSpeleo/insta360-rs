//! Timed sensor records embedded in INSV trailers.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::container::InsvMetadata;
use crate::{Error, Result};

const EXPOSURE_SAMPLE_SIZE: usize = 16;
const MAX_EXPOSURE_SAMPLES: usize = 10_000_000;

/// Exposure used by the camera at a media-relative timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExposureSample {
    pub timestamp: Duration,
    pub shutter_speed: Duration,
}

/// Exposure on the camera clock, before first-frame or video-time mapping.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraExposureSample {
    pub timestamp_micros: i64,
    pub shutter_speed: Duration,
}

/// Decodes exposure samples without discarding the measurements before video start.
///
/// Raw recordings store microseconds; legacy recordings store milliseconds.
/// Keeping this clock separate is necessary for exposure-file PTS mapping and
/// shutter-midpoint or rolling-shutter queries preceding the first video frame.
pub fn decode_camera_exposure_record(
    data: &[u8],
    metadata: &InsvMetadata,
) -> Result<Vec<CameraExposureSample>> {
    if !data.len().is_multiple_of(EXPOSURE_SAMPLE_SIZE) {
        return Err(Error::InvalidMedia(
            "exposure record has a truncated sample".into(),
        ));
    }
    let count = data.len() / EXPOSURE_SAMPLE_SIZE;
    if count > MAX_EXPOSURE_SAMPLES {
        return Err(Error::InvalidMedia(
            "exposure record exceeds the sample limit".into(),
        ));
    }
    let multiplier = match metadata.is_raw_gyro {
        Some(true) => 1_i64,
        Some(false) => 1_000_i64,
        None => {
            return Err(Error::InvalidMedia(
                "metadata does not declare the exposure clock unit".into(),
            ))
        }
    };
    let mut samples = Vec::with_capacity(count);
    for chunk in data.chunks_exact(EXPOSURE_SAMPLE_SIZE) {
        let timestamp = i64::from_le_bytes(chunk[..8].try_into().expect("timestamp width"));
        let timestamp_micros = timestamp.checked_mul(multiplier).ok_or_else(|| {
            Error::InvalidMedia("exposure camera timestamp overflows microseconds".into())
        })?;
        let shutter_seconds = f64::from_le_bytes(chunk[8..16].try_into().expect("shutter width"));
        let shutter_speed = Duration::try_from_secs_f64(shutter_seconds).map_err(|_| {
            Error::InvalidMedia("exposure record contains an invalid shutter speed".into())
        })?;
        if samples.last().is_some_and(|sample: &CameraExposureSample| {
            sample.timestamp_micros >= timestamp_micros
        }) {
            return Err(Error::InvalidMedia(
                "exposure camera timestamps must be strictly increasing".into(),
            ));
        }
        samples.push(CameraExposureSample {
            timestamp_micros,
            shutter_speed,
        });
    }
    Ok(samples)
}

/// Decodes INSV exposure records (`4` and, when present, `12`).
///
/// X5 raw-gyro recordings use microsecond camera timestamps for both IMU and
/// exposure records. Legacy recordings use milliseconds. Samples captured
/// before the first video frame are omitted.
pub fn decode_exposure_record(data: &[u8], metadata: &InsvMetadata) -> Result<Vec<ExposureSample>> {
    if !data.len().is_multiple_of(EXPOSURE_SAMPLE_SIZE) {
        return Err(Error::InvalidMedia(format!(
            "exposure record size {} is not divisible by {EXPOSURE_SAMPLE_SIZE}",
            data.len()
        )));
    }
    let count = data.len() / EXPOSURE_SAMPLE_SIZE;
    if count > MAX_EXPOSURE_SAMPLES {
        return Err(Error::InvalidMedia(format!(
            "exposure record contains {count} samples, exceeding the {MAX_EXPOSURE_SAMPLES} sample limit"
        )));
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    let timestamp_multiplier = if metadata.is_raw_gyro == Some(true) {
        1_i128
    } else {
        1_000_i128
    };
    let first_timestamp = metadata.first_frame_timestamp.unwrap_or_else(|| {
        i64::from_le_bytes(data[..8].try_into().expect("timestamp is eight bytes"))
    });
    let first_timestamp_us = i128::from(first_timestamp) * timestamp_multiplier;
    let mut samples = Vec::with_capacity(count);

    for chunk in data.chunks_exact(EXPOSURE_SAMPLE_SIZE) {
        let camera_timestamp = i64::from_le_bytes(
            chunk[..8]
                .try_into()
                .expect("exposure timestamp is eight bytes"),
        );
        let timestamp_us = i128::from(camera_timestamp) * timestamp_multiplier - first_timestamp_us;
        if timestamp_us < 0 {
            continue;
        }
        let timestamp_us = u64::try_from(timestamp_us).map_err(|_| {
            Error::InvalidMedia("media-relative exposure timestamp overflows Duration".into())
        })?;
        let shutter_seconds = f64::from_le_bytes(
            chunk[8..16]
                .try_into()
                .expect("shutter speed is eight bytes"),
        );
        if !shutter_seconds.is_finite() || shutter_seconds < 0.0 {
            return Err(Error::InvalidMedia(
                "exposure record contains an invalid shutter speed".into(),
            ));
        }
        samples.push(ExposureSample {
            timestamp: Duration::from_micros(timestamp_us),
            shutter_speed: Duration::try_from_secs_f64(shutter_seconds)
                .map_err(|_| Error::InvalidMedia("shutter speed overflows Duration".into()))?,
        });
    }
    for pair in samples.windows(2) {
        if pair[1].timestamp <= pair[0].timestamp {
            return Err(Error::InvalidMedia(
                "exposure record timestamps must be strictly increasing".into(),
            ));
        }
    }
    Ok(samples)
}
