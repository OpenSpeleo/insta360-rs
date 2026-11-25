//! Portable patch correspondence. No vendor runtime or image-sized search volume.
//!
//! Dynamic uses sparse pyramidal inverse-compositional Lucas–Kanade. Optical
//! Flow adds dense inverse-search patch aggregation and variational smoothing,
//! following Kroeger et al. (2016), <https://arxiv.org/abs/1603.03590>. These are
//! our versioned profiles, not a claim of byte equivalence to Studio or OpenCV.

use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug)]
pub(super) struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<f32>,
    pub valid: Vec<bool>,
}

// Analysis fields are nonempty. Most samples are inside the azimuth interval;
// retain Euclidean wrapping for both boundary taps and arbitrary callers.
fn wrapped_column(x: isize, width: usize) -> usize {
    if x >= 0 && (x as usize) < width {
        x as usize
    } else {
        x.rem_euclid(width as isize) as usize
    }
}

impl Image {
    fn index(&self, x: isize, y: isize) -> Option<usize> {
        (y >= 0 && y < self.height as isize)
            .then(|| y as usize * self.width + wrapped_column(x, self.width))
    }

    fn pixel(&self, x: isize, y: isize) -> Option<f32> {
        let i = self.index(x, y)?;
        // Match the zero-initialized bilinear accumulator at integer positions,
        // including normalization of negative zero.
        self.valid[i].then(|| 0.0 + self.pixels[i])
    }

    fn gradient(&self, x: isize, y: isize) -> Option<[f32; 2]> {
        self.pixel(x, y)?;
        Some([
            (self.pixel(x + 1, y)? - self.pixel(x - 1, y)?) * 0.5,
            (self.pixel(x, y + 1)? - self.pixel(x, y - 1)?) * 0.5,
        ])
    }

    fn sample(&self, x: f32, y: f32) -> Option<f32> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let xx = x.floor() as isize;
        let yy = y.floor() as isize;
        let dx = x - x.floor();
        let dy = y - y.floor();
        let mut result = 0.0;
        for (oy, wy) in [(0, 1.0 - dy), (1, dy)] {
            for (ox, wx) in [(0, 1.0 - dx), (1, dx)] {
                if wx * wy <= 0.0 {
                    continue;
                }
                let i = self.index(xx + ox, yy + oy)?;
                if !self.valid[i] {
                    return None;
                }
                result += self.pixels[i] * wx * wy;
            }
        }
        Some(result)
    }

    fn downsample(&self) -> Self {
        let width = self.width / 2;
        let height = self.height / 2;
        let mut result = Self {
            width,
            height,
            pixels: vec![0.0; width * height],
            valid: vec![false; width * height],
        };
        for y in 0..height {
            for x in 0..width {
                let samples = [
                    2 * y * self.width + 2 * x,
                    2 * y * self.width + 2 * x + 1,
                    (2 * y + 1) * self.width + 2 * x,
                    (2 * y + 1) * self.width + 2 * x + 1,
                ];
                let i = y * width + x;
                result.valid[i] = samples.iter().all(|&j| self.valid[j]);
                result.pixels[i] = samples.iter().map(|&j| self.pixels[j]).sum::<f32>() * 0.25;
            }
        }
        result
    }
}

struct PyramidLevel<'a> {
    image: Cow<'a, Image>,
    // Dense overlapping patches reuse gradients; sparse tracks calculate them
    // only where needed, avoiding a full-belt pass for their smaller workload.
    gradients: Vec<Option<[f32; 2]>>,
}

impl<'a> PyramidLevel<'a> {
    fn new(image: Cow<'a, Image>, dense: bool) -> Self {
        let gradients = if dense {
            (0..image.width * image.height)
                .into_par_iter()
                .map(|i| image.gradient((i % image.width) as isize, (i / image.width) as isize))
                .collect()
        } else {
            Vec::new()
        };
        Self { image, gradients }
    }

    fn reference(&self, x: isize, y: isize) -> Option<(f32, f32, f32)> {
        let value = self.image.pixel(x, y)?;
        let [gx, gy] = if self.gradients.is_empty() {
            self.image.gradient(x, y)?
        } else {
            self.gradients[self.image.index(x, y)?]?
        };
        Some((value, gx, gy))
    }
}

fn pyramid(image: &Image, dense: bool) -> Vec<PyramidLevel<'_>> {
    let mut levels = vec![PyramidLevel::new(Cow::Borrowed(image), dense)];
    // Narrow calibrated belts must retain enough valid rows for a patch.
    while levels.len() < 4 && levels.last().expect("base level exists").image.height >= 64 {
        let reduced = levels.last().expect("base level exists").image.downsample();
        levels.push(PyramidLevel::new(Cow::Owned(reduced), dense));
    }
    levels
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Vector {
    pub dx: f32,
    pub dy: f32,
    pub confidence: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Field {
    pub width: usize,
    pub height: usize,
    pub vectors: Vec<Vector>,
}

impl Field {
    // The solver produces finite vectors with confidence in [0, 1]. At integer
    // coordinates only one bilinear tap contributes. Keep its multiply/divide
    // and zero-initialized sums: returning the vector directly would change
    // fractional-confidence rounding and the sign of negative zero.
    fn sample_integer(&self, x: isize, y: isize) -> Vector {
        if y < 0 || y >= self.height as isize {
            return Vector::default();
        }
        let value = self.vectors[y as usize * self.width + wrapped_column(x, self.width)];
        let weight = value.confidence;
        let mut result = Vector {
            dx: 0.0 + value.dx * weight,
            dy: 0.0 + value.dy * weight,
            confidence: 0.0 + weight,
        };
        if result.confidence > 0.0 {
            result.dx /= result.confidence;
            result.dy /= result.confidence;
        }
        result
    }

    pub fn sample(&self, x: f32, y: f32) -> Vector {
        if !x.is_finite() || !y.is_finite() || y < 0.0 || y > (self.height - 1) as f32 {
            return Vector::default();
        }
        let x = x.rem_euclid(self.width as f32);
        let low = [x.floor() as usize % self.width, y.floor() as usize];
        let high = [(low[0] + 1) % self.width, (low[1] + 1).min(self.height - 1)];
        let fraction = [x - x.floor(), y - y.floor()];
        let mut result = Vector::default();
        for (yy, wy) in [(low[1], 1.0 - fraction[1]), (high[1], fraction[1])] {
            for (xx, wx) in [(low[0], 1.0 - fraction[0]), (high[0], fraction[0])] {
                let value = self.vectors[yy * self.width + xx];
                let w = wx * wy * value.confidence;
                result.dx += value.dx * w;
                result.dy += value.dy * w;
                result.confidence += w;
            }
        }
        if result.confidence > 0.0 {
            result.dx /= result.confidence;
            result.dy /= result.confidence;
        }
        result
    }
}

#[derive(Clone, Copy)]
struct Patch {
    x: usize,
    y: usize,
    flow: Vector,
    error: f32,
}

fn track(
    first: &PyramidLevel<'_>,
    second: &Image,
    position: (usize, usize),
    initial: Vector,
    radius: isize,
    iterations: usize,
    cancel: &AtomicBool,
) -> Patch {
    let (x, y) = position;
    let fail = Patch {
        x,
        y,
        flow: Vector::default(),
        error: 1.0,
    };
    // Bounded patch scratch. The largest profile uses a 15x15 patch.
    let mut values = [(0.0_f32, 0.0_f32, 0.0_f32); 225];
    let count = ((radius * 2 + 1) * (radius * 2 + 1)) as usize;
    let mut mean = 0.0;
    let mut n = 0;
    for oy in -radius..=radius {
        for ox in -radius..=radius {
            let Some(reference) = first.reference(x as isize + ox, y as isize + oy) else {
                return fail;
            };
            values[n] = reference;
            mean += reference.0;
            n += 1;
        }
    }
    mean /= count as f32;
    let (mut hxx, mut hxy, mut hyy) = (0.0, 0.0, 0.0);
    for (value, gx, gy) in &mut values[..count] {
        *value -= mean;
        hxx += *gx * *gx;
        hxy += *gx * *gy;
        hyy += *gy * *gy;
    }
    let det = hxx * hyy - hxy * hxy;
    if det <= 1e-8 || det / (hxx + hyy).max(1e-8) < 1e-5 {
        return fail;
    }
    let mut flow = initial;
    let mut previous = f32::INFINITY;
    let mut best = fail;
    // Every used element is overwritten before reading in each iteration.
    let mut target = [0.0_f32; 225];
    for _ in 0..iterations {
        if cancel.load(Ordering::Relaxed) {
            return fail;
        }
        let mut mean = 0.0;
        let mut n = 0;
        for oy in -radius..=radius {
            for ox in -radius..=radius {
                let Some(value) = second.sample(
                    x as f32 + ox as f32 + flow.dx,
                    y as f32 + oy as f32 + flow.dy,
                ) else {
                    return best;
                };
                target[n] = value;
                mean += value;
                n += 1;
            }
        }
        mean /= count as f32;
        let (mut bx, mut by, mut error) = (0.0, 0.0, 0.0);
        for (&value, &(reference, gx, gy)) in target[..count].iter().zip(&values[..count]) {
            let residual = value - mean - reference;
            bx += gx * residual;
            by += gy * residual;
            error += residual * residual;
        }
        error /= count as f32;
        if error > previous {
            break;
        }
        previous = error;
        best = Patch {
            x,
            y,
            flow: Vector {
                confidence: 1.0,
                ..flow
            },
            error,
        };
        let dx = (hyy * bx - hxy * by) / det;
        let dy = (hxx * by - hxy * bx) / det;
        flow.dx -= dx;
        flow.dy -= dy;
        if !flow.dx.is_finite() || !flow.dy.is_finite() || flow.dx.abs().max(flow.dy.abs()) > 32.0 {
            break;
        }
        if dx * dx + dy * dy < 0.0004 {
            break;
        }
    }
    best
}

/// Integer offsets (coarsest-level pixels, either axis) searched for each
/// patch's starting vector.
///
/// The belt pyramid is shallow: a 64-row belt only supports two levels, so a
/// solver started from zero captures about one coarse pixel (about 1 degree).
/// Near-field parallax on the X4 is 1.7 degrees at 1-2 m and 4-5 degrees at
/// 40 cm, and the solver locked onto wrong local minima there (residuals of
/// 18 px at full resolution). Seeding from an exhaustive correlation search
/// brings those to 2-6 px. Larger ranges find more false matches on repetitive
/// texture and lower coverage.
const SEED_SEARCH_RANGE: isize = 6;
/// Minimum correlation a seed needs; weaker patches start from zero.
const SEED_MIN_SCORE: f32 = 0.5;
/// Offsets scoring within this of the best are treated as equally good, and the
/// one nearest zero wins. Smooth or one-dimensional texture gives a flat peak,
/// and quantization noise alone would otherwise pick its offset; on a 32-row belt
/// a stray vertical offset pushes the patch out of the belt.
const SEED_TOLERANCE: f32 = 0.02;
/// A seeded fit replaces the fit from zero only when its patch error is below
/// this fraction of it. Correlation alone is ambiguous on anisotropic texture
/// (any offset along a stripe matches), and forward and backward passes then
/// seed differently and fail their consistency check.
const SEED_ERROR_RATIO: f32 = 0.5;

/// Starting vector for one coarsest-level patch: the integer offset within
/// [`SEED_SEARCH_RANGE`] with the best zero-mean normalized correlation, or zero
/// when the patch is flat or incomplete or no offset correlates well. Among
/// offsets within [`SEED_TOLERANCE`] of the best score the smallest wins.
fn search_seed(first: &Image, second: &Image, (x, y): (usize, usize), radius: isize) -> Vector {
    let range = SEED_SEARCH_RANGE;
    let side = (2 * radius + 1) as usize;
    let mut reference = Vec::with_capacity(side * side);
    for oy in -radius..=radius {
        for ox in -radius..=radius {
            match first.pixel(x as isize + ox, y as isize + oy) {
                Some(v) => reference.push(v),
                None => return Vector::default(),
            }
        }
    }
    let n = reference.len() as f32;
    let mean = reference.iter().sum::<f32>() / n;
    reference.iter_mut().for_each(|v| *v -= mean);
    let energy = reference.iter().map(|v| v * v).sum::<f32>();
    if energy < 1e-6 {
        return Vector::default();
    }
    let mut scores = Vec::with_capacity(((2 * range + 1) * (2 * range + 1)) as usize);
    let mut target = vec![0.0_f32; reference.len()];
    for dy in -range..=range {
        'offset: for dx in -range..=range {
            let mut i = 0;
            for oy in -radius..=radius {
                for ox in -radius..=radius {
                    match second.pixel(x as isize + ox + dx, y as isize + oy + dy) {
                        Some(v) => target[i] = v,
                        None => continue 'offset,
                    }
                    i += 1;
                }
            }
            let tm = target.iter().sum::<f32>() / n;
            let (mut cross, mut te) = (0.0, 0.0);
            for (r, t) in reference.iter().zip(&target) {
                let t = t - tm;
                cross += r * t;
                te += t * t;
            }
            scores.push((cross / (energy * te).sqrt().max(1e-6), dx, dy));
        }
    }
    let top = scores.iter().map(|s| s.0).fold(f32::NEG_INFINITY, f32::max);
    if top < SEED_MIN_SCORE {
        return Vector::default();
    }
    // Ascending distance, then descending score; the scan order breaks exact ties.
    let best = scores
        .into_iter()
        .filter(|s| s.0 >= top - SEED_TOLERANCE)
        .min_by(|a, b| {
            (a.1 * a.1 + a.2 * a.2)
                .cmp(&(b.1 * b.1 + b.2 * b.2))
                .then(b.0.total_cmp(&a.0))
        })
        .expect("the top score is a candidate");
    Vector {
        dx: best.1 as f32,
        dy: best.2 as f32,
        confidence: 1.0,
    }
}

fn estimate(
    a: &[PyramidLevel<'_>],
    b: &[PyramidLevel<'_>],
    dense: bool,
    cancel: &AtomicBool,
) -> Field {
    let mut coarse: Option<Field> = None;
    for level in (0..a.len()).rev() {
        let reference = &a[level];
        let first = &reference.image;
        let second = &b[level].image;
        let radius = if dense { 3 } else { 7 };
        let stride = if dense { 4 } else { 16 };
        // Anchor sparse rows at the optical seam. Starting at the patch margin
        // puts the only track of a 32-row belt at row 8, outside narrow overlap.
        let start = if dense {
            radius + 1
        } else {
            let center = first.height / 2;
            center - (center - radius - 1) / stride * stride
        };
        let positions: Vec<_> = (start..first.height - radius - 1)
            .step_by(stride)
            .flat_map(|y| (0..first.width).step_by(stride).map(move |x| (x, y)))
            .collect();
        let patches: Vec<_> = positions
            .par_iter()
            .map(|&(x, y)| {
                // Choose the largest supported patch before fitting. This is
                // adaptation to observed source support, not a lower residual
                // or consistency threshold when a difficult solve fails.
                let fit = |initial: Vector| {
                    let radius = if dense {
                        Some(radius)
                    } else {
                        [7, 5, 3].into_iter().find(|&radius| {
                            let margin = radius as isize + 1;
                            (-margin..=margin).all(|oy| {
                                (-margin..=margin).all(|ox| {
                                    let xx = x as isize + ox;
                                    let yy = y as isize + oy;
                                    first.pixel(xx, yy).is_some()
                                        && second
                                            .sample(xx as f32 + initial.dx, yy as f32 + initial.dy)
                                            .is_some()
                                })
                            })
                        })
                    };
                    let Some(radius) = radius else {
                        return Patch {
                            x,
                            y,
                            flow: Vector::default(),
                            error: 1.0,
                        };
                    };
                    track(
                        reference,
                        second,
                        (x, y),
                        initial,
                        radius as isize,
                        if dense { 16 } else { 15 },
                        cancel,
                    )
                };
                let Some(coarse) = coarse.as_ref() else {
                    // Coarsest level: the solver's own start from zero, unless a
                    // correlation seed converges to a clearly better fit.
                    let zero = fit(Vector::default());
                    let seed = search_seed(first, second, (x, y), radius as isize);
                    if seed.dx == 0.0 && seed.dy == 0.0 {
                        return zero;
                    }
                    let seeded = fit(seed);
                    return if seeded.flow.confidence > 0.0
                        && seeded.error < zero.error * SEED_ERROR_RATIO
                    {
                        seeded
                    } else {
                        zero
                    };
                };
                let v = coarse.sample((x as f32 + 0.5) * 0.5 - 0.5, (y as f32 + 0.5) * 0.5 - 0.5);
                fit(Vector {
                    dx: v.dx * 2.0,
                    dy: v.dy * 2.0,
                    ..v
                })
            })
            .collect();
        // Dense patches vote over their footprint; sparse tracks use a compact
        // tent kernel. Unsupported pixels never acquire invented correspondence.
        let support = if dense { radius } else { stride };
        let mut field = aggregate(first, second, &patches, support);
        if dense && !cancel.load(Ordering::Relaxed) {
            refine(first, second, &mut field);
        }
        coarse = Some(field);
    }
    coarse.expect("nonempty pyramid produces its finest field")
}

fn aggregate(first: &Image, second: &Image, patches: &[Patch], support: usize) -> Field {
    let mut field = Field {
        width: first.width,
        height: first.height,
        vectors: vec![Vector::default(); first.width * first.height],
    };
    // Indexed parallel collection retains row-major patch order. Each output
    // row visits only overlapping patch rows, in that same order: no atomics or
    // cross-thread floating-point reduction changes a pixel's accumulation.
    field
        .vectors
        .par_chunks_mut(first.width)
        .enumerate()
        .for_each(|(y, row)| {
            let begin = patches.partition_point(|p| p.y < y.saturating_sub(support));
            let end = patches.partition_point(|p| p.y <= y + support);
            for patch in patches[begin..end]
                .iter()
                .filter(|p| p.flow.confidence > 0.0 && p.error < 0.04)
            {
                let oy = y as isize - patch.y as isize;
                for ox in -(support as isize)..=support as isize {
                    let x = (patch.x as isize + ox).rem_euclid(first.width as isize) as usize;
                    let i = y * first.width + x;
                    if !first.valid[i] || !second.valid[i] {
                        continue;
                    }
                    let position_weight = (1.0 - ox.unsigned_abs() as f32 / (support + 1) as f32)
                        * (1.0 - oy.unsigned_abs() as f32 / (support + 1) as f32);
                    let weight = position_weight / (patch.error + 0.001);
                    row[x].dx += patch.flow.dx * weight;
                    row[x].dy += patch.flow.dy * weight;
                    row[x].confidence += weight;
                }
            }
            for v in row {
                if v.confidence > 0.0 {
                    v.dx /= v.confidence;
                    v.dy /= v.confidence;
                    v.confidence = 1.0;
                }
            }
        });
    field
}

/// Fixed-iteration Jacobi solution of linearized brightness constancy and
/// quadratic flow smoothness. Restrict smoothing to accepted valid support.
fn refine(first: &Image, second: &Image, field: &mut Field) {
    let mut next = field.vectors.clone();
    for _ in 0..5 {
        next.par_iter_mut().enumerate().for_each(|(i, out)| {
            let f = field.vectors[i];
            *out = f;
            if f.confidence == 0.0 {
                return;
            }
            let x = (i % field.width) as f32;
            let y = (i / field.width) as f32;
            let (Some(a), Some(b), Some(l), Some(r), Some(t), Some(d)) = (
                first.pixel((i % field.width) as isize, (i / field.width) as isize),
                second.sample(x + f.dx, y + f.dy),
                second.sample(x + f.dx - 1.0, y + f.dy),
                second.sample(x + f.dx + 1.0, y + f.dy),
                second.sample(x + f.dx, y + f.dy - 1.0),
                second.sample(x + f.dx, y + f.dy + 1.0),
            ) else {
                return;
            };
            let (mut dx, mut dy, mut n) = (0.0, 0.0, 0.0);
            for (ox, oy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let v = field.sample_integer(
                    (i % field.width) as isize + ox,
                    (i / field.width) as isize + oy,
                );
                if v.confidence > 0.0 {
                    dx += v.dx;
                    dy += v.dy;
                    n += 1.0;
                }
            }
            if n == 0.0 {
                return;
            }
            dx /= n;
            dy /= n;
            let gx = (r - l) * 0.5;
            let gy = (d - t) * 0.5;
            let correction =
                (b - a + gx * (dx - f.dx) + gy * (dy - f.dy)) / (0.05 + gx * gx + gy * gy);
            out.dx = dx - gx * correction;
            out.dy = dy - gy * correction;
        });
        std::mem::swap(&mut next, &mut field.vectors);
    }
}

/// Angular radius (radians) over which accepted correspondence spreads into
/// neighbouring rejected cells, weighted by confidence.
const FILL_SIGMA: f64 = 2.8 * std::f64::consts::PI / 180.0;
/// Angular radius of the final smoothing, removing steps at confidence edges.
const SMOOTH_SIGMA: f64 = 0.35 * std::f64::consts::PI / 180.0;
/// Blurred acceptance at which filled correspondence keeps full strength. Below
/// it, filled vectors fade to zero so isolated matches do not drive wide warps.
const FILL_FULL_SUPPORT: f32 = 0.2;

/// Separable Gaussian over interleaved channels: periodic in azimuth, truncated
/// at the belt's latitude edges (callers normalize by a blurred weight channel).
fn gaussian<const N: usize>(
    values: &[[f32; N]],
    width: usize,
    height: usize,
    [sigma_x, sigma_y]: [f32; 2],
) -> Vec<[f32; N]> {
    let taps = |sigma: f32| {
        let radius = (sigma * 3.0).ceil() as isize;
        let kernel: Vec<f32> = (-radius..=radius)
            .map(|o| (-(o * o) as f32 / (2.0 * sigma * sigma)).exp())
            .collect();
        (radius, kernel)
    };
    let (radius, kernel) = taps(sigma_x);
    let mut horizontal = vec![[0.0; N]; values.len()];
    horizontal
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                for (k, o) in kernel.iter().zip(-radius..=radius) {
                    let value = values[y * width + wrapped_column(x as isize + o, width)];
                    for c in 0..N {
                        out[c] += k * value[c];
                    }
                }
            }
        });
    let (radius, kernel) = taps(sigma_y);
    let mut result = vec![[0.0; N]; values.len()];
    result
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            for (k, o) in kernel.iter().zip(-radius..=radius) {
                let yy = y as isize + o;
                if yy < 0 || yy >= height as isize {
                    continue;
                }
                let source = &horizontal[yy as usize * width..][..width];
                for (out, value) in row.iter_mut().zip(source) {
                    for c in 0..N {
                        out[c] += k * value[c];
                    }
                }
            }
        });
    result
}

/// Turn sparse, consistency-filtered correspondence into a smooth field.
///
/// Rejected cells previously became hard zeros beside accepted displacements.
/// Those steps fold the render map, and the safety pass then scaled the whole
/// field down. Here rejected cells take the confidence-weighted mean of nearby
/// accepted vectors (normalized convolution), fading to zero only where no
/// correspondence is nearby, and the result is lightly smoothed. Every vector
/// is returned with confidence 1: zero is a smooth value, not a pinned rejection.
/// `half_height` is the belt's angular half-height in radians; the field spans
/// a full turn of azimuth over its width and the belt over its height.
pub(super) fn densify(field: &mut Field, half_height: f64) {
    let (width, height) = (field.width, field.height);
    if width == 0 || height == 0 {
        return;
    }
    let weighted: Vec<[f32; 3]> = field
        .vectors
        .iter()
        .map(|v| {
            let c = if v.confidence >= 0.5 && v.dx.is_finite() && v.dy.is_finite() {
                v.confidence
            } else {
                0.0
            };
            [c * v.dx, c * v.dy, c]
        })
        .collect();
    let cell = [
        std::f64::consts::TAU / width as f64,
        2.0 * half_height / height as f64,
    ];
    let sigma = |angle: f64| cell.map(|size| ((angle / size) as f32).max(0.5));
    let (fill_sigma, smooth_sigma) = (sigma(FILL_SIGMA), sigma(SMOOTH_SIGMA));
    let spread = gaussian(&weighted, width, height, fill_sigma);
    let unit = gaussian(&vec![[1.0_f32]; width * height], width, height, fill_sigma);
    let combined: Vec<[f32; 2]> = weighted
        .iter()
        .zip(&spread)
        .zip(&unit)
        .map(|((&[cx, cy, c], &[sx, sy, sc]), &[norm])| {
            let fade = (sc / norm.max(1e-6) / FILL_FULL_SUPPORT).min(1.0);
            let fill = if sc > 1e-6 {
                [sx / sc * fade, sy / sc * fade]
            } else {
                [0.0; 2]
            };
            [cx + (1.0 - c) * fill[0], cy + (1.0 - c) * fill[1]]
        })
        .collect();
    let smooth = gaussian(&combined, width, height, smooth_sigma);
    let unit = gaussian(
        &vec![[1.0_f32]; width * height],
        width,
        height,
        smooth_sigma,
    );
    for ((v, [dx, dy]), [norm]) in field.vectors.iter_mut().zip(smooth).zip(unit) {
        *v = Vector {
            dx: dx / norm,
            dy: dy / norm,
            confidence: 1.0,
        };
    }
}

pub(super) fn correspond(first: &Image, second: &Image, dense: bool, cancel: &AtomicBool) -> Field {
    let (first, second) = rayon::join(|| pyramid(first, dense), || pyramid(second, dense));
    let (mut forward, backward) = rayon::join(
        || estimate(&first, &second, dense, cancel),
        || estimate(&second, &first, dense, cancel),
    );
    let width = forward.width;
    forward
        .vectors
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, v)| {
            if v.confidence == 0.0 {
                return;
            }
            let reverse = backward.sample((i % width) as f32 + v.dx, (i / width) as f32 + v.dy);
            let error = (v.dx + reverse.dx).hypot(v.dy + reverse.dy);
            if reverse.confidence < 0.5 || !error.is_finite() || error > 1.5 {
                *v = Vector::default();
            } else {
                v.confidence = (1.0 - error / 1.5).clamp(0.0, 1.0);
            }
        });
    forward
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector_bits(vector: Vector) -> [u32; 3] {
        [vector.dx, vector.dy, vector.confidence].map(f32::to_bits)
    }

    #[test]
    fn interior_image_indices_preserve_wrapping_and_vertical_rejection() {
        for width in [1, 3, 17, 128] {
            let image = Image {
                width,
                height: 5,
                pixels: vec![0.0; width * 5],
                valid: vec![true; width * 5],
            };
            let extent = width as isize;
            let columns = (-2 * extent..=3 * extent).chain([
                isize::MIN,
                isize::MIN + 1,
                isize::MAX - 1,
                isize::MAX,
            ]);
            for x in columns {
                for y in -2..=6 {
                    let expected = (0..5)
                        .contains(&y)
                        .then(|| y as usize * width + x.rem_euclid(width as isize) as usize);
                    assert_eq!(image.index(x, y), expected, "x={x} y={y} width={width}");
                }
            }
        }
    }

    fn finite_test_field(width: usize, height: usize) -> Field {
        let values = [
            0.0,
            -0.0,
            0.37,
            -0.73,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::from_bits(1),
            -f32::from_bits(1),
        ];
        let confidence = [0.0, -0.0, 0.25, 0.7, 1.0, f32::MIN_POSITIVE];
        Field {
            width,
            height,
            vectors: (0..width * height)
                .map(|i| Vector {
                    dx: values[i % values.len()],
                    dy: values[(i / values.len()) % values.len()],
                    confidence: confidence[(i / (values.len() * values.len())) % confidence.len()],
                })
                .collect(),
        }
    }

    #[test]
    fn integer_field_neighbors_match_bilinear_reference_bits() {
        for width in [1, 3, 17, 128] {
            let field = finite_test_field(width, 401);
            for y in -1..=field.height as isize {
                for x in -2 * width as isize..=3 * width as isize {
                    assert_eq!(
                        vector_bits(field.sample_integer(x, y)),
                        vector_bits(field.sample(x as f32, y as f32)),
                        "x={x} y={y} width={width}",
                    );
                }
            }
        }
    }

    #[test]
    fn integer_neighbor_refinement_matches_scalar_jacobi_reference_bits() {
        let first = chart(0.0);
        let mut second = chart(2.25);
        for i in (0..second.valid.len()).step_by(19) {
            second.valid[i] = false;
        }
        let mut optimized = finite_test_field(first.width, first.height);
        let mut reference = optimized.clone();
        refine(&first, &second, &mut optimized);
        refine_scalar_reference(&first, &second, &mut reference);
        for (i, (actual, expected)) in optimized.vectors.iter().zip(reference.vectors).enumerate() {
            assert_eq!(vector_bits(*actual), vector_bits(expected), "cell {i}");
        }
    }

    #[test]
    #[ignore = "opt-in scalar versus integer-neighbor timing; no performance assertion"]
    fn benchmark_dense_integer_neighbors() {
        use std::{hint::black_box, time::Instant};

        fn sample_neighbors(field: &Field, integer: bool) -> f32 {
            let mut checksum = 0.0;
            for y in 0..field.height {
                for x in 0..field.width {
                    for (ox, oy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                        let v = if integer {
                            field.sample_integer(x as isize + ox, y as isize + oy)
                        } else {
                            field.sample(x as f32 + ox as f32, y as f32 + oy as f32)
                        };
                        checksum += black_box(v.dx + v.dy + v.confidence);
                    }
                }
            }
            checksum
        }

        let field = finite_test_field(2048, 64);
        assert_eq!(
            sample_neighbors(&field, false).to_bits(),
            sample_neighbors(&field, true).to_bits(),
        );
        let mut milliseconds = [Vec::new(), Vec::new()];
        for repeat in 0..7 {
            // Alternate order so neither implementation always follows the other.
            for index in [repeat % 2, 1 - repeat % 2] {
                let started = Instant::now();
                black_box(sample_neighbors(black_box(&field), index == 1));
                milliseconds[index].push(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        for values in &mut milliseconds {
            values.sort_by(f64::total_cmp);
        }
        println!(
            "dense integer neighbors: debug={} scalar_median_ms={:.3} integer_median_ms={:.3} samples_per_run={}",
            cfg!(debug_assertions),
            milliseconds[0][3],
            milliseconds[1][3],
            field.width * field.height * 4,
        );
    }

    // Historical bilinear-neighbor implementation, deliberately executed
    // serially to independently check all five Jacobi passes and their order.
    fn refine_scalar_reference(first: &Image, second: &Image, field: &mut Field) {
        let mut next = field.vectors.clone();
        for _ in 0..5 {
            next.iter_mut().enumerate().for_each(|(i, out)| {
                let f = field.vectors[i];
                *out = f;
                if f.confidence == 0.0 {
                    return;
                }
                let x = (i % field.width) as f32;
                let y = (i / field.width) as f32;
                let (Some(a), Some(b), Some(l), Some(r), Some(t), Some(d)) = (
                    first.pixel((i % field.width) as isize, (i / field.width) as isize),
                    second.sample(x + f.dx, y + f.dy),
                    second.sample(x + f.dx - 1.0, y + f.dy),
                    second.sample(x + f.dx + 1.0, y + f.dy),
                    second.sample(x + f.dx, y + f.dy - 1.0),
                    second.sample(x + f.dx, y + f.dy + 1.0),
                ) else {
                    return;
                };
                let (mut dx, mut dy, mut n) = (0.0, 0.0, 0.0);
                for (ox, oy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                    let v = field.sample(x + ox, y + oy);
                    if v.confidence > 0.0 {
                        dx += v.dx;
                        dy += v.dy;
                        n += 1.0;
                    }
                }
                if n == 0.0 {
                    return;
                }
                dx /= n;
                dy /= n;
                let gx = (r - l) * 0.5;
                let gy = (d - t) * 0.5;
                let correction =
                    (b - a + gx * (dx - f.dx) + gy * (dy - f.dy)) / (0.05 + gx * gx + gy * gy);
                out.dx = dx - gx * correction;
                out.dy = dy - gy * correction;
            });
            std::mem::swap(&mut next, &mut field.vectors);
        }
    }

    #[test]
    fn integer_references_match_bilinear_samples_and_gradients_at_wrapped_edges() {
        let mut image = chart(0.0);
        image.pixels[0] = -0.0;
        image.valid[31] = false;
        image.valid[image.width * 15 + 127] = false;
        let level = PyramidLevel::new(Cow::Borrowed(&image), true);
        for y in -1..=image.height as isize {
            for x in -3..image.width as isize + 3 {
                assert_eq!(
                    image.pixel(x, y).map(f32::to_bits),
                    image.sample(x as f32, y as f32).map(f32::to_bits),
                );
                let expected = (|| {
                    let value = image.sample(x as f32, y as f32)?;
                    let left = image.sample(x as f32 - 1.0, y as f32)?;
                    let right = image.sample(x as f32 + 1.0, y as f32)?;
                    let top = image.sample(x as f32, y as f32 - 1.0)?;
                    let bottom = image.sample(x as f32, y as f32 + 1.0)?;
                    Some([value, (right - left) * 0.5, (bottom - top) * 0.5])
                })();
                assert_eq!(
                    level
                        .reference(x, y)
                        .map(|(v, x, y)| [v, x, y].map(f32::to_bits)),
                    expected.map(|v| v.map(f32::to_bits)),
                );
            }
        }
    }

    #[test]
    fn cached_dense_gradients_preserve_every_estimated_vector() {
        let first = chart(0.0);
        let second = chart(2.25);
        let mut a = pyramid(&first, true);
        let mut b = pyramid(&second, true);
        assert!(std::ptr::eq(a[0].image.as_ref(), &first));
        assert!(std::ptr::eq(b[0].image.as_ref(), &second));
        assert_eq!(a.len(), 2);
        let cached = estimate(&a, &b, true, &AtomicBool::new(false));
        for level in a.iter_mut().chain(&mut b) {
            level.gradients.clear();
        }
        let direct = estimate(&a, &b, true, &AtomicBool::new(false));
        for (cached, direct) in cached.vectors.into_iter().zip(direct.vectors) {
            assert_eq!(vector_bits(cached), vector_bits(direct));
        }
    }

    #[test]
    fn row_owned_aggregation_matches_serial_scatter_order_exactly() {
        let mut first = chart(0.0);
        let mut second = chart(0.0);
        for i in (0..first.valid.len()).step_by(17) {
            first.valid[i] = false;
        }
        for i in (0..second.valid.len()).step_by(29) {
            second.valid[i] = false;
        }
        let patches: Vec<_> = [0, 1, 19, 32, 62, 63]
            .into_iter()
            .flat_map(|y| {
                [0, 1, 4, 63, 124, 127].into_iter().map(move |x| {
                    let index = y * 128 + x;
                    Patch {
                        x,
                        y,
                        flow: Vector {
                            dx: (index % 19) as f32 / 3.7 - 1.2,
                            dy: (index % 13) as f32 / 7.1 - 0.8,
                            confidence: if index.is_multiple_of(7) { 0.0 } else { 1.0 },
                        },
                        error: if index.is_multiple_of(5) {
                            0.05
                        } else {
                            (index % 11) as f32 * 0.003
                        },
                    }
                })
            })
            .collect();
        for support in [3, 16] {
            let actual = aggregate(&first, &second, &patches, support);
            let mut expected = vec![Vector::default(); first.width * first.height];
            // Historical patch-major scatter is the independent execution-order
            // reference, especially where neighborhoods wrap around azimuth.
            for patch in patches
                .iter()
                .filter(|p| p.flow.confidence > 0.0 && p.error < 0.04)
            {
                for oy in -(support as isize)..=support as isize {
                    let y = patch.y as isize + oy;
                    if y < 0 || y >= first.height as isize {
                        continue;
                    }
                    for ox in -(support as isize)..=support as isize {
                        let x = (patch.x as isize + ox).rem_euclid(first.width as isize) as usize;
                        let i = y as usize * first.width + x;
                        if !first.valid[i] || !second.valid[i] {
                            continue;
                        }
                        let position = (1.0 - ox.unsigned_abs() as f32 / (support + 1) as f32)
                            * (1.0 - oy.unsigned_abs() as f32 / (support + 1) as f32);
                        let weight = position / (patch.error + 0.001);
                        expected[i].dx += patch.flow.dx * weight;
                        expected[i].dy += patch.flow.dy * weight;
                        expected[i].confidence += weight;
                    }
                }
            }
            for vector in &mut expected {
                if vector.confidence > 0.0 {
                    vector.dx /= vector.confidence;
                    vector.dy /= vector.confidence;
                    vector.confidence = 1.0;
                }
            }
            for (actual, expected) in actual.vectors.into_iter().zip(expected) {
                assert_eq!(vector_bits(actual), vector_bits(expected));
            }
        }
    }

    fn chart(shift: f32) -> Image {
        let (width, height) = (128, 64);
        let pixels = (0..width * height)
            .map(|i| {
                let x = (i % width) as f32 - shift;
                let y = (i / width) as f32;
                0.5 + 0.13 * (x * 0.37).sin()
                    + 0.12 * (y * 0.29).sin()
                    + 0.1 * (x * 0.21 + y * 0.47).cos()
            })
            .collect();
        Image {
            width,
            height,
            pixels,
            valid: vec![true; width * height],
        }
    }
    #[test]
    fn both_algorithms_recover_independent_subpixel_translation() {
        for dense in [false, true] {
            let field = correspond(&chart(0.0), &chart(2.25), dense, &AtomicBool::new(false));
            let values: Vec<_> = (20..44)
                .flat_map(|y| (20..100).map(move |x| y * 128 + x))
                .map(|i| field.vectors[i])
                .filter(|v| v.confidence > 0.5)
                .collect();
            assert!(values.len() > 100, "insufficient support ({dense})");
            let error = values
                .iter()
                .map(|v| (v.dx - 2.25).abs() + v.dy.abs())
                .sum::<f32>()
                / values.len() as f32;
            assert!(error < 0.3, "translation error{error} (dense={dense})");
        }
    }
    #[test]
    fn coarse_seed_recovers_shifts_beyond_the_solver_capture_range() {
        // A 64-row belt has two pyramid levels, so 11 px is 5.5 px at the coarsest
        // level: far outside what the solver captures when started from zero.
        for dense in [false, true] {
            let field = correspond(&chart(0.0), &chart(11.0), dense, &AtomicBool::new(false));
            let values: Vec<_> = (20..44)
                .flat_map(|y| (20..100).map(move |x| y * 128 + x))
                .map(|i| field.vectors[i])
                .filter(|v| v.confidence > 0.5)
                .collect();
            assert!(values.len() > 100, "insufficient support ({dense})");
            let error = values
                .iter()
                .map(|v| (v.dx - 11.0).abs() + v.dy.abs())
                .sum::<f32>()
                / values.len() as f32;
            assert!(error < 0.3, "translation error {error} (dense={dense})");
        }
    }

    #[test]
    fn seed_search_ignores_flat_patches() {
        let mut flat = chart(0.0);
        flat.pixels.fill(0.5);
        let seed = search_seed(&flat, &chart(3.0), (64, 32), 3);
        assert_eq!(vector_bits(seed), vector_bits(Vector::default()));
    }

    // 256 x 64 cells over a full turn and a 10.26 degree belt (the X4 overlap):
    // 1.4 x 0.16 degrees per cell.
    const DENSIFY_HALF_HEIGHT: f64 = 5.13 * std::f64::consts::PI / 180.0;

    fn sparse_field(accepted: impl Fn(usize, usize) -> Option<(f32, f32)>) -> Field {
        let (width, height) = (256, 64);
        Field {
            width,
            height,
            vectors: (0..width * height)
                .map(|i| match accepted(i % width, i / width) {
                    Some((dx, dy)) => Vector {
                        dx,
                        dy,
                        confidence: 1.0,
                    },
                    // Rejected cells carry garbage that must not leak through.
                    None => Vector {
                        dx: 99.0,
                        dy: -99.0,
                        confidence: 0.0,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn densify_fills_patchy_acceptance_with_nearby_correspondence() {
        let mut field = sparse_field(|x, y| ((x + y) % 3 == 0).then_some((2.0, -1.0)));
        densify(&mut field, DENSIFY_HALF_HEIGHT);
        for v in &field.vectors {
            assert_eq!(v.confidence, 1.0);
            assert!(
                (v.dx - 2.0).abs() < 1e-3 && (v.dy + 1.0).abs() < 1e-3,
                "{v:?}"
            );
        }
    }

    #[test]
    fn densify_fades_an_isolated_match_instead_of_spreading_it() {
        let mut field = sparse_field(|x, y| (x == 128 && y == 32).then_some((6.0, 0.0)));
        densify(&mut field, DENSIFY_HALF_HEIGHT);
        let at = |x: usize, y: usize| field.vectors[y * field.width + x];
        assert!(at(128, 32).dx < 6.0, "a single match is smoothed down");
        // Two fill radii (5.6 degrees, four cells) away there is no correspondence.
        assert!(at(132, 32).dx.abs() < 0.05, "{:?}", at(132, 32));
        assert!(at(128, 0).dx.abs() < 0.6, "{:?}", at(128, 0));
        assert!(field
            .vectors
            .iter()
            .all(|v| v.dx.is_finite() && v.dy.abs() < 1e-6));
    }

    #[test]
    fn densify_wraps_at_the_azimuth_cut() {
        // Accepted columns 0..4 are symmetric about 1.5, so column -1 (= 255)
        // must mirror column 4.
        let mut field = sparse_field(|x, _| (x < 4).then_some((3.0, 0.0)));
        densify(&mut field, DENSIFY_HALF_HEIGHT);
        let at = |x: usize| field.vectors[32 * field.width + x];
        assert!(at(255).dx > 1.0, "{:?}", at(255));
        assert!(
            (at(255).dx - at(4).dx).abs() < 1e-4,
            "{:?} {:?}",
            at(255),
            at(4)
        );
        assert!((at(254).dx - at(5).dx).abs() < 1e-4);
    }

    #[test]
    fn flat_and_masked_pixels_never_generate_flow() {
        let mut image = chart(0.0);
        image.pixels.fill(0.5);
        assert!(correspond(&image, &image, true, &AtomicBool::new(false))
            .vectors
            .iter()
            .all(|v| v.confidence == 0.0));
        let mut image = chart(0.0);
        image.valid.fill(false);
        assert!(correspond(&image, &image, false, &AtomicBool::new(false))
            .vectors
            .iter()
            .all(|v| v.confidence == 0.0));
    }

    #[test]
    fn vertical_motion_wraps_at_azimuth_cut_and_rejects_real_occlusion() {
        // Each azimuth frequency has an integer number of periods: the signal
        // is independently continuous across x=0, including its derivatives.
        let chart = |dx: f32, dy: f32| {
            let (width, height) = (128, 96);
            let pixels = (0..width * height)
                .map(|i| {
                    let x = ((i % width) as f32 - dx) * std::f32::consts::TAU / width as f32;
                    let y = (i / width) as f32 - dy;
                    0.5 + 0.13 * (x * 6.0).sin()
                        + 0.12 * (y * 0.23).sin()
                        + 0.1 * (x * 3.0 + y * 0.41).cos()
                })
                .collect();
            Image {
                width,
                height,
                pixels,
                valid: vec![true; width * height],
            }
        };
        let first = chart(0.0, 0.0);
        let mut second = chart(3.25, -1.5);
        // A bright featureless foreground occluder exists in the second lens
        // only. Its pixels remain valid; this tests correspondence confidence,
        // not the simpler case of an already-known housing exclusion mask.
        for y in 22..74 {
            for x in 44..88 {
                second.pixels[y * 128 + x] = 0.95;
            }
        }
        for dense in [false, true] {
            let field = correspond(&first, &second, dense, &AtomicBool::new(false));
            let cut: Vec<_> = (24..72)
                .flat_map(|y| (0..6).chain(122..128).map(move |x| y * 128 + x))
                .map(|i| field.vectors[i])
                .filter(|v| v.confidence >= 0.5)
                .collect();
            assert!(
                cut.len() > 250,
                "azimuth cut lost support: {} dense={dense}",
                cut.len()
            );
            let error = cut
                .iter()
                .map(|v| (v.dx - 3.25).abs() + (v.dy + 1.5).abs())
                .sum::<f32>()
                / cut.len() as f32;
            assert!(
                error < 0.35,
                "wrapped two-axis motion error={error} dense={dense}"
            );
            let hidden: Vec<_> = (40..56)
                .flat_map(|y| (60..72).map(move |x| y * 128 + x))
                .map(|i| field.vectors[i])
                .collect();
            let rejected = hidden.iter().filter(|v| v.confidence < 0.5).count();
            assert!(
                rejected * 10 >= hidden.len() * 9,
                "occluder was assigned correspondence: {rejected}/{} dense={dense}",
                hidden.len()
            );
        }
    }

    #[test]
    fn sparse_tracks_cover_narrow_valid_overlap_at_the_belt_center() {
        let narrow = |shift| {
            let mut image = chart(shift);
            image.height = 32;
            image.pixels.truncate(image.width * image.height);
            image.valid = (0..image.width * image.height)
                .map(|i| (11..23).contains(&(i / image.width)))
                .collect();
            image
        };
        let field = correspond(&narrow(0.0), &narrow(1.5), false, &AtomicBool::new(false));
        let valid: Vec<_> = (13..21)
            .flat_map(|y| (20..100).map(move |x| y * 128 + x))
            .map(|i| field.vectors[i])
            .filter(|v| v.confidence >= 0.5)
            .collect();
        assert!(
            valid.len() > 300,
            "narrow overlap lost sparse support: {}",
            valid.len()
        );
        let error = valid
            .iter()
            .map(|v| (v.dx - 1.5).abs() + v.dy.abs())
            .sum::<f32>()
            / valid.len() as f32;
        assert!(error < 0.3, "narrow overlap translation error={error}");
    }
}
