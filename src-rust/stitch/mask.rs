//! Calibrated housing masks prepared once, before either renderer samples pixels.

use std::sync::{Arc, Mutex};

use super::{
    calibration_to_source_dimensions, interpolate_mask_radius_squared, mask_recipe,
    project_camera_ray_with_clipping, resolved_render_geometry,
};
use crate::profile::{MaskBoundaryInterpolation, RadialMaskRecipe};
use crate::{Error, ParsedLens, ResolvedCalibration, ResolvedLensGeometry, Result};

// Two full-resolution masks plus the temporary raster/distance workspace remain bounded.
const MAX_MASK_PIXELS: usize = 64 * 1024 * 1024;
const MAX_BOUNDARY_POINTS: usize = 1024;

#[derive(Debug)]
pub(crate) struct FisheyeMask {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) weights: Vec<f32>,
}

impl FisheyeMask {
    pub(super) fn pixel_weight(&self, x: usize, y: usize) -> f64 {
        f64::from(self.weights[y * self.width + x])
    }

    pub(super) fn weight(&self, x: f64, y: f64) -> f64 {
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x > (self.width - 1) as f64
            || y > (self.height - 1) as f64
        {
            return 0.0;
        }
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = x.ceil() as usize;
        let y1 = y.ceil() as usize;
        let dx = x - x0 as f64;
        let dy = y - y0 as f64;
        let top = self.pixel_weight(x0, y0) * (1.0 - dx) + self.pixel_weight(x1, y0) * dx;
        let bottom = self.pixel_weight(x0, y1) * (1.0 - dx) + self.pixel_weight(x1, y1) * dx;
        top * (1.0 - dy) + bottom * dy
    }
}

pub(crate) type PreparedMasks = [Option<FisheyeMask>; 2];

#[derive(Clone, Debug, PartialEq)]
struct MaskKey {
    dimensions: [(u32, u32); 2],
    lenses: [ParsedLens; 2],
    geometry: [Option<ResolvedLensGeometry>; 2],
    recipes: [Option<RadialMaskRecipe>; 2],
}

/// Only the current geometry is retained. Cloned CPU stitchers share this cache.
#[derive(Debug, Default)]
pub(crate) struct MaskCache(Mutex<Option<(MaskKey, Arc<PreparedMasks>)>>);

impl MaskCache {
    pub(crate) fn prepare(
        &self,
        dimensions: [(u32, u32); 2],
        calibration: &ResolvedCalibration,
    ) -> Result<Arc<PreparedMasks>> {
        let key = MaskKey {
            dimensions,
            lenses: calibration.lenses.clone(),
            geometry: resolved_render_geometry(calibration)?,
            recipes: std::array::from_fn(|index| mask_recipe(calibration, index)),
        };
        let mut cache = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((previous, masks)) = cache.as_ref() {
            if previous == &key {
                return Ok(Arc::clone(masks));
            }
        }
        let total = dimensions.iter().zip(key.recipes).try_fold(
            0_usize,
            |total, (&(width, height), recipe)| {
                if recipe.is_none() {
                    return Ok(total);
                }
                let pixels = mask_len(width as usize, height as usize)?;
                total
                    .checked_add(pixels)
                    .filter(|value| *value <= MAX_MASK_PIXELS)
                    .ok_or_else(|| invalid("combined source masks exceed the 64-megapixel limit"))
            },
        )?;
        let _ = total;
        // Drop our old entry before allocating another resolution. In-flight
        // calls own their Arc; one render cannot invalidate another's masks.
        *cache = None;
        let build = |index| {
            build_mask(
                dimensions[index],
                &key.lenses[index],
                index,
                key.recipes[index],
                key.geometry[index],
            )
        };
        let masks = if key.recipes.iter().all(Option::is_some) {
            // Keep one preparation per shared cache. A Rayon join here could
            // steal another caller that waits for this same mutex and deadlock.
            // One scoped helper has a non-stealing wait and bounded scratch.
            std::thread::scope(|scope| -> Result<_> {
                match std::thread::Builder::new()
                    .name("insta360-mask".into())
                    .spawn_scoped(scope, || build(1))
                {
                    Ok(worker) => {
                        let first = build(0);
                        let second = worker
                            .join()
                            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                        // Join even after a first-lens error before returning.
                        Ok([first?, second?])
                    }
                    Err(_) => Ok([build(0)?, build(1)?]),
                }
            })?
        } else {
            // Bare and single-mask inputs need no helper thread.
            [build(0)?, build(1)?]
        };
        let masks = Arc::new(masks);
        *cache = Some((key, Arc::clone(&masks)));
        Ok(masks)
    }
}

fn mask_len(width: usize, height: usize) -> Result<usize> {
    width
        .checked_mul(height)
        .filter(|size| width > 0 && height > 0 && *size <= MAX_MASK_PIXELS)
        .ok_or_else(|| invalid("source mask dimensions are empty or exceed the 64-megapixel limit"))
}

fn filled<T: Clone>(size: usize, value: T) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(size)
        .map_err(|_| invalid("cannot allocate the source mask workspace"))?;
    values.resize(size, value);
    Ok(values)
}

fn build_mask(
    dimensions: (u32, u32),
    lens: &ParsedLens,
    lens_index: usize,
    recipe: Option<RadialMaskRecipe>,
    geometry: Option<ResolvedLensGeometry>,
) -> Result<Option<FisheyeMask>> {
    let Some(recipe) = recipe else {
        return Ok(None);
    };
    let geometry =
        geometry.ok_or_else(|| invalid("housing mask requires resolved lens geometry"))?;
    let points = recipe.lower_hemisphere_boundary;
    if points.is_empty()
        || points.len() > MAX_BOUNDARY_POINTS
        || !recipe.feather_weight_per_pixel.is_finite()
        || recipe.feather_weight_per_pixel <= 0.0
        || points.iter().any(|point| {
            !point.azimuth_degrees.is_finite()
                || !(0.0..=90.0).contains(&point.azimuth_degrees)
                || !point.half_fov_degrees.is_finite()
                || !(0.0..180.0).contains(&point.half_fov_degrees)
        })
        || points
            .windows(2)
            .any(|pair| pair[0].azimuth_degrees >= pair[1].azimuth_degrees)
    {
        return Err(invalid(
            "housing mask requires 1..=1024 ordered finite angular boundaries and a positive feather scale",
        ));
    }
    let center = calibration_to_source_dimensions(dimensions, lens, lens_index, lens.cx, lens.cy);
    let project_radius = |azimuth_degrees: f64, half_fov_degrees: f64| -> Result<f64> {
        let theta = half_fov_degrees.to_radians();
        let azimuth = azimuth_degrees.to_radians();
        let ray = [
            theta.sin() * azimuth.sin(),
            theta.sin() * azimuth.cos(),
            theta.cos(),
        ];
        let (x, y) = project_camera_ray_with_clipping(lens, ray, Some(geometry), false)
            .ok_or_else(|| invalid("housing boundary cannot be projected by this lens"))?;
        let source = calibration_to_source_dimensions(dimensions, lens, lens_index, x, y);
        let radius = (source.0 - center.0).powi(2) + (source.1 - center.1).powi(2);
        if !radius.is_finite() || radius < 0.0 {
            return Err(invalid("housing mask radius is non-finite"));
        }
        Ok(radius)
    };
    let boundaries = points
        .iter()
        .map(|point| {
            Ok((
                point.azimuth_degrees,
                project_radius(point.azimuth_degrees, point.half_fov_degrees)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    // Native 0x16d601c..28 caps the circle FOV by the last recipe point and
    // the requested FOV, then projects that circle on the zero-azimuth axis.
    let outer_angle = points
        .last()
        .expect("nonempty recipe")
        .half_fov_degrees
        .min(geometry.full_fov_degrees * 0.5);
    let outer = project_radius(0.0, outer_angle)?;
    let width = dimensions.0 as usize;
    let height = dimensions.1 as usize;
    mask_len(width, height)?;

    // INSCoreMedia 0x16d64cc..65c4 rasterizes a CCW-rotated image. Its
    // center is truncated in the native canvas before scaling to mask size.
    let canvas_half = f64::from(lens.canvas_width) * 0.5;
    let scale_x = f64::from(dimensions.0) / canvas_half;
    let scale_y = f64::from(dimensions.1) / f64::from(lens.canvas_height);
    let native_x = lens.cx - canvas_half * lens_index as f64;
    let rotated_center = [
        lens.cy.trunc() * scale_y,
        (canvas_half - native_x).trunc() * scale_x,
    ];
    let binary = match recipe.interpolation {
        MaskBoundaryInterpolation::ProjectedRadius => {
            raster_mask(width, height, rotated_center, outer, &boundaries)?
        }
        MaskBoundaryInterpolation::Angle => {
            raster_mask_with(width, height, rotated_center, outer, |angle| {
                let Some(pair) = points.windows(2).find(|pair| {
                    pair[0].azimuth_degrees <= angle && angle < pair[1].azimuth_degrees
                }) else {
                    return Ok(outer);
                };
                let fraction = (angle - pair[0].azimuth_degrees)
                    / (pair[1].azimuth_degrees - pair[0].azimuth_degrees);
                let half_fov = pair[0].half_fov_degrees
                    + fraction * (pair[1].half_fov_degrees - pair[0].half_fov_degrees);
                // Native Method2 projects each interpolated half-FOV. Method3
                // interpolates already projected knots; the two are nonlinear
                // and cannot share a table of interpolated pixel radii.
                project_radius(angle, half_fov)
            })?
        }
    };
    let weights = feather_mask(width, height, &binary, recipe.feather_weight_per_pixel)?;
    Ok(Some(FisheyeMask {
        width,
        height,
        weights,
    }))
}

/// Filled integer circle followed by the native angular erosion, in rotated coordinates.
fn raster_mask(
    width: usize,
    height: usize,
    center: [f64; 2],
    outer_squared: f64,
    boundary: &[(f64, f64)],
) -> Result<Vec<u8>> {
    raster_mask_with(width, height, center, outer_squared, |angle| {
        Ok(interpolate_mask_radius_squared(
            angle,
            boundary,
            outer_squared,
        ))
    })
}

fn raster_mask_with(
    width: usize,
    height: usize,
    center: [f64; 2],
    outer_squared: f64,
    radius_squared: impl Fn(f64) -> Result<f64>,
) -> Result<Vec<u8>> {
    let mut rotated = filled(mask_len(width, height)?, 0_u8)?;
    let radius = outer_squared.sqrt();
    if center
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 1.0e9)
        || !radius.is_finite()
        || radius > 1.0e6
    {
        return Err(invalid(
            "housing circle exceeds the raster coordinate limit",
        ));
    }
    fill_circle(
        &mut rotated,
        height,
        width,
        [center[0] as i64, center[1] as i64],
        radius as i64,
    );
    // Native erodes from the right edge to the image midpoint and stops at
    // the first accepted pixel. The row band is cos(20°) times the circle radius.
    let band = f64::from(f32::from_bits(0x3f708fb2)) * radius;
    let begin = (center[1] - band).max(0.0).min(width as f64) as usize;
    let end = (center[1] + band).max(0.0).min(width as f64) as usize;
    for row in begin..end {
        let dy = row as f64 - center[1];
        for column in (height / 2 + 1..height).rev() {
            let dx = column as f64 - center[0];
            let angle = dy.abs().atan2(dx.abs()).to_degrees();
            let limit = radius_squared(angle)?;
            if dx * dx + dy * dy <= limit {
                break;
            }
            rotated[row * height + column] = 0;
        }
    }
    let mut binary = filled(width * height, 0_u8)?;
    for y in 0..height {
        for x in 0..width {
            binary[y * width + x] = rotated[(width - 1 - x) * height + y];
        }
    }
    // Native 0x16d6764..6868 clears the horizontal crop's top edge only for
    // square inputs, and always clears the two vertical image boundaries.
    if width == height {
        binary[..width].fill(0);
    }
    for row in binary.chunks_exact_mut(width) {
        row[0] = 0;
        row[width - 1] = 0;
    }
    Ok(binary)
}

fn fill_circle(image: &mut [u8], width: usize, height: usize, center: [i64; 2], radius: i64) {
    // Integer midpoint-circle scan conversion, matching the non-antialiased
    // filled LINE_8 circle used by the vendor (OpenCV drawing.cpp Circle).
    let mut major = radius;
    let mut minor = 0_i64;
    let mut error = 0_i64;
    while minor <= major {
        for (row_offset, extent) in [
            (minor, major),
            (-minor, major),
            (major, minor),
            (-major, minor),
        ] {
            let row = center[1] + row_offset;
            let left = (center[0] - extent).max(0);
            let right = (center[0] + extent).min(width as i64 - 1);
            if row >= 0 && row < height as i64 && left <= right {
                image[row as usize * width + left as usize..=row as usize * width + right as usize]
                    .fill(1);
            }
        }
        minor += 1;
        error += 2 * minor - 1;
        if error > 0 {
            error -= 2 * major - 1;
            major -= 1;
        }
    }
}

/// L2/mask5 chamfer distance uses integer 16.16 costs, not radial distance.
fn feather_mask(width: usize, height: usize, binary: &[u8], multiplier: f64) -> Result<Vec<f32>> {
    let length = mask_len(width, height)?;
    if binary.len() != length {
        return Err(invalid(
            "source mask raster length disagrees with its dimensions",
        ));
    }
    // OpenCV 4.x distranform.cpp getDistanceTransformMask(52): 1, 1.4,
    // 2.1969, rounded to 16 fractional bits by distanceTransform_5x5.
    const HV: u32 = 65_536;
    const DIAGONAL: u32 = 91_750;
    const KNIGHT: u32 = 143_976;
    const FAR: u32 = u32::MAX - KNIGHT;
    const PREVIOUS: [(isize, isize, u32); 8] = [
        (-1, -2, KNIGHT),
        (1, -2, KNIGHT),
        (-2, -1, KNIGHT),
        (-1, -1, DIAGONAL),
        (0, -1, HV),
        (1, -1, DIAGONAL),
        (2, -1, KNIGHT),
        (-1, 0, HV),
    ];
    let mut distances = filled(length, FAR)?;
    for (distance, pixel) in distances.iter_mut().zip(binary) {
        if *pixel == 0 {
            *distance = 0;
        }
    }
    // The eight offsets are fixed for interior pixels. Keeping the two scans
    // separate avoids per-neighbor coordinate arithmetic and bounds branches,
    // while the border follows the same clipped stencil as the scalar path.
    // No padded image is allocated: narrow inputs retain the same memory bound.
    let border_distance = |distances: &[u32], index: usize, x: usize, y: usize, forward| {
        let mut shortest = distances[index];
        for (dx, dy, cost) in PREVIOUS {
            let (xx, yy) = if forward {
                (x as isize + dx, y as isize + dy)
            } else {
                (x as isize - dx, y as isize - dy)
            };
            if xx >= 0 && yy >= 0 && xx < width as isize && yy < height as isize {
                shortest =
                    shortest.min(distances[yy as usize * width + xx as usize].saturating_add(cost));
            }
        }
        shortest
    };
    for y in 0..height {
        let row = y * width;
        for x in 0..width {
            let index = row + x;
            if distances[index] == 0 {
                continue;
            }
            distances[index] = if y >= 2 && x >= 2 && x + 2 < width {
                let above = index - width;
                let above_two = above - width;
                distances[index]
                    .min(distances[above_two - 1].saturating_add(KNIGHT))
                    .min(distances[above_two + 1].saturating_add(KNIGHT))
                    .min(distances[above - 2].saturating_add(KNIGHT))
                    .min(distances[above - 1].saturating_add(DIAGONAL))
                    .min(distances[above].saturating_add(HV))
                    .min(distances[above + 1].saturating_add(DIAGONAL))
                    .min(distances[above + 2].saturating_add(KNIGHT))
                    .min(distances[index - 1].saturating_add(HV))
            } else {
                border_distance(&distances, index, x, y, true)
            };
        }
    }
    for y in (0..height).rev() {
        let row = y * width;
        for x in (0..width).rev() {
            let index = row + x;
            if distances[index] == 0 {
                continue;
            }
            distances[index] = if y + 2 < height && x >= 2 && x + 2 < width {
                let below = index + width;
                let below_two = below + width;
                distances[index]
                    .min(distances[below_two + 1].saturating_add(KNIGHT))
                    .min(distances[below_two - 1].saturating_add(KNIGHT))
                    .min(distances[below + 2].saturating_add(KNIGHT))
                    .min(distances[below + 1].saturating_add(DIAGONAL))
                    .min(distances[below].saturating_add(HV))
                    .min(distances[below - 1].saturating_add(DIAGONAL))
                    .min(distances[below - 2].saturating_add(KNIGHT))
                    .min(distances[index + 1].saturating_add(HV))
            } else {
                border_distance(&distances, index, x, y, false)
            };
        }
    }
    let mut weights = filled(length, 0.0_f32)?;
    for (weight, distance) in weights.iter_mut().zip(distances) {
        // distanceTransform produces f32 before convertTo applies its double alpha.
        let pixels = distance as f32 / HV as f32;
        *weight = (f64::from(pixels) * multiplier).min(1.0) as f32;
    }
    Ok(weights)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::MissingCalibration(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::synthetic_dual_fisheye_calibration;
    use crate::stitch::{low_frequency_rgb, CpuStitcher, LensFrame};

    const FEATHER: f64 = 0.243_902_444_839_477_54;

    // Retained pre-optimization scan for exact output equivalence. The separate
    // Dijkstra test below checks the metric independently of either raster scan.
    fn scalar_feather_mask(
        width: usize,
        height: usize,
        binary: &[u8],
        multiplier: f64,
    ) -> Result<Vec<f32>> {
        let length = mask_len(width, height)?;
        if binary.len() != length {
            return Err(invalid(
                "source mask raster length disagrees with its dimensions",
            ));
        }
        // OpenCV 4.x distranform.cpp getDistanceTransformMask(52): 1, 1.4,
        // 2.1969, rounded to 16 fractional bits by distanceTransform_5x5.
        const HV: u32 = 65_536;
        const DIAGONAL: u32 = 91_750;
        const KNIGHT: u32 = 143_976;
        const FAR: u32 = u32::MAX - KNIGHT;
        const PREVIOUS: [(isize, isize, u32); 8] = [
            (-1, -2, KNIGHT),
            (1, -2, KNIGHT),
            (-2, -1, KNIGHT),
            (-1, -1, DIAGONAL),
            (0, -1, HV),
            (1, -1, DIAGONAL),
            (2, -1, KNIGHT),
            (-1, 0, HV),
        ];
        let mut distances = filled(length, FAR)?;
        for (distance, pixel) in distances.iter_mut().zip(binary) {
            if *pixel == 0 {
                *distance = 0;
            }
        }
        for forward in [true, false] {
            for step in 0..length {
                let index = if forward { step } else { length - 1 - step };
                if distances[index] == 0 {
                    continue;
                }
                let x = (index % width) as isize;
                let y = (index / width) as isize;
                let mut shortest = distances[index];
                for (dx, dy, cost) in PREVIOUS {
                    let (xx, yy) = if forward {
                        (x + dx, y + dy)
                    } else {
                        (x - dx, y - dy)
                    };
                    if xx >= 0 && yy >= 0 && xx < width as isize && yy < height as isize {
                        shortest = shortest
                            .min(distances[yy as usize * width + xx as usize].saturating_add(cost));
                    }
                }
                distances[index] = shortest;
            }
        }
        let mut weights = filled(length, 0.0_f32)?;
        for (weight, distance) in weights.iter_mut().zip(distances) {
            // distanceTransform produces f32 before convertTo applies its double alpha.
            let pixels = distance as f32 / HV as f32;
            *weight = (f64::from(pixels) * multiplier).min(1.0) as f32;
        }
        Ok(weights)
    }

    #[test]
    fn optimized_feather_matches_scalar_on_rectangular_narrow_and_random_masks() {
        let mut random = 0x42f0_c391_u32;
        for (width, height) in [
            (1, 1),
            (1, 31),
            (31, 1),
            (2, 17),
            (17, 2),
            (3, 3),
            (4, 17),
            (5, 29),
            (7, 4),
            (19, 23),
            (65, 64),
            (127, 9),
        ] {
            for density in [0, 1, 3, 7, 8] {
                let binary: Vec<_> = (0..width * height)
                    .map(|_| {
                        random ^= random << 13;
                        random ^= random >> 17;
                        random ^= random << 5;
                        u8::from(random % 8 < density)
                    })
                    .collect();
                for multiplier in [FEATHER, 0.01, 0.75, 1.0] {
                    assert_eq!(
                        feather_mask(width, height, &binary, multiplier).unwrap(),
                        scalar_feather_mask(width, height, &binary, multiplier).unwrap(),
                        "{width}x{height}, density {density}, multiplier {multiplier}",
                    );
                }
            }
        }
    }

    #[test]
    fn integer_circle_has_independent_scanline_golden() {
        let mut binary = vec![0; 7 * 7];
        fill_circle(&mut binary, 7, 7, [3, 3], 3);
        let rows = [
            "0001000", "0111110", "0111110", "1111111", "0111110", "0111110", "0001000",
        ];
        for (actual, expected) in binary.chunks_exact(7).zip(rows) {
            assert_eq!(
                actual,
                expected
                    .as_bytes()
                    .iter()
                    .map(|pixel| pixel - b'0')
                    .collect::<Vec<_>>()
            );
        }
        // A radius-zero circle remains one raster pixel, not an empty region.
        binary.fill(0);
        fill_circle(&mut binary, 7, 7, [0, 0], 0);
        assert_eq!(
            binary
                .iter()
                .map(|value| usize::from(*value))
                .sum::<usize>(),
            1
        );
    }

    #[test]
    fn chamfer_metric_matches_known_axial_diagonal_and_knight_distances() {
        let mut binary = vec![1; 11 * 11];
        binary[5 * 11 + 5] = 0;
        let distances = feather_mask(11, 11, &binary, 0.1).unwrap();
        for (dx, dy, fixed) in [
            (0, 0, 0),
            (1, 0, 65_536),
            (1, 1, 91_750),
            (2, 1, 143_976),
            (3, 2, 235_726),
        ] {
            let expected = (f64::from(fixed as f32 / 65_536.0) * 0.1) as f32;
            assert_eq!(distances[(5 + dy) * 11 + 5 + dx], expected);
        }
        // At one source pixel the recovered multiplier, not the previous
        // analytic radial approximation, determines the weight.
        let feathered = feather_mask(11, 11, &binary, FEATHER).unwrap();
        assert_eq!(feathered[5 * 11 + 6], FEATHER as f32);
        assert_eq!(feathered[0], 1.0);
    }

    #[test]
    fn distance_field_matches_independent_graph_shortest_paths() {
        // Dijkstra on the full symmetric mask graph is independent of the
        // production forward/backward raster sweeps and checks all quadrants.
        for size in 2..14_usize {
            let mut binary = vec![1; size * size];
            for (index, value) in binary.iter_mut().enumerate() {
                if (index * 37 + size * 11) % 19 == 0 {
                    *value = 0;
                }
            }
            binary[size / 2] = 0;
            let actual = feather_mask(size, size, &binary, 0.01).unwrap();
            let mut best = binary
                .iter()
                .map(|value| if *value == 0 { 0_u32 } else { u32::MAX })
                .collect::<Vec<_>>();
            let mut visited = vec![false; best.len()];
            for _ in 0..best.len() {
                let index = (0..best.len())
                    .filter(|index| !visited[*index])
                    .min_by_key(|index| best[*index])
                    .unwrap();
                visited[index] = true;
                let (x, y) = ((index % size) as isize, (index / size) as isize);
                for dy in -2_isize..=2 {
                    for dx in -2_isize..=2 {
                        let cost = match (dx.abs(), dy.abs()) {
                            (0, 1) | (1, 0) => 65_536,
                            (1, 1) => 91_750,
                            (1, 2) | (2, 1) => 143_976,
                            _ => continue,
                        };
                        let (xx, yy) = (x + dx, y + dy);
                        if xx >= 0 && yy >= 0 && xx < size as isize && yy < size as isize {
                            let neighbor = yy as usize * size + xx as usize;
                            best[neighbor] = best[neighbor].min(best[index].saturating_add(cost));
                        }
                    }
                }
            }
            for (weight, distance) in actual.iter().zip(best) {
                assert_eq!(
                    *weight,
                    (f64::from(distance as f32 / 65_536.0) * 0.01).min(1.0) as f32
                );
            }
        }
    }

    #[test]
    fn variable_boundary_erosion_faces_down_after_native_rotation() {
        // The rotated center maps to original (31,32); seven angular points
        // exercise a shape the former fixed-four-point representation rejected.
        let boundary = [
            (0.0, 100.0),
            (5.0, 100.0),
            (10.0, 100.0),
            (20.0, 200.0),
            (40.0, 300.0),
            (60.0, 400.0),
            (90.0, 400.0),
        ];
        let binary = raster_mask(64, 64, [32.0, 32.0], 400.0, &boundary).unwrap();
        assert_eq!(binary[47 * 64 + 31], 0);
        for (x, y) in [(46, 32), (16, 32), (31, 17)] {
            assert_eq!(binary[y * 64 + x], 1);
        }
        assert!(binary[..64].iter().all(|pixel| *pixel == 0));
        assert!(binary
            .chunks_exact(64)
            .all(|row| row[0] == 0 && row[63] == 0));
    }

    #[test]
    fn method_two_projects_interpolated_angles_before_rasterization() {
        use crate::profile::{camera_profile, MaskBoundaryPoint};
        use crate::CameraModel;
        // Independent equidistant lens with one pixel per degree. At45°
        // azimuth, interpolated angles give radius30. Interpolating the
        // squared endpoint radii instead gives sqrt(1000), or31.622... .
        static POINTS: [MaskBoundaryPoint; 2] = [
            MaskBoundaryPoint {
                azimuth_degrees: 0.0,
                half_fov_degrees: 20.0,
            },
            MaskBoundaryPoint {
                azimuth_degrees: 90.0,
                half_fov_degrees: 40.0,
            },
        ];
        let mut calibration = synthetic_dual_fisheye_calibration(128, 128).unwrap();
        let lens = &mut calibration.lenses[0];
        lens.fx = 180.0 / std::f64::consts::PI;
        lens.fy = lens.fx;
        let recipe = RadialMaskRecipe {
            lower_hemisphere_boundary: &POINTS,
            interpolation: MaskBoundaryInterpolation::Angle,
            feather_weight_per_pixel: FEATHER,
            provenance: camera_profile(&CameraModel::X4)
                .unwrap()
                .lens(86)
                .unwrap()
                .mask_recipe
                .unwrap()
                .provenance,
        };
        let geometry = Some(ResolvedLensGeometry {
            full_fov_degrees: 100.0,
            blend_angle_degrees: 100.0,
            blend_angle_recorded: false,
        });
        let angle = build_mask((128, 128), lens, 0, Some(recipe), geometry)
            .unwrap()
            .unwrap();
        let projected = build_mask(
            (128, 128),
            lens,
            0,
            Some(RadialMaskRecipe {
                interpolation: MaskBoundaryInterpolation::ProjectedRadius,
                ..recipe
            }),
            geometry,
        )
        .unwrap()
        .unwrap();
        // Native rotated(row86,col86) maps to source(x41,y86),22px on
        // each axis: radius sqrt(968)=31.113, between the two limits.
        assert_eq!(angle.pixel_weight(41, 86), 0.0);
        assert!(projected.pixel_weight(41, 86) > 0.0);
        // Positive control within radius30 remains usable in both methods.
        assert!(angle.pixel_weight(44, 83) > 0.0);
        assert!(projected.pixel_weight(44, 83) > 0.0);
    }

    #[test]
    fn masked_bilinear_taps_cannot_pollute_low_frequency_color() {
        let mut binary = vec![0; 128 * 128];
        fill_circle(&mut binary, 128, 128, [64, 64], 32);
        let weights = feather_mask(128, 128, &binary, FEATHER).unwrap();
        let mask = FisheyeMask {
            width: 128,
            height: 128,
            weights,
        };
        let rgb = binary
            .iter()
            .flat_map(|pixel| [if *pixel == 0 { 255 } else { 100 }; 3])
            .collect();
        let frame = LensFrame::new(128, 128, rgb).unwrap();
        assert!(mask.weight(96.25, 64.25) > 0.0);
        assert_eq!(
            low_frequency_rgb(&frame, 64.25, 64.25, Some(&mask)),
            [100.0; 3]
        );
    }

    #[test]
    fn concurrent_cpu_clones_prepare_masks_inside_a_small_rayon_pool() {
        use crate::StitchEngine;
        use rayon::prelude::*;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        const CHILD: &str = "INSTA360_RS_CONCURRENT_MASK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // A deadlock regression must fail in bounded time, not hang CI.
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "stitch::mask::tests::concurrent_cpu_clones_prepare_masks_inside_a_small_rayon_pool", "--nocapture"])
                .env(CHILD, "1")
                .stdout(Stdio::null())
                .spawn().unwrap();
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "concurrent shared-cache child failed: {status}"
                    );
                    return;
                }
                if started.elapsed() > Duration::from_secs(30) {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("concurrent shared-cache mask preparation deadlocked");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let mut calibration = synthetic_dual_fisheye_calibration(64, 64).unwrap();
        calibration.camera_model = Some(crate::CameraModel::X5);
        calibration.lens_geometry = [Some(ResolvedLensGeometry {
            full_fov_degrees: 198.0,
            blend_angle_degrees: 186.0,
            blend_angle_recorded: false,
        }); 2];
        for lens in &mut calibration.lenses {
            lens.lens_type = 117;
            lens.xi = Some(2.0);
            lens.fx = 48.0;
            lens.fy = 48.0;
        }
        let lenses = [
            LensFrame::new(64, 64, vec![40; 64 * 64 * 3]).unwrap(),
            LensFrame::new(64, 64, vec![80; 64 * 64 * 3]).unwrap(),
        ];
        let projection = crate::EquirectangularProjection {
            width: 16,
            height: 8,
        };
        let mut variants = Vec::new();
        for index in 0..4 {
            let mut variant = calibration.clone();
            variant.lenses[0].cx += f64::from(index) * 0.5;
            let expected = CpuStitcher::new()
                .stitch(&lenses, &variant, projection)
                .unwrap();
            variants.push((variant, expected));
        }
        let stitcher = CpuStitcher::new();
        let clones = [stitcher.clone(), stitcher.clone()];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            (0..64).into_par_iter().for_each(|index| {
                let (calibration, expected) = &variants[index % variants.len()];
                let actual = clones[index % clones.len()]
                    .stitch(&lenses, calibration, projection)
                    .unwrap();
                assert_eq!(actual.as_rgb8(), expected.as_rgb8());
            });
        });
    }

    #[test]
    fn paired_mask_preparation_matches_sequential_work_and_preserves_cache_error_contracts() {
        let dimensions = [(64, 48), (48, 64)];
        let mut calibration = synthetic_dual_fisheye_calibration(64, 64).unwrap();
        calibration.camera_model = Some(crate::CameraModel::X5);
        calibration.lens_geometry = [Some(ResolvedLensGeometry {
            full_fov_degrees: 198.0,
            blend_angle_degrees: 186.0,
            blend_angle_recorded: false,
        }); 2];
        for lens in &mut calibration.lenses {
            lens.xi = Some(2.0);
            lens.fx = 48.0;
            lens.fy = 48.0;
        }
        calibration.lenses[1].cx += 1.75;
        calibration.lenses[1].cy -= 2.25;
        let cache = MaskCache::default();
        for enabled in [[false, false], [false, true], [true, false], [true, true]] {
            for (lens, enabled) in calibration.lenses.iter_mut().zip(enabled) {
                lens.lens_type = if enabled { 117 } else { 113 };
            }
            let actual = cache.prepare(dimensions, &calibration).unwrap();
            let geometry = resolved_render_geometry(&calibration).unwrap();
            for index in 0..2 {
                let expected = build_mask(
                    dimensions[index],
                    &calibration.lenses[index],
                    index,
                    mask_recipe(&calibration, index),
                    geometry[index],
                )
                .unwrap();
                match (&actual[index], expected) {
                    (Some(actual), Some(expected)) => {
                        assert_eq!(
                            (actual.width, actual.height),
                            (expected.width, expected.height)
                        );
                        assert_eq!(actual.weights, expected.weights);
                        assert!(actual.weights.iter().any(|weight| *weight > 0.0));
                    }
                    (None, None) => {}
                    _ => panic!("lens {index} changed mask presence"),
                }
            }
            assert!(Arc::ptr_eq(
                &actual,
                &cache.prepare(dimensions, &calibration).unwrap()
            ));
        }
        let valid = cache.prepare(dimensions, &calibration).unwrap();
        assert!(cache
            .prepare([(8192, 8192); 2], &calibration)
            .unwrap_err()
            .to_string()
            .contains("combined source masks"));
        assert!(Arc::ptr_eq(
            &valid,
            &cache.prepare(dimensions, &calibration).unwrap()
        ));
        let mut invalid = calibration.clone();
        invalid.lenses[0].cx = 1.0e12;
        invalid.lenses[1].xi = None;
        let geometry = resolved_render_geometry(&invalid).unwrap();
        let first_error = build_mask(
            dimensions[0],
            &invalid.lenses[0],
            0,
            mask_recipe(&invalid, 0),
            geometry[0],
        )
        .unwrap_err()
        .to_string();
        let second_error = build_mask(
            dimensions[1],
            &invalid.lenses[1],
            1,
            mask_recipe(&invalid, 1),
            geometry[1],
        )
        .unwrap_err()
        .to_string();
        assert_ne!(first_error, second_error);
        assert_eq!(
            cache.prepare(dimensions, &invalid).unwrap_err().to_string(),
            first_error
        );
        assert!(cache.0.lock().unwrap().is_none());
        let recovered = cache.prepare(dimensions, &calibration).unwrap();
        for index in 0..2 {
            assert_eq!(
                recovered[index].as_ref().unwrap().weights,
                valid[index].as_ref().unwrap().weights
            );
        }
    }

    #[test]
    fn cache_reuses_masks_and_replaces_changed_geometry_without_accumulation() {
        let stitcher = CpuStitcher::new();
        let clone = stitcher.clone();
        assert!(Arc::ptr_eq(&stitcher.masks, &clone.masks));
        let mut calibration = synthetic_dual_fisheye_calibration(32, 32).unwrap();
        let first = stitcher.masks.prepare([(32, 32); 2], &calibration).unwrap();
        for _ in 0..100 {
            assert!(Arc::ptr_eq(
                &first,
                &clone.masks.prepare([(32, 32); 2], &calibration).unwrap()
            ));
        }
        calibration.lenses[0].cx += 0.125;
        let second = stitcher.masks.prepare([(32, 32); 2], &calibration).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        // The cache has released the old allocation, even while callers retain it.
        assert_eq!(Arc::strong_count(&first), 1);
        for width in 1..100 {
            let next = stitcher
                .masks
                .prepare([(width, 32); 2], &calibration)
                .unwrap();
            assert_eq!(Arc::strong_count(&next), 2);
        }
        assert_eq!(Arc::strong_count(&second), 1);
    }

    #[test]
    fn raster_limits_and_degenerate_dimensions_fail_without_allocating() {
        assert!(mask_len(0, 100).is_err());
        assert!(mask_len(usize::MAX, usize::MAX).is_err());
        assert!(mask_len(8193, 8192).is_err());
        assert!(raster_mask(16, 16, [f64::NAN, 0.0], 25.0, &[(0.0, 25.0)]).is_err());
        assert!(raster_mask(16, 16, [0.0; 2], f64::INFINITY, &[(0.0, 25.0)]).is_err());
    }
}
