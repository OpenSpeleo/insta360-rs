//! Mapping actual video presentation times onto the camera exposure clock.

use std::time::Duration;

use crate::telemetry::CameraExposureSample;
use crate::{Error, Result};

const MAX_EXACT_CAMERA_MICROS: i64 = 1_i64 << 53;

pub(crate) fn validate_camera_timestamp(timestamp_micros: i64) -> Result<()> {
    if !(-MAX_EXACT_CAMERA_MICROS..=MAX_EXACT_CAMERA_MICROS).contains(&timestamp_micros) {
        return Err(Error::InvalidMedia(
            "camera timestamp exceeds the exact microsecond range of floating-point timing".into(),
        ));
    }
    Ok(())
}

/// Frame exposure times paired with actual presentation timestamps.
///
/// The first video frame corresponds to the exposure nearest the recorded
/// first-frame camera timestamp (the earlier entry wins ties). Subsequent
/// exposures follow presentation order. Extra exposure records after the last
/// video frame are allowed. No nominal frame rate is used.
/// Camera timestamps and the first-frame anchor must be within ±2^53
/// microseconds, where every native integer microsecond is exactly representable.
#[derive(Clone, Debug)]
pub struct ExposureTimeline {
    presentation_micros: Vec<i64>,
    exposures: Vec<CameraExposureSample>,
}

impl ExposureTimeline {
    pub fn new(
        samples: &[CameraExposureSample],
        first_frame_timestamp_micros: i64,
        presentation_timestamps_micros: &[i64],
    ) -> Result<Self> {
        validate_camera_timestamp(first_frame_timestamp_micros)?;
        for sample in samples {
            validate_camera_timestamp(sample.timestamp_micros)?;
        }
        if samples.is_empty() || presentation_timestamps_micros.is_empty() {
            return Err(Error::InvalidMedia(
                "exposure mapping requires exposure samples and video timestamps".into(),
            ));
        }
        if samples
            .windows(2)
            .any(|pair| pair[0].timestamp_micros >= pair[1].timestamp_micros)
            || presentation_timestamps_micros
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(Error::InvalidMedia(
                "exposure mapping timestamps must be strictly increasing".into(),
            ));
        }
        let insertion = samples
            .partition_point(|sample| sample.timestamp_micros < first_frame_timestamp_micros);
        let anchor = if insertion == 0 {
            0
        } else if insertion == samples.len()
            || samples[insertion - 1]
                .timestamp_micros
                .abs_diff(first_frame_timestamp_micros)
                <= samples[insertion]
                    .timestamp_micros
                    .abs_diff(first_frame_timestamp_micros)
        {
            insertion - 1
        } else {
            insertion
        };
        let end = anchor
            .checked_add(presentation_timestamps_micros.len())
            .ok_or_else(|| Error::InvalidMedia("exposure mapping sample count overflows".into()))?;
        let exposures = samples.get(anchor..end).ok_or_else(|| {
            Error::InvalidMedia("exposure record ends before the final video frame".into())
        })?;
        Ok(Self {
            presentation_micros: presentation_timestamps_micros.to_vec(),
            exposures: exposures.to_vec(),
        })
    }

    /// Returns the camera timestamp corresponding to a video PTS, in microseconds.
    ///
    /// Between actual frame timestamps this linearly interpolates the camera
    /// clock. Queries outside the recorded video interval fail. Shutter-center,
    /// readout, and gyro timing adjustments are deliberately not applied here.
    /// Fractional microseconds are rounded to the precision of the returned
    /// absolute `f64`; exact frame timestamps preserve integer microseconds.
    pub fn timestamp_micros_at_pts(&self, pts_micros: i64) -> Result<f64> {
        self.timestamp_micros_from_origin_at_pts(pts_micros, 0)
    }

    /// Rebase before floating-point arithmetic so large camera epochs cannot
    /// erase shutter-midpoint or readout fractions on the file's shared clock.
    pub(crate) fn timestamp_micros_from_origin_at_pts(
        &self,
        pts_micros: i64,
        origin_micros: i64,
    ) -> Result<f64> {
        let left = self.preceding_frame(pts_micros)?;
        let left_pts = self.presentation_micros[left];
        let left_camera = self.exposures[left].timestamp_micros;
        let relative_camera = (i128::from(left_camera) - i128::from(origin_micros)) as f64;
        if pts_micros == left_pts {
            return Ok(relative_camera);
        }
        let right = left + 1;
        let fraction = (i128::from(pts_micros) - i128::from(left_pts)) as f64
            / (i128::from(self.presentation_micros[right]) - i128::from(left_pts)) as f64;
        let camera_delta =
            i128::from(self.exposures[right].timestamp_micros) - i128::from(left_camera);
        Ok(relative_camera + fraction * camera_delta as f64)
    }

    /// Returns the frame's shutter duration; between frames the preceding value applies.
    pub fn shutter_speed_at_pts(&self, pts_micros: i64) -> Result<Duration> {
        Ok(self.exposures[self.preceding_frame(pts_micros)?].shutter_speed)
    }

    #[cfg(feature = "media")]
    pub(crate) fn minimum_frame_interval_micros(&self) -> Option<u64> {
        self.exposures
            .windows(2)
            .map(|pair| pair[1].timestamp_micros.abs_diff(pair[0].timestamp_micros))
            .min()
    }

    fn preceding_frame(&self, pts_micros: i64) -> Result<usize> {
        if pts_micros < self.presentation_micros[0]
            || pts_micros > self.presentation_micros[self.presentation_micros.len() - 1]
        {
            return Err(Error::InvalidMedia(
                "video timestamp is outside the exposure mapping".into(),
            ));
        }
        Ok(self
            .presentation_micros
            .partition_point(|timestamp| *timestamp <= pts_micros)
            - 1)
    }
}
