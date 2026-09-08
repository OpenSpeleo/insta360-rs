//! Per-frame motion, expressed in the stitcher's Z-up camera-body coordinates.

use super::Orientation;
use crate::{Error, Result};

/// Maximum uniformly spaced poses per lens (also the GPU storage capacity).
pub const MAX_READOUT_POSES: usize = 129;

/// Sensor scan direction expressed in the decoded source image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ReadoutDirection {
    TopToBottom = 0,
    BottomToTop = 1,
    LeftToRight = 2,
    RightToLeft = 3,
}

/// Rotations from the reference camera body to the body at each sensor time.
///
/// For a body-to-world pose `Q(t)`, each entry is `Q(t).inverse() * Q(reference)`.
/// Entries span the *full sensor* readout at uniform time intervals. The crop
/// interval identifies the part visible in the decoded image, before reversing
/// the scan direction. Pixel centers use `(coordinate + 0.5) / image_dimension`.
#[derive(Clone, Debug)]
pub struct ReadoutPoseTable {
    direction: ReadoutDirection,
    sensor_fraction: [f64; 2],
    poses: Vec<Orientation>,
}

impl ReadoutPoseTable {
    /// Creates a bounded table with an increasing sensor crop interval in [0, 1].
    pub fn new(
        direction: ReadoutDirection,
        sensor_fraction: [f64; 2],
        poses: Vec<Orientation>,
    ) -> Result<Self> {
        if !(2..=MAX_READOUT_POSES).contains(&poses.len()) {
            return Err(Error::InvalidMedia(format!(
                "sensor readout requires 2..={MAX_READOUT_POSES} poses"
            )));
        }
        if !sensor_fraction.iter().all(|v| v.is_finite())
            || sensor_fraction[0] < 0.0
            || sensor_fraction[1] > 1.0
            || sensor_fraction[0] >= sensor_fraction[1]
        {
            return Err(Error::InvalidMedia(
                "invalid sensor readout crop interval".into(),
            ));
        }
        for pose in &poses {
            pose.validate()?;
        }
        // Uniform sampling must resolve angular winding before shortest-arc SLERP.
        for pair in poses.windows(2) {
            let a = pair[0];
            let b = pair[1];
            let dot = (a.w * b.w + a.x * b.x + a.y * b.y + a.z * b.z).abs();
            if dot < std::f64::consts::FRAC_1_SQRT_2 {
                return Err(Error::InvalidMedia(
                    "sensor readout pose interval exceeds 90 degrees".into(),
                ));
            }
        }
        Ok(Self {
            direction,
            sensor_fraction,
            poses,
        })
    }

    pub fn direction(&self) -> ReadoutDirection {
        self.direction
    }
    pub fn sensor_fraction(&self) -> [f64; 2] {
        self.sensor_fraction
    }
    pub fn poses(&self) -> &[Orientation] {
        &self.poses
    }

    /// Interpolates a rotation at normalized full-sensor capture time.
    pub(crate) fn rotation_at_fraction(&self, fraction: f64) -> Orientation {
        let position = fraction.clamp(0.0, 1.0) * (self.poses.len() - 1) as f64;
        let index = (position.floor() as usize).min(self.poses.len() - 2);
        self.poses[index].slerp(self.poses[index + 1], position - index as f64)
    }

    pub(crate) fn rotation_at_source(&self, source: [f64; 2], dimensions: [u32; 2]) -> Orientation {
        let axis = match self.direction {
            ReadoutDirection::TopToBottom | ReadoutDirection::BottomToTop => 1,
            ReadoutDirection::LeftToRight | ReadoutDirection::RightToLeft => 0,
        };
        let coordinate = ((source[axis] + 0.5) / f64::from(dimensions[axis])).clamp(0.0, 1.0);
        let mut fraction = self.sensor_fraction[0]
            + coordinate * (self.sensor_fraction[1] - self.sensor_fraction[0]);
        if matches!(
            self.direction,
            ReadoutDirection::BottomToTop | ReadoutDirection::RightToLeft
        ) {
            fraction = 1.0 - fraction;
        }
        self.rotation_at_fraction(fraction)
    }
}

/// Global stabilization plus independent source-sensor motion for two lenses.
#[derive(Clone, Debug)]
pub struct FrameMotion {
    correction: Orientation,
    readout: [Option<ReadoutPoseTable>; 2],
}

impl FrameMotion {
    /// Global correction only, using the same convention as `stitch_with_orientation`.
    pub fn global(correction: Orientation) -> Result<Self> {
        Self::new(correction, [None, None])
    }

    /// `correction.inverse()` maps an output ray into the reference camera body.
    pub fn new(correction: Orientation, readout: [Option<ReadoutPoseTable>; 2]) -> Result<Self> {
        correction.validate()?;
        Ok(Self {
            correction,
            readout,
        })
    }

    pub fn correction(&self) -> Orientation {
        self.correction
    }
    pub fn readout(&self) -> &[Option<ReadoutPoseTable>; 2] {
        &self.readout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_coordinates_respect_crop_and_scan_direction() {
        for direction in [
            ReadoutDirection::TopToBottom,
            ReadoutDirection::BottomToTop,
            ReadoutDirection::LeftToRight,
            ReadoutDirection::RightToLeft,
        ] {
            let table = ReadoutPoseTable::new(
                direction,
                [0.1, 0.9],
                vec![
                    Orientation::IDENTITY,
                    Orientation::from_axis_angle([0.0, 0.0, 1.0], 1.0).unwrap(),
                ],
            )
            .unwrap();
            let position = match direction {
                ReadoutDirection::TopToBottom => 0.1 + 0.75 * 0.8,
                ReadoutDirection::BottomToTop => 0.9 - 0.75 * 0.8,
                ReadoutDirection::LeftToRight => 0.1 + 0.25 * 0.8,
                ReadoutDirection::RightToLeft => 0.9 - 0.25 * 0.8,
            };
            let ray = table
                .rotation_at_source([24.5, 149.5], [100, 200])
                .rotate_vector([1.0, 0.0, 0.0]);
            assert!((ray[1].atan2(ray[0]) - position).abs() < 1e-12);
        }
    }

    #[test]
    fn rejects_ambiguous_or_unbounded_tables() {
        assert!(ReadoutPoseTable::new(
            ReadoutDirection::TopToBottom,
            [0.8, 0.2],
            vec![Orientation::IDENTITY; 2]
        )
        .is_err());
        assert!(ReadoutPoseTable::new(
            ReadoutDirection::TopToBottom,
            [0.0, 1.0],
            vec![Orientation::IDENTITY; MAX_READOUT_POSES + 1]
        )
        .is_err());
        assert!(ReadoutPoseTable::new(
            ReadoutDirection::TopToBottom,
            [0.0, 1.0],
            vec![
                Orientation::IDENTITY,
                Orientation::from_axis_angle([1.0, 0.0, 0.0], 2.0).unwrap()
            ]
        )
        .is_err());
    }
}
