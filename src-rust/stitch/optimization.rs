//! Deterministic, resolution-independent per-pair stitching correction.

use super::{flow, mask::PreparedMasks, LensFrame, MaskCache, StitchSource};
use crate::{Error, FrameMotion, Orientation, ResolvedCalibration, Result, SeamMode};
use rayon::prelude::*;
use std::f64::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(any(feature = "media", test))]
use std::sync::OnceLock;

/// Immutable correction for one source pair. The owner must retain the exact pair
/// identity alongside this value; reusing it for another timestamp is invalid.
#[derive(Clone, Debug)]
pub struct PreparedStitchPlan {
    mode: SeamMode,
    calibration: ResolvedCalibration,
    dimensions: [(u32, u32); 2],
    readout: Vec<u64>,
    basis: Orientation,
    half_height: f64,
    field: flow::Field,
}

impl PreparedStitchPlan {
    pub fn mode(&self) -> SeamMode {
        self.mode
    }
    /// Fraction of the analysis belt with independently consistent correspondence.
    pub fn confidence_coverage(&self) -> f64 {
        if self.field.vectors.is_empty() {
            return 0.0;
        }
        self.field
            .vectors
            .iter()
            .filter(|v| v.confidence >= 0.5)
            .count() as f64
            / self.field.vectors.len() as f64
    }
    pub fn validate(
        &self,
        calibration: &ResolvedCalibration,
        dimensions: [(u32, u32); 2],
        motion: &FrameMotion,
    ) -> Result<()> {
        if self.calibration != *calibration
            || self.dimensions != dimensions
            || self.readout != readout_key(motion)
        {
            return Err(Error::InvalidMedia(
                "stitch plan does not match source dimensions, calibration or sensor motion".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn direction(&self, lens_index: usize, direction: [f64; 3]) -> [f64; 3] {
        if lens_index == 0 || self.field.vectors.is_empty() {
            return direction;
        }
        let local = self.basis.rotate_vector(direction);
        let theta = local[0].hypot(local[1]).atan2(local[2]);
        let latitude = theta - PI * 0.5;
        if latitude.abs() >= self.half_height {
            return direction;
        }
        let phi = local[1].atan2(local[0]);
        let x = (phi / TAU * self.field.width as f64 - 0.5) as f32;
        let y = ((latitude / self.half_height + 1.0) * 0.5 * self.field.height as f64 - 0.5) as f32;
        let [dx, dy] = sample_displacement(&self.field, x, y);
        if dx == 0.0 && dy == 0.0 {
            return direction;
        }
        // Taper and confidence rejection are already baked into shared vertices.
        // Interpolation to zero displacement is continuous at calibrated fallback.
        let phi = phi + f64::from(dx) * TAU / self.field.width as f64;
        let theta = theta + f64::from(dy) * 2.0 * self.half_height / self.field.height as f64;
        self.basis.inverse().rotate_vector([
            theta.sin() * phi.cos(),
            theta.sin() * phi.sin(),
            theta.cos(),
        ])
    }
    #[cfg(feature = "gpu")]
    pub(crate) fn gpu_data(&self) -> Vec<[f32; 4]> {
        let mut result = Vec::with_capacity(self.field.vectors.len() + 3);
        result.push([
            self.field.width as f32,
            self.field.height as f32,
            self.half_height as f32,
            0.0,
        ]);
        result.push([
            self.basis.w as f32,
            self.basis.x as f32,
            self.basis.y as f32,
            self.basis.z as f32,
        ]);
        result.extend(
            self.field
                .vectors
                .iter()
                .map(|v| [v.dx, v.dy, v.confidence, 0.0]),
        );
        result
    }
}

fn readout_key(motion: &FrameMotion) -> Vec<u64> {
    let mut key = Vec::new();
    for table in motion.readout() {
        if let Some(table) = table {
            key.push(table.direction() as u64 + 1);
            key.extend(table.sensor_fraction().map(f64::to_bits));
            for p in table.poses() {
                key.extend([p.w, p.x, p.y, p.z].map(f64::to_bits));
            }
        } else {
            key.push(0);
        }
    }
    key
}

/// Continuous previews share a bounded budget instead of competing for every
/// global Rayon worker with decoders and other CPU-intensive applications.
/// Export and standalone preparation retain their caller's Rayon policy.
#[cfg(any(feature = "media", test))]
fn interactive_pool() -> Result<&'static rayon::ThreadPool> {
    static POOL: OnceLock<std::result::Result<rayon::ThreadPool, String>> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism()
            .map_or(1, |count| count.get())
            .min(4);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("insv-stitch-preview-{index}"))
            .build()
            .map_err(|error| format!("cannot start interactive stitching workers: {error}"))
    })
    .as_ref()
    .map_err(|reason| Error::MissingCapability(reason.clone()))
}

/// Reusable preparation. No previous frame contributes to a new correction map.
#[derive(Debug, Default)]
pub struct StitchPlanner {
    #[cfg(any(feature = "media", test))]
    interactive: bool,
    masks: Arc<MaskCache>,
    boundary: Option<(Arc<PreparedMasks>, [Vec<usize>; 2])>,
    #[cfg(feature = "ai-stitching")]
    ai_model: Option<crate::seam_ai::SeamModel>,
}

impl StitchPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes only CPU scheduling; calibration and per-pair solver work are unchanged.
    #[cfg(any(feature = "media", test))]
    pub(crate) fn set_interactive(&mut self, interactive: bool) {
        self.interactive = interactive;
    }

    #[cfg(feature = "media")]
    pub(crate) fn with_masks(masks: Arc<MaskCache>) -> Self {
        Self {
            masks,
            ..Self::default()
        }
    }
    pub fn prepare(
        &mut self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        motion: &FrameMotion,
        mode: SeamMode,
    ) -> Result<Arc<PreparedStitchPlan>> {
        self.prepare_sources(
            &lenses.each_ref().map(StitchSource::Rgb),
            calibration,
            motion,
            mode,
        )
    }
    pub fn prepare_sources(
        &mut self,
        sources: &[StitchSource<'_>; 2],
        calibration: &ResolvedCalibration,
        motion: &FrameMotion,
        mode: SeamMode,
    ) -> Result<Arc<PreparedStitchPlan>> {
        self.prepare_sources_controlled(sources, calibration, motion, mode, &AtomicBool::new(false))
    }

    pub fn prepare_sources_controlled(
        &mut self,
        sources: &[StitchSource<'_>; 2],
        calibration: &ResolvedCalibration,
        motion: &FrameMotion,
        mode: SeamMode,
        cancel: &AtomicBool,
    ) -> Result<Arc<PreparedStitchPlan>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if let Some(reason) = mode.unavailable_reason() {
            return Err(Error::MissingCapability(reason.into()));
        }
        #[cfg(any(feature = "media", test))]
        if self.interactive && mode != SeamMode::Fixed {
            return interactive_pool()?.install(|| {
                self.prepare_sources_inner(sources, calibration, motion, mode, cancel)
            });
        }
        self.prepare_sources_inner(sources, calibration, motion, mode, cancel)
    }

    fn prepare_sources_inner(
        &mut self,
        sources: &[StitchSource<'_>; 2],
        calibration: &ResolvedCalibration,
        motion: &FrameMotion,
        mode: SeamMode,
        cancel: &AtomicBool,
    ) -> Result<Arc<PreparedStitchPlan>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        calibration.validate_for_stitching()?;
        for source in sources {
            source.validate()?;
        }
        let dimensions = sources.each_ref().map(StitchSource::dimensions);
        let masks = self.masks.prepare(dimensions, calibration)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let basis = calibration.lenses[0].orientation;
        let geometry = super::resolved_render_geometry(calibration)?;
        let first_axis = basis.inverse().rotate_vector([0.0, 0.0, 1.0]);
        let second_axis = calibration.lenses[1]
            .orientation
            .inverse()
            .rotate_vector([0.0, 0.0, 1.0]);
        let separation = first_axis
            .iter()
            .zip(second_axis)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            .clamp(-1.0, 1.0)
            .acos();
        let extra = (PI - separation).abs();
        let half_height = geometry
            .iter()
            .flatten()
            .map(|g| g.half_fov_radians() - PI * 0.5 + extra)
            .fold(0.0_f64, f64::max);
        let half_height = if geometry.iter().all(Option::is_none) {
            12_f64.to_radians()
        } else {
            half_height.max(1_f64.to_radians())
        };
        let mut plan = PreparedStitchPlan {
            mode,
            calibration: calibration.clone(),
            dimensions,
            readout: readout_key(motion),
            basis,
            half_height,
            field: flow::Field {
                width: 0,
                height: 0,
                vectors: Vec::new(),
            },
        };
        if mode == SeamMode::Fixed {
            return Ok(Arc::new(plan));
        }
        if half_height > 45_f64.to_radians() {
            return Err(Error::MissingCapability(
                "stitch optimization overlap exceeds the supported 90-degree analysis belt".into(),
            ));
        }
        if mode == SeamMode::Ai {
            #[cfg(feature = "ai-stitching")]
            {
                if self.ai_model.is_none() {
                    self.ai_model = Some(crate::seam_ai::SeamModel::new()?);
                }
                plan.field = super::ai::prepare(
                    self.ai_model.as_mut().expect("initialized model"),
                    sources,
                    calibration,
                    motion,
                    &masks,
                    basis,
                    half_height,
                    cancel,
                )?;
                prepare_render_displacements(&mut plan.field);
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                return Ok(Arc::new(plan));
            }
            #[cfg(not(feature = "ai-stitching"))]
            return Err(Error::MissingCapability(
                mode.unavailable_reason().expect("disabled feature").into(),
            ));
        }
        let width = if mode == SeamMode::Dynamic {
            1024
        } else {
            2048
        };
        let height = ((2.0 * half_height / TAU * width as f64).ceil() as usize)
            .max(32)
            .next_multiple_of(8);
        let images: [flow::Image; 2] = std::array::from_fn(|index| {
            let mut image = flow::Image {
                width,
                height,
                pixels: vec![0.0; width * height],
                valid: vec![false; width * height],
            };
            image
                .pixels
                .par_iter_mut()
                .zip(image.valid.par_iter_mut())
                .enumerate()
                .for_each(|(i, (pixel, valid))| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let phi = (i % width) as f64 / width as f64 * TAU + PI / width as f64;
                    let theta = PI * 0.5
                        + (((i / width) as f64 + 0.5) / height as f64 * 2.0 - 1.0) * half_height;
                    let direction = basis.inverse().rotate_vector([
                        theta.sin() * phi.cos(),
                        theta.sin() * phi.sin(),
                        theta.cos(),
                    ]);
                    if let Some(sample) = super::project_and_sample(
                        &sources[index],
                        &calibration.lenses[index],
                        index,
                        direction,
                        0.08,
                        masks[index].as_ref(),
                        geometry[index],
                        motion.readout()[index].as_ref(),
                    ) {
                        *pixel = ((sample.color[0] * 0.2126
                            + sample.color[1] * 0.7152
                            + sample.color[2] * 0.0722)
                            / 255.0) as f32;
                        *valid = sample.illumination_weight > 0.01;
                    }
                });
            image
        });
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        plan.field = flow::correspond(
            &images[0],
            &images[1],
            mode == SeamMode::OpticalFlow,
            cancel,
        );
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        prepare_render_displacements(&mut plan.field);
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        Ok(Arc::new(plan))
    }

    /// Repair only RGB pixels whose chroma reconstruction can touch excluded
    /// source support. Keep the SIMD decoder conversion for the image interior.
    pub fn repair_rgb_support(
        &mut self,
        rgb: &mut LensFrame,
        source: StitchSource<'_>,
        calibration: &ResolvedCalibration,
        lens_index: usize,
    ) -> Result<()> {
        source.validate()?;
        if lens_index >= 2 || source.dimensions() != (rgb.width(), rgb.height()) {
            return Err(Error::InvalidMedia(
                "RGB support repair source dimensions or lens index mismatch".into(),
            ));
        }
        if matches!(source, StitchSource::Rgb(_)) {
            return Ok(());
        }
        let dimensions = [source.dimensions(); 2];
        let masks = self.masks.prepare(dimensions, calibration)?;
        if self
            .boundary
            .as_ref()
            .is_none_or(|(previous, _)| !Arc::ptr_eq(previous, &masks))
        {
            let lists = std::array::from_fn(|index| {
                let Some(mask) = &masks[index] else {
                    return Vec::new();
                };
                support_boundary(mask)
            });
            self.boundary = Some((Arc::clone(&masks), lists));
        }
        let Some(mask) = masks[lens_index].as_ref() else {
            return Ok(());
        };
        let boundary = &self
            .boundary
            .as_ref()
            .expect("boundary cache prepared above")
            .1[lens_index];
        // Cached indices are in row order. Split mutable rows so only boundary
        // pixels are reconstructed in parallel, without a full-image scan or
        // another RGB allocation on each frame.
        rgb.rgb
            .par_chunks_mut(mask.width * 3)
            .enumerate()
            .for_each(|(y, row)| {
                let row_start = y * mask.width;
                let first = boundary.partition_point(|&i| i < row_start);
                for &i in boundary[first..]
                    .iter()
                    .take_while(|&&i| i < row_start + mask.width)
                {
                    let x = i - row_start;
                    let color = source.sample(x as f64, y as f64, Some(mask));
                    for (target, value) in row[x * 3..x * 3 + 3].iter_mut().zip(color) {
                        *target = value.round().clamp(0.0, 255.0) as u8;
                    }
                }
            });
        Ok(())
    }
}

/// Rendering interpolates final displacements, not confidence-normalized flow.
/// First/last rows are zero, joining the identity outside this latitude domain.
fn sample_displacement(field: &flow::Field, x: f32, y: f32) -> [f32; 2] {
    if !x.is_finite() || !y.is_finite() || y < 0.0 || y > (field.height - 1) as f32 {
        return [0.0; 2];
    }
    let x = x.rem_euclid(field.width as f32);
    let low = [x.floor() as usize % field.width, y.floor() as usize];
    let high = [
        (low[0] + 1) % field.width,
        (low[1] + 1).min(field.height - 1),
    ];
    let fraction = [x - x.floor(), y - y.floor()];
    let mut result = [0.0; 2];
    for (yy, wy) in [(low[1], 1.0 - fraction[1]), (high[1], fraction[1])] {
        for (xx, wx) in [(low[0], 1.0 - fraction[0]), (high[0], fraction[0])] {
            let value = field.vectors[yy * field.width + xx];
            let weight = wx * wy;
            result[0] += value.dx * weight;
            result[1] += value.dy * weight;
        }
    }
    result
}

const MIN_RENDER_JACOBIAN: f64 = 0.25;

// Derivatives [du/dx, du/dy, dv/dx, dv/dy] at a bilinear cell's corners.
// The determinant of I + D is affine inside a bilinear cell (the xy terms
// cancel), so its minimum occurs at a corner. Include the periodic azimuth cell.
fn cell_derivatives(field: &flow::Field, index: usize) -> [[f64; 4]; 4] {
    let x = index % field.width;
    let y = index / field.width;
    let right = (x + 1) % field.width;
    let values = [
        field.vectors[y * field.width + x],
        field.vectors[y * field.width + right],
        field.vectors[(y + 1) * field.width + x],
        field.vectors[(y + 1) * field.width + right],
    ];
    let difference = |a: usize, b: usize| {
        [
            f64::from(values[a].dx) - f64::from(values[b].dx),
            f64::from(values[a].dy) - f64::from(values[b].dy),
        ]
    };
    let top = difference(1, 0);
    let bottom = difference(3, 2);
    let left = difference(2, 0);
    let right = difference(3, 1);
    [[top, left], [top, right], [bottom, left], [bottom, right]]
        .map(|[x, y]| [x[0], y[0], x[1], y[1]])
}

fn determinant([ux, uy, vx, vy]: [f64; 4], scale: f64) -> f64 {
    (1.0 + scale * ux) * (1.0 + scale * vy) - scale * uy * scale * vx
}

fn render_map_is_safe(field: &flow::Field) -> bool {
    (0..field.width * (field.height - 1))
        .into_par_iter()
        .all(|i| {
            cell_derivatives(field, i).into_iter().all(|d| {
                let value = determinant(d, 1.0);
                value.is_finite() && value >= MIN_RENDER_JACOBIAN
            })
        })
}

// Find the end of the safe interval connected to identity. det(I+sD) is a
// quadratic; if convex its stationary point is the only possible interior
// minimum. Bisection then stays before the first unsafe crossing, including
// matrices that become positive again after an intervening fold.
fn identity_scale(derivative: [f64; 4]) -> f64 {
    // Leave a margin for storing the final scaled vertices in float32.
    scale_for_minimum_jacobian(derivative, MIN_RENDER_JACOBIAN + 0.0001)
}

fn scale_for_minimum_jacobian(derivative: [f64; 4], target: f64) -> f64 {
    let [ux, uy, vx, vy] = derivative;
    let quadratic = ux * vy - uy * vx;
    let trace = ux + vy;
    let minimum_at = if quadratic > 0.0 {
        (-trace / (2.0 * quadratic)).clamp(0.0, 1.0)
    } else {
        1.0
    };
    if determinant(derivative, minimum_at) >= target {
        return 1.0;
    }
    let (mut low, mut high) = (0.0, minimum_at);
    for _ in 0..32 {
        let middle = (low + high) * 0.5;
        if determinant(derivative, middle) >= target {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

fn prepare_render_displacements(field: &mut flow::Field) -> f64 {
    field.vectors.par_iter_mut().enumerate().for_each(|(i, v)| {
        let y = i / field.width;
        if !v.dx.is_finite()
            || !v.dy.is_finite()
            || !v.confidence.is_finite()
            || v.confidence < 0.5
            || v.dx.abs().max(v.dy.abs()) > 32.0
            || y == 0
            || y + 1 == field.height
        {
            *v = flow::Vector::default();
            return;
        }
        let latitude = (y as f64 + 0.5) / field.height as f64 * 2.0 - 1.0;
        let taper = super::smoothstep(((1.0 - latitude.abs()) * 4.0).clamp(0.0, 1.0));
        v.dx = (f64::from(v.dx) * taper) as f32;
        v.dy = (f64::from(v.dy) * taper) as f32;
    });
    if render_map_is_safe(field) {
        return 1.0;
    }
    // Contract only unsafe cells toward their mean displacement, preserving
    // translation while reducing derivatives. Each Jacobi pass gathers at most
    // four immutable cell requests per vertex. Rejected vertices stay pinned at
    // zero; safe distant translations are untouched. A stronger local target
    // leaves room for merging requests and pinning, which require revalidation.
    let mut requests = vec![[1.0, 0.0, 0.0]; field.width * (field.height - 1)];
    for _ in 0..32 {
        requests
            .par_iter_mut()
            .enumerate()
            .for_each(|(i, request)| {
                let derivatives = cell_derivatives(field, i);
                if derivatives
                    .iter()
                    .all(|&d| determinant(d, 1.0) >= MIN_RENDER_JACOBIAN)
                {
                    *request = [1.0, 0.0, 0.0];
                    return;
                }
                let scale = derivatives
                    .into_iter()
                    .map(|d| scale_for_minimum_jacobian(d, 0.5))
                    .fold(1.0, f64::min);
                let right = i / field.width * field.width + (i % field.width + 1) % field.width;
                let vertices =
                    [i, right, i + field.width, right + field.width].map(|j| field.vectors[j]);
                // All pinned vertices are zero. Contract toward their fixed value
                // so a cell bordering fallback can satisfy the requested bound.
                let mean = if vertices.iter().any(|v| v.confidence < 0.5) {
                    [0.0; 2]
                } else {
                    vertices.into_iter().fold([0.0; 2], |sum, v| {
                        [
                            sum[0] + f64::from(v.dx) * 0.25,
                            sum[1] + f64::from(v.dy) * 0.25,
                        ]
                    })
                };
                *request = [scale, mean[0], mean[1]];
            });
        if requests.iter().all(|request| request[0] == 1.0) {
            return 1.0;
        }
        field.vectors.par_iter_mut().enumerate().for_each(|(i, v)| {
            if v.confidence < 0.5 {
                return;
            }
            let x = i % field.width;
            let y = i / field.width;
            let left = (x + field.width - 1) % field.width;
            let mut sum = [0.0; 2];
            let mut count = 0;
            for row in [y.checked_sub(1), (y + 1 < field.height).then_some(y)]
                .into_iter()
                .flatten()
            {
                for column in [left, x] {
                    let [scale, mx, my] = requests[row * field.width + column];
                    if scale < 1.0 {
                        sum[0] += mx + scale * (f64::from(v.dx) - mx);
                        sum[1] += my + scale * (f64::from(v.dy) - my);
                        count += 1;
                    }
                }
            }
            if count > 0 {
                // Attenuate existing components only. Allowing an incident
                // request to increase a component can send an unsafe gradient
                // back and forth between otherwise settled neighboring cells.
                v.dx = (sum[0] / f64::from(count))
                    .clamp(f64::from(v.dx).min(0.0), f64::from(v.dx).max(0.0))
                    as f32;
                v.dy = (sum[1] / f64::from(count))
                    .clamp(f64::from(v.dy).min(0.0), f64::from(v.dy).max(0.0))
                    as f32;
            }
        });
    }
    if render_map_is_safe(field) {
        return 1.0;
    }
    // Different incident requests can create another unsafe cell. Bounded local
    // repair is not itself a proof; a conservative final scale certifies any
    // unresolved field after the fixed pass budget, followed by quantized checks.
    let scale = (0..field.width * (field.height - 1))
        .into_par_iter()
        .map(|i| {
            cell_derivatives(field, i)
                .into_iter()
                .map(identity_scale)
                .fold(1.0, f64::min)
        })
        .reduce(|| 1.0, f64::min);
    field.vectors.par_iter_mut().for_each(|v| {
        v.dx = (f64::from(v.dx) * scale) as f32;
        v.dy = (f64::from(v.dy) * scale) as f32;
    });
    // Verify the actual quantized vertices, not only the unrounded derivation.
    // Unexpected numerical failure has a bounded, exact calibrated fallback.
    if !render_map_is_safe(field) {
        field.vectors.fill(flow::Vector::default());
        return 0.0;
    }
    scale
}

fn support_boundary(mask: &super::mask::FisheyeMask) -> Vec<usize> {
    // Separable seven-pixel erosion in O(width*height), once per source mask.
    let (width, height) = (mask.width, mask.height);
    let mut horizontal = vec![false; width * height];
    for y in 0..height {
        let mut zeros = (0..4.min(width))
            .filter(|&x| mask.pixel_weight(x, y) <= 0.0)
            .count();
        for x in 0..width {
            horizontal[y * width + x] = zeros > 0;
            if x >= 3 && mask.pixel_weight(x - 3, y) <= 0.0 {
                zeros -= 1;
            }
            if x + 4 < width && mask.pixel_weight(x + 4, y) <= 0.0 {
                zeros += 1;
            }
        }
    }
    let mut counts = vec![0_usize; width];
    for y in 0..4.min(height) {
        for x in 0..width {
            counts[x] += usize::from(horizontal[y * width + x]);
        }
    }
    let mut boundary = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if counts[x] > 0 && mask.pixel_weight(x, y) > 0.0 {
                boundary.push(y * width + x);
            }
            if y >= 3 {
                counts[x] -= usize::from(horizontal[(y - 3) * width + x]);
            }
            if y + 4 < height {
                counts[x] += usize::from(horizontal[(y + 4) * width + x]);
            }
        }
    }
    boundary
}

#[cfg(test)]
mod tests {
    use super::*;

    mod correspondence {
        // Share the independent forward-projected chart with integration tests.
        use crate as insta360_rs;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/common/correspondence.rs"
        ));
    }

    #[test]
    fn interactive_workers_are_shared_bounded_and_inherited_by_nested_rayon_work() {
        let pool = interactive_pool().unwrap();
        assert!(std::ptr::eq(pool, interactive_pool().unwrap()));
        let available = std::thread::available_parallelism().map_or(1, |count| count.get());
        assert_eq!(pool.current_num_threads(), available.min(4));
        let observe = || {
            assert!(std::thread::current()
                .name()
                .unwrap()
                .starts_with("insv-stitch-preview-"));
            rayon::current_num_threads()
        };
        assert_eq!(
            pool.install(|| rayon::join(observe, observe)),
            (available.min(4), available.min(4))
        );
    }

    #[test]
    fn interactive_preparation_preserves_all_mode_fields_and_rendered_pixels() {
        use crate::{CpuStitcher, EquirectangularProjection};
        let (lenses, calibration) = correspondence::chart(0.8);
        let motion = FrameMotion::global(Orientation::IDENTITY).unwrap();
        let projection = EquirectangularProjection {
            width: 256,
            height: 128,
        };
        let stitcher = CpuStitcher::new();
        let mut global = StitchPlanner::new();
        let mut interactive = StitchPlanner::new();
        interactive.set_interactive(true);
        for mode in [
            SeamMode::Dynamic,
            SeamMode::OpticalFlow,
            #[cfg(feature = "ai-stitching")]
            SeamMode::Ai,
        ] {
            let expected = global
                .prepare(&lenses, &calibration, &motion, mode)
                .unwrap();
            let actual = interactive
                .prepare(&lenses, &calibration, &motion, mode)
                .unwrap();
            assert!(
                expected.confidence_coverage() > 0.05,
                "chart must exercise accepted {mode:?} corrections"
            );
            assert_eq!(
                (actual.field.width, actual.field.height),
                (expected.field.width, expected.field.height)
            );
            assert_eq!(actual.half_height.to_bits(), expected.half_height.to_bits());
            for (actual, expected) in actual.field.vectors.iter().zip(&expected.field.vectors) {
                assert_eq!(
                    [actual.dx, actual.dy, actual.confidence].map(f32::to_bits),
                    [expected.dx, expected.dy, expected.confidence].map(f32::to_bits),
                    "interactive scheduling changed a {mode:?} correction",
                );
            }
            let render = |plan: &PreparedStitchPlan| {
                stitcher
                    .stitch_with_motion_and_plan(
                        &lenses,
                        &calibration,
                        projection,
                        &motion,
                        Some(plan),
                    )
                    .unwrap()
            };
            assert_eq!(
                render(&actual),
                render(&expected),
                "interactive scheduling changed {mode:?} pixels"
            );
        }
        assert!(matches!(
            interactive.prepare_sources_controlled(
                &lenses.each_ref().map(StitchSource::Rgb),
                &calibration,
                &motion,
                SeamMode::Dynamic,
                &AtomicBool::new(true),
            ),
            Err(Error::Cancelled)
        ));
    }

    fn constant_field(width: usize, height: usize, dx: f32, dy: f32) -> flow::Field {
        flow::Field {
            width,
            height,
            vectors: vec![
                flow::Vector {
                    dx,
                    dy,
                    confidence: 1.0
                };
                width * height
            ],
        }
    }

    fn test_plan(field: flow::Field) -> PreparedStitchPlan {
        PreparedStitchPlan {
            mode: SeamMode::Dynamic,
            calibration: crate::calibration::synthetic_dual_fisheye_calibration(16, 16).unwrap(),
            dimensions: [(16, 16); 2],
            readout: Vec::new(),
            basis: Orientation::IDENTITY,
            half_height: 12_f64.to_radians(),
            field,
        }
    }

    fn rendered_grid_point(plan: &PreparedStitchPlan, x: f64, y: f64) -> [f64; 2] {
        let phi = (x + 0.5) / plan.field.width as f64 * TAU;
        let theta =
            PI * 0.5 + ((y + 0.5) / plan.field.height as f64 * 2.0 - 1.0) * plan.half_height;
        let ray = plan.direction(
            1,
            [
                theta.sin() * phi.cos(),
                theta.sin() * phi.sin(),
                theta.cos(),
            ],
        );
        let change = (ray[1].atan2(ray[0]) - phi + PI).rem_euclid(TAU) - PI;
        [
            x + change / TAU * plan.field.width as f64,
            ((ray[0].hypot(ray[1]).atan2(ray[2]) - PI * 0.5) / plan.half_height + 1.0)
                * 0.5
                * plan.field.height as f64
                - 0.5,
        ]
    }

    fn assert_positive_bilinear_map(field: &flow::Field) {
        // Independent source-coordinate interpolation: do not call the
        // production derivative/determinant helpers. Include the azimuth cut.
        for y in 0..field.height - 1 {
            for x in 0..field.width {
                let positions = [
                    (x, y),
                    ((x + 1) % field.width, y),
                    (x, y + 1),
                    ((x + 1) % field.width, y + 1),
                ];
                let vertices: [[f64; 2]; 4] = std::array::from_fn(|i| {
                    let v = field.vectors[positions[i].1 * field.width + positions[i].0];
                    [
                        (i % 2) as f64 + f64::from(v.dx),
                        (i / 2) as f64 + f64::from(v.dy),
                    ]
                });
                let point = |u: f64, v: f64| {
                    let weights = [(1.0 - u) * (1.0 - v), u * (1.0 - v), (1.0 - u) * v, u * v];
                    std::array::from_fn::<_, 2, _>(|axis| {
                        vertices
                            .iter()
                            .zip(weights)
                            .map(|(p, w)| p[axis] * w)
                            .sum::<f64>()
                    })
                };
                for u in [0.0, 0.25, 0.5, 0.75, 1.0] {
                    for v in [0.0, 0.25, 0.5, 0.75, 1.0] {
                        let [left, right, top, bottom] =
                            [point(0.0, v), point(1.0, v), point(u, 0.0), point(u, 1.0)];
                        let jacobian = (right[0] - left[0]) * (bottom[1] - top[1])
                            - (bottom[0] - top[0]) * (right[1] - left[1]);
                        assert!(
                            jacobian >= 0.25 - 1e-12,
                            "cell {x},{y} at {u},{v}: {jacobian}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn final_latitude_taper_cannot_fold_a_constant_vertical_proposal() {
        // Raw dy=6 has Jacobian1. The old post-sampling taper maps rows58,59
        // to62.6083984375,62.5595703125: an actual reversed interval.
        let mut field = constant_field(16, 64, 0.0, 6.0);
        let scale = prepare_render_displacements(&mut field);
        assert!(scale > 0.0 && scale <= 1.0);
        assert_positive_bilinear_map(&field);
        let plan = test_plan(field);
        let first = rendered_grid_point(&plan, 4.0, 58.0);
        let second = rendered_grid_point(&plan, 4.0, 59.0);
        assert!(second[1] - first[1] >= 0.2499);
        for y in [-0.5, 0.0, 63.0, 63.5] {
            let actual = rendered_grid_point(&plan, 4.0, y);
            assert!(
                (actual[1] - y).abs() < 1e-10,
                "boundary must join calibrated identity"
            );
        }
    }

    #[test]
    fn rejected_confidence_and_wrapped_azimuth_join_calibrated_fallback_continuously() {
        let mut field = constant_field(16, 32, 6.0, -2.0);
        for y in 0..field.height {
            field.vectors[y * field.width].confidence = 0.49;
            field.vectors[y * field.width + 7].confidence = 0.0;
        }
        let scale = prepare_render_displacements(&mut field);
        assert!(scale > 0.0 && scale <= 1.0);
        assert_positive_bilinear_map(&field);
        let plan = test_plan(field);
        for x in [0.0, 7.0, 16.0] {
            let point = rendered_grid_point(&plan, x, 16.0);
            assert!((point[0] - x).abs() < 1e-10);
            assert!((point[1] - 16.0).abs() < 1e-10);
        }
        // The former normalized-confidence gate jumped at this position.
        let cutoff = (0.5 - 0.49) / (1.0 - 0.49);
        for x in [-0.00001, 0.0, cutoff, 7.0, 15.99999] {
            let left = rendered_grid_point(&plan, x - 0.00001, 16.0);
            let right = rendered_grid_point(&plan, x + 0.00001, 16.0);
            assert!((right[0] - left[0]).hypot(right[1] - left[1]) < 0.001);
        }
    }

    #[test]
    fn safe_horizontal_translation_keeps_its_full_magnitude() {
        let mut field = constant_field(16, 32, 1.25, 0.0);
        assert_eq!(prepare_render_displacements(&mut field), 1.0);
        assert_positive_bilinear_map(&field);
        let plan = test_plan(field);
        let point = rendered_grid_point(&plan, 15.5, 16.0);
        assert!((point[0] - 16.75).abs() < 1e-10);
        assert!((point[1] - 16.0).abs() < 1e-10);
    }

    #[test]
    fn unsafe_confidence_boundary_does_not_attenuate_a_distant_safe_translation() {
        let mut field = constant_field(128, 64, 3.0, 0.0);
        for y in 0..field.height {
            field.vectors[y * field.width + 8].confidence = 0.0;
        }
        assert_eq!(
            prepare_render_displacements(&mut field),
            1.0,
            "local repair must resolve this field without global attenuation"
        );
        assert_positive_bilinear_map(&field);
        let distant = field.vectors[32 * field.width + 80];
        assert_eq!([distant.dx, distant.dy], [3.0, 0.0]);
        let plan = test_plan(field);
        let point = rendered_grid_point(&plan, 80.0, 32.0);
        assert!((point[0] - 83.0).abs() < 1e-10);
    }

    #[test]
    fn safety_scale_stops_before_an_intermediate_fold_and_rejects_invalid_proposals() {
        // Both ends have positive determinant, but scaling through s=0.5 folds.
        let derivative = [-2.0, 0.0, 0.0, -2.0];
        let scale = identity_scale(derivative);
        assert!(scale > 0.0 && scale < 0.25);
        for i in 0..=100 {
            let value = (1.0 - 2.0 * scale * i as f64 / 100.0).powi(2);
            assert!(value >= 0.25);
        }
        let mut field = constant_field(8, 16, 0.0, 0.0);
        field.vectors[45].dx = f32::NAN;
        field.vectors[46].dy = 33.0;
        field.vectors[47].confidence = f32::INFINITY;
        prepare_render_displacements(&mut field);
        for i in [45, 46, 47] {
            let v = field.vectors[i];
            assert_eq!([v.dx, v.dy, v.confidence], [0.0; 3]);
        }
        assert_positive_bilinear_map(&field);
    }

    #[cfg(feature = "media")]
    #[test]
    fn analysis_and_renderer_reuse_one_mask_allocation_and_invalidation_key() {
        let cpu = super::super::CpuStitcher::new();
        let mut planner = StitchPlanner::with_masks(cpu.mask_cache());
        let motion = FrameMotion::global(Orientation::IDENTITY).unwrap();
        let mut previous = None;
        for size in [16, 32, 16] {
            let calibration =
                crate::calibration::synthetic_dual_fisheye_calibration(size, size).unwrap();
            let lenses = std::array::from_fn(|_| {
                LensFrame::new(size, size, vec![128; (size * size * 3) as usize]).unwrap()
            });
            planner
                .prepare(&lenses, &calibration, &motion, SeamMode::Fixed)
                .unwrap();
            let analysis = planner
                .masks
                .prepare([(size, size); 2], &calibration)
                .unwrap();
            super::super::StitchEngine::stitch(
                &cpu,
                &lenses,
                &calibration,
                crate::EquirectangularProjection {
                    width: size * 2,
                    height: size,
                },
            )
            .unwrap();
            let rendered = cpu
                .mask_cache()
                .prepare([(size, size); 2], &calibration)
                .unwrap();
            assert!(
                Arc::ptr_eq(&analysis, &rendered),
                "render rebuilt the analysis masks"
            );
            if let Some(previous) = previous {
                assert!(
                    !Arc::ptr_eq(&previous, &rendered),
                    "geometry change reused stale masks"
                );
            }
            previous = Some(rendered);
        }
        // Backend fallback carries the same allocation owner into the new CPU
        // renderer instead of resetting analysis/model/boundary preparation.
        let fallback = super::super::CpuStitcher::with_masks(cpu.mask_cache());
        assert!(Arc::ptr_eq(&fallback.mask_cache(), &planner.masks));
    }

    #[test]
    fn linear_boundary_cache_matches_independent_square_footprint() {
        for width in 1..19 {
            for height in 1..13 {
                let mask = super::super::mask::FisheyeMask {
                    width,
                    height,
                    weights: (0..width * height)
                        .map(|i| {
                            if (i * 31 + i / width * 11) % 17 < 2 {
                                0.0
                            } else {
                                0.5
                            }
                        })
                        .collect(),
                };
                let expected: Vec<_> = (0..width * height)
                    .filter(|&i| {
                        let (x, y) = (i % width, i / width);
                        mask.weights[i] > 0.0
                            && (y.saturating_sub(3)..=(y + 3).min(height - 1)).any(|yy| {
                                (x.saturating_sub(3)..=(x + 3).min(width - 1))
                                    .any(|xx| mask.weights[yy * width + xx] == 0.0)
                            })
                    })
                    .collect();
                assert_eq!(support_boundary(&mask), expected, "{width}x{height}");
            }
        }
    }

    #[test]
    fn cancellation_precedes_mask_or_model_preparation() {
        let lenses = std::array::from_fn(|_| LensFrame::new(2, 2, vec![128; 12]).unwrap());
        let calibration = crate::calibration::synthetic_dual_fisheye_calibration(2, 2).unwrap();
        let motion = FrameMotion::global(Orientation::IDENTITY).unwrap();
        let mut planner = StitchPlanner::new();
        for mode in [
            SeamMode::Fixed,
            SeamMode::Dynamic,
            SeamMode::OpticalFlow,
            SeamMode::Ai,
        ] {
            assert!(matches!(
                planner.prepare_sources_controlled(
                    &lenses.each_ref().map(StitchSource::Rgb),
                    &calibration,
                    &motion,
                    mode,
                    &AtomicBool::new(true),
                ),
                Err(Error::Cancelled)
            ));
        }
    }
}
