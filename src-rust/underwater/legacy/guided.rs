//! Native monochrome fast guided filter used by legacy haze removal.
//!
//! INSCoreMedia 1.10.4 `GuidedFilterMono` at 0x1b6797c downsamples by two
//! with nearest sampling, averages over a 31×31 REFLECT_101 window, then
//! upsamples the linear coefficients with bilinear sampling. Its float path
//! scales both inputs by 255 and epsilon by 255²; only the intercept is
//! scaled back before applying the coefficients to the original guide.

use crate::Result;

const RADIUS: isize = 15;
const SIDE: f64 = 31.0;
const EPSILON: f32 = 0.01 * 255.0 * 255.0;

pub(super) fn filter(
    guide: &[f32],
    transmission: &[f32],
    width: usize,
    height: usize,
) -> Result<Vec<f32>> {
    // LegacySession validates the full dimensions before allocating these
    // quarter-size inputs, so the reduced dimensions are always nonzero.
    debug_assert!(width >= 2 && height >= 2);
    debug_assert_eq!(guide.len(), width * height);
    debug_assert_eq!(transmission.len(), guide.len());
    let (small_width, small_height) = (width / 2, height / 2);
    let count = small_width * small_height;
    let mut guide_small = super::allocate(count, 0.0)?;
    let mut source_small = super::allocate(count, 0.0)?;
    for y in 0..small_height {
        for x in 0..small_width {
            let source = (y * height / small_height) * width + x * width / small_width;
            guide_small[y * small_width + x] = guide[source] * 255.0;
            source_small[y * small_width + x] = transmission[source] * 255.0;
        }
    }
    let mut scratch = super::allocate(count, 0.0_f64)?;
    let mut mean_guide = super::allocate(count, 0.0)?;
    let mut mean_source = super::allocate(count, 0.0)?;
    let mut a = super::allocate(count, 0.0)?;
    let mut b = super::allocate(count, 0.0)?;
    box_mean(
        &guide_small,
        None,
        &mut mean_guide,
        &mut scratch,
        small_width,
        small_height,
    );
    box_mean(
        &source_small,
        None,
        &mut mean_source,
        &mut scratch,
        small_width,
        small_height,
    );
    box_mean(
        &guide_small,
        Some(&guide_small),
        &mut a,
        &mut scratch,
        small_width,
        small_height,
    );
    box_mean(
        &guide_small,
        Some(&source_small),
        &mut b,
        &mut scratch,
        small_width,
        small_height,
    );
    for index in 0..count {
        let variance = a[index] - mean_guide[index] * mean_guide[index];
        let covariance = b[index] - mean_guide[index] * mean_source[index];
        a[index] = covariance / (variance + EPSILON);
        b[index] = mean_source[index] - a[index] * mean_guide[index];
    }
    // Reuse the means as coefficient outputs instead of retaining another
    // pair of image-sized buffers. Scratch is bounded by the reduced image.
    box_mean(
        &a,
        None,
        &mut mean_guide,
        &mut scratch,
        small_width,
        small_height,
    );
    box_mean(
        &b,
        None,
        &mut mean_source,
        &mut scratch,
        small_width,
        small_height,
    );
    for value in &mut mean_source {
        *value *= 1.0 / 255.0;
    }
    let mut output = super::allocate(guide.len(), 0.0)?;
    for y in 0..height {
        let yy = super::resize_coordinate(y, height, small_height);
        for x in 0..width {
            let xx = super::resize_coordinate(x, width, small_width);
            let a = super::bilinear(&mean_guide, small_width, small_height, xx, yy);
            let b = super::bilinear(&mean_source, small_width, small_height, xx, yy);
            output[y * width + x] = a * guide[y * width + x] + b;
        }
    }
    Ok(output)
}

fn reflect(index: isize, length: usize) -> usize {
    if length == 1 {
        return 0;
    }
    let period = 2 * (length as isize - 1);
    let index = index.rem_euclid(period) as usize;
    index.min(period as usize - index)
}

/// Two sliding sums preserve the full reflected window, including when the
/// kernel exceeds an image dimension. Time and storage are linear in pixels.
fn box_mean(
    input: &[f32],
    product: Option<&[f32]>,
    output: &mut [f32],
    scratch: &mut [f64],
    width: usize,
    height: usize,
) {
    let sample = |index: usize| {
        // OpenCV materializes products at the input's float precision before
        // the normalized box filter accumulates them in double precision.
        f64::from(product.map_or(input[index], |other| input[index] * other[index]))
    };
    for y in 0..height {
        let mut sum: f64 = (-RADIUS..=RADIUS)
            .map(|x| sample(y * width + reflect(x, width)))
            .sum();
        for x in 0..width {
            scratch[y * width + x] = sum;
            sum += sample(y * width + reflect(x as isize + RADIUS + 1, width))
                - sample(y * width + reflect(x as isize - RADIUS, width));
        }
    }
    for x in 0..width {
        let mut sum: f64 = (-RADIUS..=RADIUS)
            .map(|y| scratch[reflect(y, height) * width + x])
            .sum();
        for y in 0..height {
            output[y * width + x] = (sum / (SIDE * SIDE)) as f32;
            sum += scratch[reflect(y as isize + RADIUS + 1, height) * width + x]
                - scratch[reflect(y as isize - RADIUS, height) * width + x];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflected_window_matches_independent_two_dimensional_reference() {
        for (width, height) in [(1, 1), (2, 3), (9, 7), (37, 35)] {
            let input: Vec<_> = (0..width * height)
                .map(|i| (i * 13 % 37) as f32 / 7.0)
                .collect();
            let mut actual = vec![0.0; input.len()];
            let mut scratch = vec![0.0; input.len()];
            for product in [None, Some(input.as_slice())] {
                box_mean(&input, product, &mut actual, &mut scratch, width, height);
                let fold = |mut value: isize, length: usize| {
                    if length == 1 {
                        return 0;
                    }
                    while value < 0 || value >= length as isize {
                        value = if value < 0 {
                            -value
                        } else {
                            2 * length as isize - value - 2
                        };
                    }
                    value as usize
                };
                for y in 0..height {
                    for x in 0..width {
                        let mut sum = 0.0_f64;
                        for dy in -15..=15 {
                            for dx in -15..=15 {
                                let index = fold(y as isize + dy, height) * width
                                    + fold(x as isize + dx, width);
                                let value = input[index];
                                sum += f64::from(if product.is_some() {
                                    value * value
                                } else {
                                    value
                                });
                            }
                        }
                        assert_eq!(actual[y * width + x], (sum / 961.0) as f32);
                    }
                }
            }
        }
    }

    #[test]
    fn constant_transmission_survives_edges_odd_sizes_and_repeated_calls() {
        for (width, height) in [(16, 16), (17, 19), (64, 32)] {
            for step in 0..64 {
                let guide: Vec<_> = (0..width * height)
                    .map(|i| ((i * 31 + step) % 256) as f32)
                    .collect();
                let output = filter(&guide, &vec![0.5; guide.len()], width, height).unwrap();
                assert!(output.iter().all(|value| (*value - 0.5).abs() < 1e-6));
            }
        }
    }

    #[test]
    fn constant_guide_uses_two_reflected_means_and_nearest_reduction() {
        // The native nearest resize selects the even columns of this pattern.
        // Bilinear or area reduction would instead include the odd-column 1s.
        let (width, height) = (16, 16);
        let source: Vec<_> = (0..width * height)
            .map(|i| if i % width % 2 == 0 { 0.25 } else { 1.0 })
            .collect();
        let output = filter(&vec![120.0; source.len()], &source, width, height).unwrap();
        assert!(output.iter().all(|value| (*value - 0.25).abs() < 1e-6));
    }

    #[test]
    fn varying_guide_and_transmission_match_independent_scalar_reference() {
        // Golden values from a direct f64 Python reference: nested 31×31
        // reflected windows, unscaled inputs/epsilon=.01, and independently
        // computed half-pixel bilinear coordinates. The implementation uses
        // the native float intermediates, hence the explicit tolerance.
        let (width, height) = (17, 19);
        let guide: Vec<_> = (0..height)
            .flat_map(|y| (0..width).map(move |x| ((x * 11 + y * 17) % 256) as f32))
            .collect();
        let source: Vec<_> = (0..height)
            .flat_map(|y| (0..width).map(move |x| 0.2 + ((x / 3 + y / 5) % 3) as f32 * 0.25))
            .collect();
        let output = filter(&guide, &source, width, height).unwrap();
        for (x, y, expected) in [
            (0, 0, 0.456_181_98),
            (16, 0, 0.442_833_95),
            (0, 18, 0.452_714_3),
            (16, 18, 0.438_477_2),
            (8, 9, 0.436_775_48),
            (3, 7, 0.445_510_75),
            (13, 11, 0.451_880_25),
        ] {
            assert!((output[y * width + x] - expected).abs() < 2e-6);
        }
    }
}
