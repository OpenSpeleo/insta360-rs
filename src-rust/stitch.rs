//! Asset-free, geometry-stable CPU stitching primitives.

use std::f64::consts::PI;
use std::sync::Arc;

mod mask;
use mask::FisheyeMask;
pub(crate) use mask::MaskCache;
#[cfg(feature = "gpu")]
pub(crate) use mask::PreparedMasks;

use rayon::prelude::*;

use crate::calibration::{
    LensProjectionModel, ParsedLens, ResolvedCalibration, ResolvedLensGeometry,
};
use crate::motion::{FrameMotion, Orientation, ReadoutPoseTable};
use crate::profile::{lens_profile, RadialMaskRecipe};
use crate::types::EquirectangularProjection;
use crate::{Error, Result};

const RGB_CHANNELS: usize = 3;
const DEFAULT_FEATHER_FRACTION: f64 = 0.08;
const LOW_FREQUENCY_OFFSETS: [f64; 5] = [-64.0, -32.0, 0.0, 32.0, 64.0];
const LOW_FREQUENCY_WEIGHTS: [f64; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
const COLOR_ADJUSTMENT_SAMPLE_STRIDE: usize = 8;
const COLOR_ADJUSTMENT_LONGITUDE_DIVISOR: usize = 5;
const COLOR_ADJUSTMENT_SMOOTH_RADIUS: isize = 10;
const COLOR_ADJUSTMENT_INITIAL_THRESHOLD: f64 = 255.0;
const COLOR_ADJUSTMENT_THRESHOLD_MARGIN: f64 = 30.0;
const COLOR_ADJUSTMENT_RATE: f64 = 0.5;

#[derive(Clone, Copy, Debug)]
struct ProjectedSample {
    color: [f64; RGB_CHANNELS],
    source_x: f64,
    source_y: f64,
    detail_weight: f64,
    illumination_weight: f64,
}

#[derive(Clone, Debug)]
struct ColorAdjustment {
    slopes: [Vec<[f64; RGB_CHANNELS]>; 2],
    height: usize,
    dead_zone: f64,
    body_to_color: Orientation,
}

impl ColorAdjustment {
    fn neutral(width: usize, height: usize) -> Self {
        Self {
            slopes: [
                vec![[0.0; RGB_CHANNELS]; width],
                vec![[0.0; RGB_CHANNELS]; width],
            ],
            height,
            dead_zone: f64::INFINITY,
            body_to_color: Orientation::IDENTITY,
        }
    }

    fn gain(&self, lens_index: usize, column: f64, row: f64) -> [f64; RGB_CHANNELS] {
        let width = self.slopes[lens_index].len();
        let column = column.rem_euclid(width as f64);
        // Floating-point remainder can round a tiny negative coordinate to width.
        let left = column.floor() as usize % width;
        let right = (left + 1) % width;
        let fraction = column - column.floor();
        let row = row.clamp(0.0, (self.height - 1) as f64);
        let distance_from_dead_zone = if lens_index == 0 {
            row - self.dead_zone
        } else {
            (self.height - 1) as f64 - row - self.dead_zone
        }
        .max(0.0);
        std::array::from_fn(|channel| {
            let first = self.slopes[lens_index][left][channel];
            let second = self.slopes[lens_index][right][channel];
            let slope = first + (second - first) * fraction;
            (1.0 + slope * distance_from_dead_zone).max(0.0)
        })
    }

    fn gains_for_direction(&self, direction: [f64; 3]) -> [[f64; RGB_CHANNELS]; 2] {
        if !self.dead_zone.is_finite() {
            return [[1.0; RGB_CHANNELS]; 2];
        }
        // The correction sphere has its north pole at the first lens axis.
        // Vendor ColorAdjustment rotates its statistics sphere by 90 degrees and
        // remaps the resulting gains back. Derive that basis from calibration
        // so the neutral pole follows the actual first lens and its ordering.
        let direction = self.body_to_color.rotate_vector(direction);
        let longitude = (-direction[1]).atan2(direction[0]);
        let colatitude = direction[0].hypot(direction[1]).atan2(direction[2]);
        let column = longitude / (2.0 * PI) * self.slopes[0].len() as f64 - 0.5;
        let row = colatitude / PI * self.height as f64 - 0.5;
        std::array::from_fn(|lens_index| self.gain(lens_index, column, row))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ColorColumnStats {
    sums: [[f64; RGB_CHANNELS]; 2],
    count: u64,
}

/// An owned, tightly packed RGB8 fisheye frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LensFrame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl LensFrame {
    /// Validates and creates a tightly packed RGB8 frame.
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Result<Self> {
        let expected = frame_buffer_len(width, height)?;
        if rgb.len() != expected {
            return Err(Error::InvalidMedia(format!(
                "lens frame requires {expected} RGB bytes, received {}",
                rgb.len()
            )));
        }
        Ok(Self { width, height, rgb })
    }

    /// Frame width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Frame height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Tightly packed RGB8 pixels in row-major order.
    pub fn as_rgb8(&self) -> &[u8] {
        &self.rgb
    }

    /// Consumes the frame and returns its RGB8 storage.
    pub fn into_rgb8(self) -> Vec<u8> {
        self.rgb
    }
}

/// An owned, tightly packed RGB8 equirectangular panorama.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanoramaFrame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl PanoramaFrame {
    /// Validates and creates an equirectangular RGB8 frame.
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Result<Self> {
        EquirectangularProjection { width, height }.validate()?;
        let expected = frame_buffer_len(width, height)?;
        if rgb.len() != expected {
            return Err(Error::InvalidMedia(format!(
                "panorama requires {expected} RGB bytes, received {}",
                rgb.len()
            )));
        }
        Ok(Self { width, height, rgb })
    }

    /// Panorama width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Panorama height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Tightly packed RGB8 pixels in row-major order.
    pub fn as_rgb8(&self) -> &[u8] {
        &self.rgb
    }

    /// Consumes the panorama and returns its RGB8 storage.
    pub fn into_rgb8(self) -> Vec<u8> {
        self.rgb
    }
}

/// Common interface implemented by geometry-stable stitch backends.
pub trait StitchEngine {
    /// Stitches a synchronized pair of lens frames into one equirectangular frame.
    fn stitch(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
    ) -> Result<PanoramaFrame>;
}

/// Deterministic, row-parallel fixed-geometry stitcher.
///
/// Prepared source masks are reused for the current calibration and dimensions.
/// Clones share a bounded mask cache; the stitcher no longer implements `Copy`.
#[derive(Clone, Debug)]
pub struct CpuStitcher {
    feather_fraction: f64,
    masks: Arc<MaskCache>,
}

impl Default for CpuStitcher {
    fn default() -> Self {
        Self {
            feather_fraction: DEFAULT_FEATHER_FRACTION,
            masks: Arc::new(MaskCache::default()),
        }
    }
}

impl CpuStitcher {
    /// Creates a stitcher using the default fixed feather band.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a stitcher with an edge feather expressed as a lens-size fraction.
    pub fn with_feather_fraction(feather_fraction: f64) -> Result<Self> {
        if !feather_fraction.is_finite() || !(0.0..=0.5).contains(&feather_fraction) {
            return Err(Error::InvalidMedia(
                "feather fraction must be finite and between 0 and 0.5".into(),
            ));
        }
        Ok(Self {
            feather_fraction,
            masks: Arc::new(MaskCache::default()),
        })
    }

    /// Stitches while applying a gyro-derived rotation to camera-space rays.
    ///
    /// `correction` is the same output of [`crate::motion::Stabilizer::correction_at`].
    /// It changes only global panorama orientation; calibration and seam
    /// geometry remain fixed across frames.
    pub fn stitch_with_orientation(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        correction: Orientation,
    ) -> Result<PanoramaFrame> {
        self.stitch_with_motion(
            lenses,
            calibration,
            projection,
            &FrameMotion::global(correction)?,
        )
    }

    /// Stitches with global stabilization and per-lens sensor readout correction.
    pub fn stitch_with_motion(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<PanoramaFrame> {
        let projection = projection.validate()?;
        calibration.validate_for_stitching()?;
        for lens in lenses {
            frame_buffer_len(lens.width, lens.height)?;
        }

        let width = usize::try_from(projection.width)
            .map_err(|_| Error::InvalidMedia("panorama width does not fit usize".into()))?;
        let row_len = width
            .checked_mul(RGB_CHANNELS)
            .ok_or_else(|| Error::InvalidMedia("panorama row size overflowed".into()))?;
        let output_len = frame_buffer_len(projection.width, projection.height)?;
        let mut rgb = Vec::new();
        rgb.try_reserve_exact(output_len)
            .map_err(|_| Error::InvalidMedia("cannot allocate the panorama buffer".into()))?;
        rgb.resize(output_len, 0);
        let output_to_camera = motion.correction().inverse();
        let lens_geometry = resolved_render_geometry(calibration)?;
        let prepared_masks = self.masks.prepare(
            lenses
                .each_ref()
                .map(|frame| (frame.width(), frame.height())),
            calibration,
        )?;
        let fisheye_masks = prepared_masks.each_ref().map(Option::as_ref);
        let color_adjustment = estimate_color_adjustment(
            lenses,
            calibration,
            width,
            projection.height as usize,
            self.feather_fraction,
            fisheye_masks,
            lens_geometry,
            motion.readout(),
        );

        rgb.par_chunks_mut(row_len)
            .enumerate()
            .for_each(|(row, output_row)| {
                for column in 0..width {
                    let output_direction =
                        equirectangular_direction(column, row, width, projection.height as usize);
                    let direction = output_to_camera.rotate_vector(output_direction);
                    let mut samples = [None, None];
                    for (lens_index, (frame, lens)) in
                        lenses.iter().zip(calibration.lenses.iter()).enumerate()
                    {
                        samples[lens_index] = project_and_sample(
                            frame,
                            lens,
                            lens_index,
                            direction,
                            self.feather_fraction,
                            fisheye_masks[lens_index],
                            lens_geometry[lens_index],
                            motion.readout()[lens_index].as_ref(),
                        );
                    }
                    let output = &mut output_row[column * RGB_CHANNELS..][..RGB_CHANNELS];
                    if let Some(color) = blend_projected_samples(
                        samples,
                        lenses,
                        color_adjustment.gains_for_direction(direction),
                        fisheye_masks,
                    ) {
                        for channel in 0..RGB_CHANNELS {
                            output[channel] = color[channel].round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            });

        PanoramaFrame::new(projection.width, projection.height, rgb)
    }
}

impl StitchEngine for CpuStitcher {
    fn stitch(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
    ) -> Result<PanoramaFrame> {
        self.stitch_with_orientation(lenses, calibration, projection, Orientation::IDENTITY)
    }
}

fn resolved_render_geometry(
    calibration: &ResolvedCalibration,
) -> Result<[Option<ResolvedLensGeometry>; 2]> {
    let mut geometry = [None, None];
    for (index, lens) in calibration.lenses.iter().enumerate() {
        if lens.lens_type != 0 {
            geometry[index] = Some(calibration.geometry_for_lens(index)?);
        }
    }
    Ok(geometry)
}

fn mask_recipe(calibration: &ResolvedCalibration, lens_index: usize) -> Option<RadialMaskRecipe> {
    let camera = calibration.camera_model.as_ref()?;
    let lens_type = calibration.lenses.get(lens_index)?.lens_type;
    lens_profile(camera, lens_type)?.mask_recipe
}

fn frame_buffer_len(width: u32, height: u32) -> Result<usize> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidMedia(
            "frame dimensions must be non-zero".into(),
        ));
    }
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| Error::InvalidMedia("frame dimensions overflowed".into()))?;
    pixels
        .checked_mul(RGB_CHANNELS)
        .ok_or_else(|| Error::InvalidMedia("RGB frame size overflowed".into()))
}

fn equirectangular_direction(column: usize, row: usize, width: usize, height: usize) -> [f64; 3] {
    // This is the exact BackProject convention embedded in the Studio
    // Flowstate shader: phi = 2*pi-x and latitude = pi/2-y.
    let longitude = ((column as f64 + 0.5) / width as f64) * 2.0 * PI;
    let latitude = PI * 0.5 - ((row as f64 + 0.5) / height as f64) * PI;
    let latitude_cosine = latitude.cos();
    [
        latitude_cosine * longitude.cos(),
        -latitude_cosine * longitude.sin(),
        latitude.sin(),
    ]
}

#[allow(clippy::too_many_arguments)]
fn project_and_sample(
    frame: &LensFrame,
    lens: &ParsedLens,
    lens_index: usize,
    world_direction: [f64; 3],
    feather_fraction: f64,
    fisheye_mask: Option<&FisheyeMask>,
    geometry: Option<ResolvedLensGeometry>,
    readout: Option<&ReadoutPoseTable>,
) -> Option<ProjectedSample> {
    // OffsetParser produces r_c_b: body/sphere rays rotate into lens coordinates.
    // Solve in the decoded source sensor, since its row determines capture time.
    let mut capture_direction = readout.map_or(world_direction, |table| {
        table
            .rotation_at_source(
                [
                    f64::from(frame.width) * 0.5 - 0.5,
                    f64::from(frame.height) * 0.5 - 0.5,
                ],
                [frame.width, frame.height],
            )
            .rotate_vector(world_direction)
    });
    let mut previous_source: Option<[f64; 2]> = None;
    let mut resolved = None;
    for _ in 0..8 {
        let local = lens.orientation.rotate_vector(capture_direction);
        let (calibration_x, calibration_y) =
            project_camera_ray_with_clipping(lens, local, geometry, readout.is_none())?;
        let (source_x, source_y) =
            calibration_to_source(frame, lens, lens_index, calibration_x, calibration_y);
        if !source_x.is_finite() || !source_y.is_finite() {
            return None;
        }
        let source = [source_x, source_y];
        if readout.is_none()
            || previous_source.is_some_and(|previous| {
                (source_x - previous[0])
                    .abs()
                    .max((source_y - previous[1]).abs())
                    <= 0.05
            })
        {
            resolved = Some((local, source_x, source_y));
            break;
        }
        previous_source = Some(source);
        capture_direction = readout?
            .rotation_at_source(source, [frame.width, frame.height])
            .rotate_vector(world_direction);
    }
    // Nonconvergent rays are not valid samples; both backends use this bound.
    let (local, source_x, source_y) = resolved?;
    if readout.is_some() && !camera_ray_within_fov(lens, local, geometry) {
        return None;
    }
    let seam_weights = if lens.lens_type == 0 {
        None
    } else {
        Some(angular_blend_weights(geometry?, local)?)
    };
    if source_x < 0.0
        || source_y < 0.0
        || source_x > f64::from(frame.width - 1)
        || source_y > f64::from(frame.height - 1)
    {
        return None;
    }

    let sample = masked_bilinear_rgb(frame, source_x, source_y, fisheye_mask);
    let edge_distance = source_x
        .min(source_y)
        .min(f64::from(frame.width - 1) - source_x)
        .min(f64::from(frame.height - 1) - source_y);
    let feather_pixels = f64::from(frame.width.min(frame.height)) * feather_fraction;
    let (detail_weight, illumination_weight) = seam_weights.map_or_else(
        || {
            let weight = if feather_pixels <= f64::EPSILON {
                1.0
            } else {
                smoothstep((edge_distance / feather_pixels).clamp(0.0, 1.0))
            };
            (weight, weight)
        },
        |(detail_weight, illumination_weight)| {
            let mask_weight = fisheye_mask
                .map(|mask| mask.weight(source_x, source_y))
                .unwrap_or(1.0);
            (
                detail_weight * mask_weight,
                illumination_weight * mask_weight,
            )
        },
    );
    (detail_weight > 0.0 || illumination_weight > 0.0).then_some(ProjectedSample {
        color: sample,
        source_x,
        source_y,
        detail_weight,
        illumination_weight,
    })
}

#[allow(clippy::too_many_arguments)]
fn estimate_color_adjustment(
    lenses: &[LensFrame; 2],
    calibration: &ResolvedCalibration,
    width: usize,
    height: usize,
    feather_fraction: f64,
    fisheye_masks: [Option<&FisheyeMask>; 2],
    lens_geometry: [Option<ResolvedLensGeometry>; 2],
    readout: &[Option<ReadoutPoseTable>; 2],
) -> ColorAdjustment {
    if calibration.lenses.iter().all(|lens| lens.lens_type == 0) {
        return ColorAdjustment::neutral(width, height);
    }

    let mut first_pass = vec![ColorColumnStats::default(); width];
    for row in (0..height).step_by(COLOR_ADJUSTMENT_SAMPLE_STRIDE) {
        for (column, statistics) in first_pass.iter_mut().enumerate() {
            let samples = project_pair_at(
                lenses,
                calibration,
                width,
                height,
                column,
                row,
                feather_fraction,
                fisheye_masks,
                lens_geometry,
                readout,
            );
            if let [Some(first), Some(second)] = samples {
                if maximum_channel_difference(first.color, second.color)
                    < COLOR_ADJUSTMENT_INITIAL_THRESHOLD
                {
                    add_color_pair(statistics, first.color, second.color);
                }
            }
        }
    }

    let longitude_window = (width / COLOR_ADJUSTMENT_LONGITUDE_DIVISOR).max(1);
    let preliminary = windowed_color_means(&first_pass, longitude_window);
    let thresholds: Vec<f64> = preliminary
        .iter()
        .map(|means| {
            (maximum_channel_difference(means[0], means[1]) + COLOR_ADJUSTMENT_THRESHOLD_MARGIN)
                .trunc()
        })
        .collect();

    let mut second_pass = vec![ColorColumnStats::default(); width];
    for row in (0..height).step_by(COLOR_ADJUSTMENT_SAMPLE_STRIDE) {
        for (column, statistics) in second_pass.iter_mut().enumerate() {
            let samples = project_pair_at(
                lenses,
                calibration,
                width,
                height,
                column,
                row,
                feather_fraction,
                fisheye_masks,
                lens_geometry,
                readout,
            );
            if let [Some(first), Some(second)] = samples {
                if maximum_channel_difference(first.color, second.color) < thresholds[column] {
                    add_color_pair(statistics, first.color, second.color);
                }
            }
        }
    }

    let means = smooth_color_means(
        &windowed_color_means(&second_pass, longitude_window),
        COLOR_ADJUSTMENT_SMOOTH_RADIUS,
    );
    let mut adjustment = ColorAdjustment {
        slopes: [
            vec![[0.0; RGB_CHANNELS]; width],
            vec![[0.0; RGB_CHANNELS]; width],
        ],
        height,
        dead_zone: height as f64 * (1.0 - COLOR_ADJUSTMENT_RATE) * 0.5,
        body_to_color: calibration.lenses[0].orientation,
    };
    let slope_scale = 4.0 / height as f64;
    for (column, lens_means) in means.into_iter().enumerate() {
        for (channel, (first_mean, second_mean)) in
            lens_means[0].into_iter().zip(lens_means[1]).enumerate()
        {
            let middle = (first_mean + second_mean) * 0.5;
            if middle > 0.0 {
                adjustment.slopes[0][column][channel] = slope_scale * (1.0 - first_mean / middle);
                adjustment.slopes[1][column][channel] = slope_scale * (1.0 - second_mean / middle);
            }
        }
    }
    adjustment
}

#[allow(clippy::too_many_arguments)]
fn project_pair_at(
    lenses: &[LensFrame; 2],
    calibration: &ResolvedCalibration,
    width: usize,
    height: usize,
    column: usize,
    row: usize,
    feather_fraction: f64,
    fisheye_masks: [Option<&FisheyeMask>; 2],
    lens_geometry: [Option<ResolvedLensGeometry>; 2],
    readout: &[Option<ReadoutPoseTable>; 2],
) -> [Option<ProjectedSample>; 2] {
    // Gather each cyclic column around the lens axis, in the same coordinates
    // used to evaluate the opposing correction ramps.
    let direction = calibration.lenses[0]
        .orientation
        .inverse()
        .rotate_vector(equirectangular_direction(column, row, width, height));
    std::array::from_fn(|lens_index| {
        project_and_sample(
            &lenses[lens_index],
            &calibration.lenses[lens_index],
            lens_index,
            direction,
            feather_fraction,
            fisheye_masks[lens_index],
            lens_geometry[lens_index],
            readout[lens_index].as_ref(),
        )
    })
}

fn maximum_channel_difference(first: [f64; RGB_CHANNELS], second: [f64; RGB_CHANNELS]) -> f64 {
    (0..RGB_CHANNELS)
        .map(|channel| (first[channel] - second[channel]).abs())
        .fold(0.0, f64::max)
}

fn add_color_pair(
    statistics: &mut ColorColumnStats,
    first: [f64; RGB_CHANNELS],
    second: [f64; RGB_CHANNELS],
) {
    for channel in 0..RGB_CHANNELS {
        statistics.sums[0][channel] += first[channel];
        statistics.sums[1][channel] += second[channel];
    }
    statistics.count += 1;
}

fn windowed_color_means(
    statistics: &[ColorColumnStats],
    window_width: usize,
) -> Vec<[[f64; RGB_CHANNELS]; 2]> {
    let width = statistics.len();
    let half_window = window_width / 2;
    (0..width)
        .map(|column| {
            let mut sums = [[0.0; RGB_CHANNELS]; 2];
            let mut count = 0_u64;
            for offset in 0..window_width {
                let source = (column + width + offset - half_window) % width;
                for (lens_sums, source_sums) in sums.iter_mut().zip(statistics[source].sums) {
                    for (sum, source_sum) in lens_sums.iter_mut().zip(source_sums) {
                        *sum += source_sum;
                    }
                }
                count += statistics[source].count;
            }
            if count > 0 {
                for lens in &mut sums {
                    for channel in lens {
                        *channel /= count as f64;
                    }
                }
            }
            sums
        })
        .collect()
}

fn smooth_color_means(
    means: &[[[f64; RGB_CHANNELS]; 2]],
    radius: isize,
) -> Vec<[[f64; RGB_CHANNELS]; 2]> {
    let width = means.len() as isize;
    let sample_count = (radius * 2 + 1) as f64;
    (0..width)
        .map(|column| {
            let mut smoothed = [[0.0; RGB_CHANNELS]; 2];
            for offset in -radius..=radius {
                let source = (column + offset).rem_euclid(width) as usize;
                for lens in 0..2 {
                    for channel in 0..RGB_CHANNELS {
                        smoothed[lens][channel] += means[source][lens][channel] / sample_count;
                    }
                }
            }
            smoothed
        })
        .collect()
}

fn blend_projected_samples(
    samples: [Option<ProjectedSample>; 2],
    frames: &[LensFrame; 2],
    gains: [[f64; RGB_CHANNELS]; 2],
    fisheye_masks: [Option<&FisheyeMask>; 2],
) -> Option<[f64; RGB_CHANNELS]> {
    match samples {
        [None, None] => None,
        [Some(sample), None] => Some(apply_rgb_gain(sample.color, gains[0])),
        [None, Some(sample)] => Some(apply_rgb_gain(sample.color, gains[1])),
        [Some(first), Some(second)] => {
            let color = [
                apply_rgb_gain(first.color, gains[0]),
                apply_rgb_gain(second.color, gains[1]),
            ];
            let mut low = [
                low_frequency_rgb(
                    frames.first()?,
                    first.source_x,
                    first.source_y,
                    fisheye_masks[0],
                ),
                low_frequency_rgb(
                    frames.get(1)?,
                    second.source_x,
                    second.source_y,
                    fisheye_masks[1],
                ),
            ];
            low[0] = apply_rgb_gain(low[0], gains[0]);
            low[1] = apply_rgb_gain(low[1], gains[1]);
            let detail_total = first.detail_weight + second.detail_weight;
            let illumination_total = first.illumination_weight + second.illumination_weight;
            if detail_total <= f64::EPSILON || illumination_total <= f64::EPSILON {
                return Some(if first.detail_weight >= second.detail_weight {
                    color[0]
                } else {
                    color[1]
                });
            }

            let mut output = [0.0; RGB_CHANNELS];
            for channel in 0..RGB_CHANNELS {
                let high_first = color[0][channel] - low[0][channel];
                let high_second = color[1][channel] - low[1][channel];
                let high = (high_first * first.detail_weight + high_second * second.detail_weight)
                    / detail_total;
                let illumination = (low[0][channel] * first.illumination_weight
                    + low[1][channel] * second.illumination_weight)
                    / illumination_total;
                output[channel] = high + illumination;
            }
            Some(output)
        }
    }
}

fn apply_rgb_gain(color: [f64; RGB_CHANNELS], gain: [f64; RGB_CHANNELS]) -> [f64; RGB_CHANNELS] {
    std::array::from_fn(|channel| color[channel] * gain[channel])
}

fn low_frequency_rgb(
    frame: &LensFrame,
    source_x: f64,
    source_y: f64,
    fisheye_mask: Option<&FisheyeMask>,
) -> [f64; RGB_CHANNELS] {
    let maximum_x = f64::from(frame.width - 1);
    let maximum_y = f64::from(frame.height - 1);
    let mut result = [0.0; RGB_CHANNELS];
    let mut total_weight = 0.0;
    for (vertical_offset, vertical_weight) in
        LOW_FREQUENCY_OFFSETS.into_iter().zip(LOW_FREQUENCY_WEIGHTS)
    {
        for (horizontal_offset, horizontal_weight) in
            LOW_FREQUENCY_OFFSETS.into_iter().zip(LOW_FREQUENCY_WEIGHTS)
        {
            let x = (source_x + horizontal_offset).clamp(0.0, maximum_x);
            let y = (source_y + vertical_offset).clamp(0.0, maximum_y);
            let validity = fisheye_mask.map_or(1.0, |mask| mask.weight(x, y));
            let weight = vertical_weight * horizontal_weight * validity;
            if weight <= 0.0 {
                continue;
            }
            let sample = masked_bilinear_rgb(frame, x, y, fisheye_mask);
            for channel in 0..RGB_CHANNELS {
                result[channel] += sample[channel] * weight;
            }
            total_weight += weight;
        }
    }
    if total_weight <= 0.0 {
        return masked_bilinear_rgb(frame, source_x, source_y, fisheye_mask);
    }
    for channel in &mut result {
        *channel /= total_weight;
    }
    result
}

#[cfg(test)]
fn test_geometry(lens_type: u32) -> Option<ResolvedLensGeometry> {
    crate::profile::lens_profiles_for_id(lens_type)
        .next()
        .and_then(|(_, profile)| {
            Some(ResolvedLensGeometry {
                full_fov_degrees: profile.fallback.full_fov_degrees,
                blend_angle_degrees: profile.fallback.blend_angle_degrees?,
                blend_angle_recorded: false,
            })
        })
}

#[cfg(test)]
fn x5_angular_blend_weight(lens_type: u32, ray: [f64; 3]) -> Option<f64> {
    angular_blend_weights(test_geometry(lens_type)?, ray).map(|weights| weights.0)
}

fn angular_blend_weights(geometry: ResolvedLensGeometry, ray: [f64; 3]) -> Option<(f64, f64)> {
    let norm = (ray[0] * ray[0] + ray[1] * ray[1] + ray[2] * ray[2]).sqrt();
    if norm <= f64::EPSILON || !norm.is_finite() {
        return None;
    }

    // The lens blend angle is the full support of the fisheye mask. Two
    // opposing masks overlap by blend_angle - 180°; Studio feeds that overlap
    // width to calAlpha and sharpens the result with exponent 5.2.
    let blend_angle = geometry.blend_angle_radians();
    let angle = (ray[2] / norm).clamp(-1.0, 1.0).acos();
    let seam_belt = blend_angle - PI;
    let detail_alpha = overlap_alpha(angle, seam_belt);
    let illumination_belt = geometry.half_fov_radians() * 2.0 - PI;
    let illumination_alpha = overlap_alpha(angle, illumination_belt);
    Some((studio_sharpen_alpha(detail_alpha), illumination_alpha))
}

fn overlap_alpha(angle: f64, belt: f64) -> f64 {
    const HARD_SEAM_EPSILON: f64 = 1.0e-6;
    let distance_from_seam = PI * 0.5 - angle;
    if belt <= HARD_SEAM_EPSILON {
        return if distance_from_seam > HARD_SEAM_EPSILON {
            1.0
        } else if distance_from_seam < -HARD_SEAM_EPSILON {
            0.0
        } else {
            0.5
        };
    }
    (distance_from_seam / belt + 0.5).clamp(0.0, 1.0)
}

fn calibration_to_source(
    frame: &LensFrame,
    lens: &ParsedLens,
    lens_index: usize,
    calibration_x: f64,
    calibration_y: f64,
) -> (f64, f64) {
    calibration_to_source_dimensions(
        (frame.width(), frame.height()),
        lens,
        lens_index,
        calibration_x,
        calibration_y,
    )
}

fn calibration_to_source_dimensions(
    frame_dimensions: (u32, u32),
    lens: &ParsedLens,
    lens_index: usize,
    calibration_x: f64,
    calibration_y: f64,
) -> (f64, f64) {
    let lens_canvas_width = f64::from(lens.canvas_width) / 2.0;
    let lens_origin_x = lens_canvas_width * lens_index as f64;
    (
        (calibration_x - lens_origin_x) * f64::from(frame_dimensions.0) / lens_canvas_width,
        calibration_y * f64::from(frame_dimensions.1) / f64::from(lens.canvas_height),
    )
}

fn interpolate_mask_radius_squared(
    azimuth_degrees: f64,
    boundary: &[(f64, f64)],
    outer_radius_squared: f64,
) -> f64 {
    for pair in boundary.windows(2) {
        let (start_angle, start_radius_squared) = pair[0];
        let (end_angle, end_radius_squared) = pair[1];
        if azimuth_degrees < end_angle {
            let position =
                ((azimuth_degrees - start_angle) / (end_angle - start_angle)).clamp(0.0, 1.0);
            return start_radius_squared + (end_radius_squared - start_radius_squared) * position;
        }
    }
    outer_radius_squared
}

fn studio_sharpen_alpha(alpha: f64) -> f64 {
    const SHARPNESS: f64 = 5.2;
    if alpha <= 0.5 {
        0.5 * (2.0 * alpha).powf(SHARPNESS)
    } else {
        1.0 - 0.5 * (2.0 * (1.0 - alpha)).powf(SHARPNESS)
    }
}

#[cfg(test)]
fn project_camera_ray(
    lens: &ParsedLens,
    ray: [f64; 3],
    geometry: Option<ResolvedLensGeometry>,
) -> Option<(f64, f64)> {
    project_camera_ray_with_clipping(lens, ray, geometry, true)
}

fn camera_ray_within_fov(
    lens: &ParsedLens,
    ray: [f64; 3],
    geometry: Option<ResolvedLensGeometry>,
) -> bool {
    if lens.lens_type == 0 {
        return true;
    }
    let Some(geometry) = geometry else {
        return false;
    };
    let norm = (ray[0] * ray[0] + ray[1] * ray[1] + ray[2] * ray[2]).sqrt();
    if norm <= f64::EPSILON || !norm.is_finite() {
        return false;
    }
    match lens.model {
        LensProjectionModel::OmniRadtan | LensProjectionModel::OmniRadtanPro => {
            ray[2] / norm >= geometry.half_fov_radians().cos() - 0.01
        }
        LensProjectionModel::PinholePolynomialV1 | LensProjectionModel::PinholePolynomialV2 => {
            ray[0].hypot(ray[1]).atan2(ray[2]) < geometry.half_fov_radians()
        }
    }
}

fn project_camera_ray_with_clipping(
    lens: &ParsedLens,
    ray: [f64; 3],
    geometry: Option<ResolvedLensGeometry>,
    clip: bool,
) -> Option<(f64, f64)> {
    if clip && !camera_ray_within_fov(lens, ray, geometry) {
        return None;
    }
    if !ray.iter().all(|value| value.is_finite()) {
        return None;
    }

    // Synthetic calibrations predate the native model representation. Keep
    // their deterministic equidistant projection for downstream unit tests.
    if lens.lens_type == 0 {
        return project_equidistant_compatibility(lens, ray);
    }

    match lens.model {
        LensProjectionModel::OmniRadtan => project_omni(lens, ray, false),
        LensProjectionModel::OmniRadtanPro => project_omni(lens, ray, true),
        LensProjectionModel::PinholePolynomialV1 | LensProjectionModel::PinholePolynomialV2 => {
            project_polynomial(lens, ray)
        }
    }
}

fn project_omni(lens: &ParsedLens, ray: [f64; 3], pro: bool) -> Option<(f64, f64)> {
    let norm = (ray[0] * ray[0] + ray[1] * ray[1] + ray[2] * ray[2]).sqrt();
    if norm <= f64::EPSILON || !norm.is_finite() {
        return None;
    }

    let denominator = ray[2] + lens.xi? * norm;
    if denominator.abs() <= f64::EPSILON {
        return None;
    }
    let undistorted = [ray[0] / denominator, ray[1] / denominator];
    let distorted = if pro {
        radtan_distort_pro(undistorted, &lens.distortion_coefficients)?
    } else {
        radtan_distort(undistorted, &lens.distortion_coefficients)?
    };
    let x = lens.cx + distorted[0] * lens.fx;
    let y = lens.cy + distorted[1] * lens.fy;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

fn radtan_distort(position: [f64; 2], coefficients: &[f64]) -> Option<[f64; 2]> {
    let &[k1, k2, k3, p1, p2] = coefficients else {
        return None;
    };
    let x2 = position[0] * position[0];
    let y2 = position[1] * position[1];
    let xy = position[0] * position[1];
    let r2 = x2 + y2;
    let r4 = r2 * r2;
    let r6 = r4 * r2;
    let radial_delta = k1 * r2 + k2 * r4 + k3 * r6;
    Some([
        position[0] + position[0] * radial_delta + 2.0 * p1 * xy + p2 * (r2 + 2.0 * x2),
        position[1] + position[1] * radial_delta + 2.0 * p2 * xy + p1 * (r2 + 2.0 * y2),
    ])
}

fn radtan_distort_pro(position: [f64; 2], coefficients: &[f64]) -> Option<[f64; 2]> {
    let &[k1, k2, k3, k4, k5, p1, p2, p3, p4, s1, s2, s3, s4] = coefficients else {
        return None;
    };
    let x2 = position[0] * position[0];
    let y2 = position[1] * position[1];
    let xy = position[0] * position[1];
    let r2 = x2 + y2;
    let r4 = r2 * r2;
    let r6 = r4 * r2;
    let r8 = r6 * r2;
    let r10 = r8 * r2;
    let radial = 1.0 + k1 * r2 + k2 * r4 + k3 * r6 + k4 * r8 + k5 * r10;
    Some([
        position[0] * radial
            + (p1 + r2 * p3) * (r2 + 2.0 * x2)
            + 2.0 * (p2 + r2 * p4) * xy
            + s1 * r2
            + s3 * r4,
        position[1] * radial
            + (p2 + r2 * p4) * (r2 + 2.0 * y2)
            + 2.0 * (p1 + r2 * p3) * xy
            + s2 * r2
            + s4 * r4,
    ])
}

fn project_polynomial(lens: &ParsedLens, ray: [f64; 3]) -> Option<(f64, f64)> {
    let radial_length = ray[0].hypot(ray[1]);
    let theta = radial_length.atan2(ray[2]);
    if radial_length <= f64::EPSILON {
        return Some((lens.cx, lens.cy));
    }
    let normalization = lens.polynomial_projection?;
    let [c0, c1, c2, c3] = normalization.coefficients;
    let theta2 = theta * theta;
    let distorted_theta = theta * (c0 + c1 * theta + c2 * theta2 + c3 * theta2 * theta);
    let x =
        lens.cx + lens.fx * normalization.focal_scale * distorted_theta * ray[0] / radial_length;
    let y =
        lens.cy + lens.fy * normalization.focal_scale * distorted_theta * ray[1] / radial_length;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

fn project_equidistant_compatibility(lens: &ParsedLens, ray: [f64; 3]) -> Option<(f64, f64)> {
    let radial_length = ray[0].hypot(ray[1]);
    let theta = ray[2].clamp(-1.0, 1.0).acos();
    let theta2 = theta * theta;
    let distorted_theta =
        theta * (1.0 + lens.k1 * theta2 + lens.k2 * theta2 * theta2 + lens.k3 * theta2.powi(3));
    if !distorted_theta.is_finite() || distorted_theta < 0.0 {
        return None;
    }
    if radial_length <= f64::EPSILON {
        return Some((lens.cx, lens.cy));
    }
    Some((
        lens.cx + lens.fx * distorted_theta * ray[0] / radial_length,
        lens.cy - lens.fy * distorted_theta * ray[1] / radial_length,
    ))
}

fn masked_bilinear_rgb(
    frame: &LensFrame,
    x: f64,
    y: f64,
    mask: Option<&FisheyeMask>,
) -> [f64; RGB_CHANNELS] {
    let Some(mask) = mask else {
        return bilinear_rgb(frame, x, y);
    };
    let low = [x.floor() as usize, y.floor() as usize];
    let high = [x.ceil() as usize, y.ceil() as usize];
    let fractions = [x - low[0] as f64, y - low[1] as f64];
    let mut sum = [0.0; RGB_CHANNELS];
    let mut support = 0.0;
    for (row, vertical) in [(low[1], 1.0 - fractions[1]), (high[1], fractions[1])] {
        for (column, horizontal) in [(low[0], 1.0 - fractions[0]), (high[0], fractions[0])] {
            let weight = vertical * horizontal;
            if weight <= 0.0 || mask.pixel_weight(column, row) <= 0.0 {
                continue;
            }
            let offset = (row * frame.width as usize + column) * RGB_CHANNELS;
            for (channel, value) in sum.iter_mut().enumerate() {
                *value += f64::from(frame.rgb[offset + channel]) * weight;
            }
            support += weight;
        }
    }
    if support > 0.0 {
        sum.map(|value| value / support)
    } else {
        [0.0; RGB_CHANNELS]
    }
}

fn bilinear_rgb(frame: &LensFrame, x: f64, y: f64) -> [f64; RGB_CHANNELS] {
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(frame.width as usize - 1);
    let y1 = (y0 + 1).min(frame.height as usize - 1);
    let horizontal = x - x0 as f64;
    let vertical = y - y0 as f64;
    let width = frame.width as usize;

    let mut result = [0.0; RGB_CHANNELS];
    for (channel, result_channel) in result.iter_mut().enumerate() {
        let top_left = f64::from(frame.rgb[(y0 * width + x0) * RGB_CHANNELS + channel]);
        let top_right = f64::from(frame.rgb[(y0 * width + x1) * RGB_CHANNELS + channel]);
        let bottom_left = f64::from(frame.rgb[(y1 * width + x0) * RGB_CHANNELS + channel]);
        let bottom_right = f64::from(frame.rgb[(y1 * width + x1) * RGB_CHANNELS + channel]);
        let top = top_left + (top_right - top_left) * horizontal;
        let bottom = bottom_left + (bottom_right - bottom_left) * horizontal;
        *result_channel = top + (bottom - top) * vertical;
    }
    result
}

fn smoothstep(value: f64) -> f64 {
    value * value * (3.0 - 2.0 * value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::synthetic_dual_fisheye_calibration;

    #[test]
    fn native_polynomial_projection_matches_independent_degree_radius_coordinates() {
        // Independent high-precision degree-polynomial references. In V1 the
        // nonzero b0 is intentionally excluded; V2 uses the recorded b1..b4.
        for (model, id, native, radius45, radius90) in [
            (
                LensProjectionModel::PinholePolynomialV1,
                17,
                vec![],
                479.3342801514986,
                910.5209103966005,
            ),
            (
                LensProjectionModel::PinholePolynomialV2,
                19,
                vec![0.02, 0.00003, -0.0000001, -0.000000001],
                451.2080357142857,
                906.9,
            ),
        ] {
            let mut lens = synthetic_dual_fisheye_calibration(2400, 2200)
                .unwrap()
                .lenses[0]
                .clone();
            lens.model = model;
            lens.lens_type = id;
            lens.radius = Some(1000.0);
            lens.fx = 1000.0;
            lens.fy = 1000.0;
            lens.cx = 1200.0;
            lens.cy = 1100.0;
            lens.distortion_coefficients = native;
            lens.refresh_polynomial_projection().unwrap();
            let geometry = Some(ResolvedLensGeometry {
                full_fov_degrees: 210.0,
                blend_angle_degrees: 200.0,
                blend_angle_recorded: false,
            });
            for (angle, radius) in [(0.0_f64, 0.0), (45.0, radius45), (90.0, radius90)] {
                for azimuth in [0.0_f64, 37.0, 90.0, 180.0, 270.0] {
                    let (sin, cos) = angle.to_radians().sin_cos();
                    let (azimuth_sin, azimuth_cos) = azimuth.to_radians().sin_cos();
                    let ray = [sin * azimuth_cos, sin * azimuth_sin, cos];
                    let projected = project_camera_ray(&lens, ray, geometry).unwrap();
                    assert!((projected.0 - (1200.0 + radius * azimuth_cos)).abs() < 1.0e-9);
                    assert!((projected.1 - (1100.0 + radius * azimuth_sin)).abs() < 1.0e-9);
                }
            }
            assert!(project_camera_ray(&lens, [0.0, 0.0, -1.0], geometry).is_none());
            // Anisotropic decoded scaling applies to the shared normalized
            // focal lengths without changing the retained native radius.
            lens.fx *= 0.5;
            lens.fy *= 0.25;
            let projected = project_camera_ray(&lens, [0.0, 1.0, 0.0], geometry).unwrap();
            assert_eq!(projected.0, 1200.0);
            assert!((projected.1 - (1100.0 + radius90 * 0.25)).abs() < 1.0e-9);
            assert_eq!(lens.radius, Some(1000.0));
        }
    }

    #[test]
    fn radtan_pro_matches_the_studio_shader_coefficient_order() {
        let coefficients: Vec<f64> = (1..=13).map(|value| f64::from(value) / 100.0).collect();
        let projected = radtan_distort_pro([0.2, -0.1], &coefficients).expect("13 coefficients");

        assert!((projected[0] - 0.210750803125).abs() < 1.0e-14);
        assert!((projected[1] - -0.0915754015625).abs() < 1.0e-14);
    }

    #[test]
    fn x5_omni_pro_uses_xi_and_rejects_rays_outside_the_lens_fov() {
        let mut lens = synthetic_dual_fisheye_calibration(200, 200)
            .expect("synthetic lens")
            .lenses[0]
            .clone();
        lens.model = LensProjectionModel::OmniRadtanPro;
        lens.xi = Some(2.0);
        lens.cx = 100.0;
        lens.cy = 100.0;
        lens.fx = 100.0;
        lens.fy = 100.0;
        lens.distortion_coefficients = vec![0.0; 13];
        lens.lens_type = 113;
        let geometry = test_geometry(113).expect("X5 geometry");

        assert_eq!(
            project_camera_ray(&lens, [0.0, 0.0, 1.0], Some(geometry)),
            Some((100.0, 100.0))
        );
        assert_eq!(
            project_camera_ray(&lens, [1.0, 0.0, 0.0], Some(geometry)),
            Some((150.0, 100.0))
        );
        assert_eq!(
            project_camera_ray(&lens, [0.0, 0.0, -1.0], Some(geometry)),
            None
        );
    }

    #[test]
    fn x5_type_113_blend_mask_matches_studio_sharpened_seam() {
        fn ray_at_degrees(degrees: f64) -> [f64; 3] {
            let angle = degrees.to_radians();
            [angle.sin(), 0.0, angle.cos()]
        }

        assert_eq!(
            x5_angular_blend_weight(113, ray_at_degrees(80.0)),
            Some(1.0)
        );
        assert_eq!(
            x5_angular_blend_weight(113, ray_at_degrees(85.0)),
            Some(1.0)
        );
        assert!(
            (x5_angular_blend_weight(113, ray_at_degrees(90.0)).expect("seam center") - 0.5).abs()
                < 1.0e-12
        );
        let quarter =
            x5_angular_blend_weight(113, ray_at_degrees(92.5)).expect("quarter-weight point");
        assert!((quarter - 0.5 * 0.5_f64.powf(5.2)).abs() < 1.0e-12);
        let feather_edge =
            x5_angular_blend_weight(113, ray_at_degrees(94.0)).expect("feather edge");
        assert!((feather_edge - 0.5 * 0.2_f64.powf(5.2)).abs() < 1.0e-12);
        assert_eq!(
            x5_angular_blend_weight(113, ray_at_degrees(95.0)),
            Some(0.0)
        );
    }

    #[test]
    fn x5_diving_water_uses_its_six_degree_overlap() {
        fn ray_at_degrees(degrees: f64, azimuth_degrees: f64) -> [f64; 3] {
            let angle = degrees.to_radians();
            let azimuth = azimuth_degrees.to_radians();
            [
                angle.sin() * azimuth.sin(),
                angle.sin() * azimuth.cos(),
                angle.cos(),
            ]
        }

        assert_eq!(
            x5_angular_blend_weight(117, ray_at_degrees(87.0, 0.0)),
            Some(1.0)
        );
        assert!(
            (x5_angular_blend_weight(117, ray_at_degrees(90.0, 0.0)).expect("seam center") - 0.5)
                .abs()
                < 1.0e-12
        );
        assert_eq!(
            x5_angular_blend_weight(117, ray_at_degrees(93.0, 0.0)),
            Some(0.0)
        );
    }

    #[test]
    fn zero_width_overlap_uses_a_finite_hard_seam() {
        fn ray_at_degrees(degrees: f64) -> [f64; 3] {
            let angle = degrees.to_radians();
            [angle.sin(), 0.0, angle.cos()]
        }

        let geometry = ResolvedLensGeometry {
            full_fov_degrees: 200.0,
            blend_angle_degrees: 180.0,
            blend_angle_recorded: true,
        };
        for (angle, expected) in [(89.0, 1.0), (90.0, 0.5), (91.0, 0.0)] {
            let (detail, illumination) =
                angular_blend_weights(geometry, ray_at_degrees(angle)).expect("finite ray");
            assert!(detail.is_finite());
            assert!(illumination.is_finite());
            assert!((detail - expected).abs() < 1.0e-12);
        }
    }

    #[test]
    fn x5_fisheye_mask_interpolates_source_pixel_radius_squared() {
        let boundary = [(0.0, 100.0), (10.0, 100.0), (20.0, 144.0), (60.0, 196.0)];

        assert_eq!(
            interpolate_mask_radius_squared(5.0, &boundary, 196.0),
            100.0
        );
        assert_eq!(
            interpolate_mask_radius_squared(15.0, &boundary, 196.0),
            122.0
        );
        assert_eq!(
            interpolate_mask_radius_squared(70.0, &boundary, 196.0),
            196.0
        );
    }

    #[test]
    fn color_adjustment_gain_uses_opposing_vertical_ramps() {
        let adjustment = ColorAdjustment {
            slopes: [vec![[0.001; RGB_CHANNELS]], vec![[-0.001; RGB_CHANNELS]]],
            height: 100,
            dead_zone: 25.0,
            body_to_color: Orientation::IDENTITY,
        };

        assert_eq!(adjustment.gain(0, 0.0, 24.0), [1.0; RGB_CHANNELS]);
        assert_eq!(adjustment.gain(0, 0.0, 75.0), [1.05; RGB_CHANNELS]);
        assert_eq!(adjustment.gain(1, 0.0, 75.0), [1.0; RGB_CHANNELS]);
        assert_eq!(adjustment.gain(1, 0.0, 24.0), [0.95; RGB_CHANNELS]);
    }

    #[test]
    fn color_adjustment_interpolates_across_longitude_wrap_and_fractional_rows() {
        let columns: Vec<_> = [0.001, 0.002, 0.004, 0.008]
            .map(|slope| [slope; RGB_CHANNELS])
            .into();
        let adjustment = ColorAdjustment {
            slopes: [columns.clone(), columns],
            height: 100,
            dead_zone: 25.0,
            body_to_color: Orientation::IDENTITY,
        };

        let wrapped = adjustment.gain(0, -0.5, 25.5);
        assert_eq!(wrapped, adjustment.gain(0, 3.5, 25.5));
        assert!(wrapped.iter().all(|gain| (*gain - 1.00225).abs() < 1.0e-12));
        let interior = adjustment.gain(0, 0.5, 25.5);
        assert!(interior
            .iter()
            .all(|gain| (*gain - 1.00075).abs() < 1.0e-12));
        // rem_euclid can round this negative coordinate to the panorama width.
        assert_eq!(
            adjustment.gain(0, -f64::EPSILON, 25.5),
            adjustment.gain(0, 0.0, 25.5),
        );
    }

    #[test]
    fn color_adjustment_longitude_window_wraps() {
        let mut statistics = vec![ColorColumnStats::default(); 5];
        for (index, entry) in statistics.iter_mut().enumerate() {
            entry.count = 1;
            entry.sums = [[index as f64; RGB_CHANNELS]; 2];
        }

        let means = windowed_color_means(&statistics, 3);
        assert_eq!(means[0][0], [5.0 / 3.0; RGB_CHANNELS]);
        assert_eq!(means[4][1], [7.0 / 3.0; RGB_CHANNELS]);
    }
}
