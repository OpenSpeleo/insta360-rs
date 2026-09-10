//! Scalar CPU underwater restoration reconstructed from INSCoreMedia 1.10.4.
//!
//! Channel compensation, brightness and atmospheric light follow the scalar
//! `UnderwaterCorrectionCpuV2` path. SIMD and Metal use different fused stages.
//! Public buffers are RGB; the vendor's final integer LUT consumes BGR.

mod guided;

use super::ilut::IntegerLut;
use crate::{Error, Result};

const UPDATE_RATE: f32 = 0.98;

pub(super) struct LegacySession {
    width: usize,
    height: usize,
    strength: f32,
    balance: f32,
    lut: IntegerLut,
    gamma: Option<f32>,
    atmosphere: Option<[f32; 3]>,
    previous_pts: Option<f64>,
    small: Vec<[u8; 3]>,
    guide: Vec<f32>,
    field: Vec<f32>,
    scratch: Vec<f32>,
    filtered: Vec<f32>,
}

impl LegacySession {
    pub(super) fn new(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        strength: f32,
        balance: f32,
        lut: IntegerLut,
    ) -> Result<Self> {
        crate::UnderwaterColorOptions {
            mode: crate::UnderwaterColorMode::Legacy,
            strength: Some(strength),
            balance: Some(balance),
            style: None,
        }
        .validate_dimensions(width, height, fps_num, fps_den)?;
        let width = width as usize;
        let height = height as usize;
        let small_len = (width / 4) * (height / 4);
        Ok(Self {
            width,
            height,
            strength,
            balance,
            lut: lut.blended(strength),
            gamma: None,
            atmosphere: None,
            previous_pts: None,
            small: allocate(small_len, [0; 3])?,
            guide: allocate(small_len, 0.0)?,
            field: allocate(small_len, 0.0)?,
            scratch: allocate(small_len, 0.0)?,
            filtered: allocate(small_len, 0.0)?,
        })
    }

    pub(super) fn reset(&mut self) {
        self.gamma = None;
        self.atmosphere = None;
        self.previous_pts = None;
    }

    pub(super) fn process_rgb8(&mut self, pixels: &mut [u8], pts_seconds: f64) -> Result<()> {
        if pixels.len() != self.width * self.height * 3 || !pts_seconds.is_finite() {
            return Err(invalid(
                "legacy underwater frame dimensions or timestamp are invalid",
            ));
        }
        if self
            .previous_pts
            .is_some_and(|previous| pts_seconds <= previous)
        {
            self.reset();
        }
        self.previous_pts = Some(pts_seconds);
        // Native temporal smoothing is per processed frame, independent of FPS.
        // Random seeks start a fresh state; callers reset at recording changes.
        if self.strength == 0.0 {
            return Ok(());
        }
        let Some(compensation) = ChannelCompensation::prepare(
            pixels,
            self.width,
            self.height,
            self.strength,
            self.balance,
        ) else {
            // Native ChannelCompensate sets its bypass flag for degenerate
            // channel means, and ProcessFrame bypasses every subsequent stage.
            return Ok(());
        };
        for pixel in pixels.chunks_exact_mut(3) {
            pixel.copy_from_slice(&compensation.apply([pixel[0], pixel[1], pixel[2]]));
        }
        brighten(pixels, self.strength, &mut self.gamma);
        self.remove_haze(pixels)?;
        for pixel in pixels.chunks_exact_mut(3) {
            let output = self.lut.sample([pixel[2], pixel[1], pixel[0]]);
            pixel.copy_from_slice(&[output[2], output[1], output[0]]);
        }
        Ok(())
    }

    fn remove_haze(&mut self, pixels: &mut [u8]) -> Result<()> {
        let (width, height) = (self.width / 4, self.height / 4);
        resize_rgb(
            pixels,
            self.width,
            self.height,
            &mut self.small,
            width,
            height,
        );
        for (index, pixel) in self.small.iter().enumerate() {
            self.guide[index] = f32::from(gray(*pixel));
            self.field[index] = f32::from(*pixel.iter().min().expect("RGB channels"));
        }
        // 0x1f67464..94: square kernel 2*floor(min(width,height)/54)+1.
        let radius = width.min(height) / 54;
        morph_square(
            &self.field,
            &mut self.scratch,
            &mut self.filtered,
            width,
            height,
            radius,
            false,
        );
        let current = atmospheric_light(&self.small, &self.guide, &self.filtered, width, height);
        let atmosphere = match self.atmosphere {
            Some(previous) if previous != [0.0; 3] => std::array::from_fn(|channel| {
                previous[channel].mul_add(UPDATE_RATE, current[channel] * (1.0 - UPDATE_RATE))
            }),
            _ => current,
        };
        self.atmosphere = Some(atmosphere);
        for (value, pixel) in self.field.iter_mut().zip(&self.small) {
            let ratio = (0..3)
                .filter(|channel| atmosphere[*channel] > 0.0)
                .map(|channel| f32::from(pixel[channel]) / atmosphere[channel])
                .fold(f32::INFINITY, f32::min);
            // A zero atmospheric component carries no attenuation estimate.
            // Omit that component instead of allowing 0/0 to poison the frame.
            *value = if ratio.is_finite() {
                (-(0.45 * self.strength)).mul_add(ratio, 1.0)
            } else {
                1.0
            };
        }
        // TransmissionMap uses dilation, unlike the dark-channel erosion.
        morph_square(
            &self.field,
            &mut self.scratch,
            &mut self.filtered,
            width,
            height,
            radius,
            true,
        );
        let transmission = guided::filter(&self.guide, &self.filtered, width, height)?;
        for y in 0..self.height {
            let yy = resize_coordinate(y, self.height, height);
            for x in 0..self.width {
                let xx = resize_coordinate(x, self.width, width);
                let transmission = bilinear(&transmission, width, height, xx, yy);
                let pixel = &mut pixels[(y * self.width + x) * 3..][..3];
                pixel.copy_from_slice(&remove_water(
                    [pixel[0], pixel[1], pixel[2]],
                    atmosphere,
                    transmission,
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ChannelCompensation {
    unchanged: usize,
    compensated: usize,
    other_gain: f32,
    red_from_blue: f32,
    red_from_green: f32,
}

impl ChannelCompensation {
    fn prepare(
        pixels: &[u8],
        width: usize,
        height: usize,
        strength: f32,
        balance: f32,
    ) -> Option<Self> {
        let mut histogram = [[0_u32; 256]; 3];
        // ChannelCompensate 0x1f666e0..76c samples every fourth row/column.
        for y in (0..height).step_by(4) {
            for x in (0..width).step_by(4) {
                let pixel = &pixels[(y * width + x) * 3..][..3];
                for channel in 0..3 {
                    histogram[channel][usize::from(pixel[channel])] += 1;
                }
            }
        }
        let means = histogram.map(|bins| {
            let count = bins.iter().map(|count| *count as f32).sum::<f32>();
            bins.iter()
                .enumerate()
                .map(|(value, count)| (value as u32 * count) as f32)
                .sum::<f32>()
                / count
        });
        Self::from_means(means, strength, balance)
    }

    fn from_means([red, green, blue]: [f32; 3], strength: f32, balance: f32) -> Option<Self> {
        if green < 0.1 || blue < 0.1 || red > 254.0 {
            return None;
        }
        let (unchanged, compensated, strong, weak, amount) = if green >= blue {
            (1, 2, green, blue, 0.6)
        } else {
            (2, 1, blue, green, 0.3)
        };
        let delta = 2.0 * (balance - 0.5);
        let adjust_difference = |value: f32| {
            if value > 60.0 {
                value
            } else {
                (1.5 * (value - 20.0)).max(0.0)
            }
        };
        Some(Self {
            unchanged,
            compensated,
            // Equal saturated channels have no difference to compensate. Avoid
            // the native 0*(gain/0) indeterminate expression for that case.
            other_gain: if strong == weak {
                0.0
            } else {
                (strong - weak) * ((amount * strength) / (255.0 - weak) / strong)
            },
            red_from_blue: adjust_difference(blue - red)
                * (((-1.0 + delta) * strength) / (255.0 - red) / blue),
            red_from_green: adjust_difference(green - red)
                * (((2.0 - delta) * strength) / (255.0 - red) / green),
        })
    }

    fn apply(&self, pixel: [u8; 3]) -> [u8; 3] {
        let mut output = pixel;
        let weak = f32::from(pixel[self.compensated]);
        let strong = f32::from(pixel[self.unchanged]);
        output[self.compensated] = byte((self.other_gain * (255.0 - weak)).mul_add(strong, weak));
        let red = f32::from(pixel[0]);
        let red_gain = self.red_from_blue.mul_add(
            f32::from(pixel[2]),
            self.red_from_green * f32::from(pixel[1]),
        );
        output[0] = byte((255.0 - red).mul_add(red_gain, red));
        output
    }
}

fn brighten(pixels: &mut [u8], strength: f32, previous: &mut Option<f32>) {
    let sum = pixels
        .chunks_exact(3)
        .map(|pixel| u64::from(gray([pixel[0], pixel[1], pixel[2]])))
        .sum::<u64>();
    let mean = sum as f64 / (pixels.len() / 3) as f64 / 255.0;
    if mean <= 0.0 || mean >= 1.0 {
        return;
    }
    let target = (mean * 1.5).min(0.55);
    let current = target.ln() / mean.ln();
    let gamma = match *previous {
        Some(value) => {
            current.mul_add(f64::from(1.0 - UPDATE_RATE), f64::from(value * UPDATE_RATE)) as f32
        }
        None => current as f32,
    };
    *previous = Some(gamma);
    let exponent = gamma.mul_add(strength, 1.0 - strength) - 1.0;
    if exponent >= 0.0 {
        return;
    }
    let delta: [f32; 256] = std::array::from_fn(|value| {
        if value == 0 {
            0.0
        } else {
            (((value as f64 / 255.0) as f32).powf(exponent) - 1.0).min(2.0) * value as f32
        }
    });
    for pixel in pixels.chunks_exact_mut(3) {
        let offset = delta[usize::from(gray([pixel[0], pixel[1], pixel[2]]))];
        for channel in pixel {
            *channel = byte(f32::from(*channel) + offset);
        }
    }
}

fn atmospheric_light(
    pixels: &[[u8; 3]],
    guide: &[f32],
    dark: &[f32],
    width: usize,
    height: usize,
) -> [f32; 3] {
    let mut histogram = [0_u32; 256];
    let mut count = 0_u32;
    for y in (0..height).step_by(4) {
        for x in (0..width).step_by(4) {
            histogram[dark[y * width + x] as usize] += 1;
            count += 1;
        }
    }
    // UpperBound 0x1b74c18..68 takes trunc(count*.001+1) samples from above.
    let needed = (count as f32 * 0.001 + 1.0) as u32;
    let mut accumulated = 0;
    let threshold = (0..=255)
        .rev()
        .find(|value| {
            accumulated += histogram[*value];
            accumulated >= needed
        })
        .unwrap_or(0) as f32;
    let mut index = 0;
    let mut brightest = 0.0;
    for candidate in 0..pixels.len() {
        if dark[candidate] >= threshold && guide[candidate] > brightest {
            index = candidate;
            brightest = guide[candidate];
        }
    }
    pixels[index].map(f32::from)
}

/// Separable square min/max filter, with neutral exterior pixels as in OpenCV.
fn morph_square(
    input: &[f32],
    scratch: &mut [f32],
    output: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    maximum: bool,
) {
    use std::collections::VecDeque;
    let mut queue = VecDeque::<(usize, f32)>::with_capacity(2 * radius + 2);
    for vertical in [false, true] {
        let (lines, length) = if vertical {
            (width, height)
        } else {
            (height, width)
        };
        for line in 0..lines {
            queue.clear();
            let mut next = 0;
            for position in 0..length {
                let end = (position + radius + 1).min(length);
                while next < end {
                    let value = if vertical {
                        scratch[next * width + line]
                    } else {
                        input[line * width + next]
                    };
                    while queue.back().is_some_and(|(_, old)| {
                        if maximum {
                            *old <= value
                        } else {
                            *old >= value
                        }
                    }) {
                        queue.pop_back();
                    }
                    queue.push_back((next, value));
                    next += 1;
                }
                let begin = position.saturating_sub(radius);
                while queue.front().is_some_and(|(index, _)| *index < begin) {
                    queue.pop_front();
                }
                let value = queue.front().expect("nonempty morphology window").1;
                if vertical {
                    output[position * width + line] = value;
                } else {
                    scratch[line * width + position] = value;
                }
            }
        }
    }
}

fn resize_rgb(
    input: &[u8],
    width: usize,
    height: usize,
    output: &mut [[u8; 3]],
    out_width: usize,
    out_height: usize,
) {
    for y in 0..out_height {
        let yy = resize_coordinate(y, out_height, height);
        let y0 = yy.floor() as usize;
        let y1 = (y0 + 1).min(height - 1);
        let dy = yy - y0 as f32;
        let wy = [(1.0 - dy) * 2048.0, dy * 2048.0].map(|v| v.round_ties_even() as i32);
        for x in 0..out_width {
            let xx = resize_coordinate(x, out_width, width);
            let x0 = xx.floor() as usize;
            let x1 = (x0 + 1).min(width - 1);
            let dx = xx - x0 as f32;
            let wx = [(1.0 - dx) * 2048.0, dx * 2048.0].map(|v| v.round_ties_even() as i32);
            output[y * out_width + x] = std::array::from_fn(|channel| {
                let at = |x, y| i32::from(input[(y * width + x) * 3 + channel]);
                let top = at(x0, y0) * wx[0] + at(x1, y0) * wx[1];
                let bottom = at(x0, y1) * wx[0] + at(x1, y1) * wx[1];
                // OpenCV INTER_LINEAR's 8-bit scalar path uses 11-bit
                // coefficients and drops intermediate low bits before its
                // final half-up rounding (resize.cpp, VResizeLinear<uchar>).
                let top = (wy[0] * (top >> 4)) >> 16;
                let bottom = (wy[1] * (bottom >> 4)) >> 16;
                ((top + bottom + 2) >> 2) as u8
            });
        }
    }
}

fn resize_coordinate(position: usize, destination: usize, source: usize) -> f32 {
    (((position as f64 + 0.5) * source as f64 / destination as f64 - 0.5) as f32)
        .clamp(0.0, (source - 1) as f32)
}

fn bilinear(input: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (dx, dy) = (x - x0 as f32, y - y0 as f32);
    let top = input[y0 * width + x0] * (1.0 - dx) + input[y0 * width + x1] * dx;
    let bottom = input[y1 * width + x0] * (1.0 - dx) + input[y1 * width + x1] * dx;
    top * (1.0 - dy) + bottom * dy
}

fn gray([red, green, blue]: [u8; 3]) -> u8 {
    // OpenCV 4.x RGB2Gray<uchar>, gray_shift=15 (color.simd_helpers.hpp).
    ((u32::from(red) * 9798 + u32::from(green) * 19235 + u32::from(blue) * 3735 + (1 << 14)) >> 15)
        as u8
}

fn remove_water(pixel: [u8; 3], atmosphere: [f32; 3], transmission: f32) -> [u8; 3] {
    let transmission = transmission.max(0.3);
    // Scalar worker 0x1f6b190..200 divides before adding atmospheric light;
    // reciprocal multiplication can change the final nearest-even byte.
    std::array::from_fn(|c| {
        byte((f32::from(pixel[c]) - atmosphere[c]) / transmission + atmosphere[c])
    })
}

fn byte(value: f32) -> u8 {
    value.round_ties_even().clamp(0.0, 255.0) as u8
}

fn allocate<T: Clone>(length: usize, value: T) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| invalid("cannot allocate legacy underwater workspace"))?;
    values.resize(length, value);
    Ok(values)
}

fn invalid(message: &str) -> Error {
    Error::InvalidMedia(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant_lut(value: [u8; 3]) -> IntegerLut {
        let mut bytes = [16_u32, 256, 256]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        for _ in 0..8 {
            bytes.extend(value);
        }
        IntegerLut::parse(&bytes).unwrap()
    }

    fn session(strength: f32) -> LegacySession {
        LegacySession::new(64, 64, 30, 1, strength, 0.5, constant_lut([11, 22, 33])).unwrap()
    }

    #[test]
    fn channel_compensation_matches_independent_uniform_color_arithmetic() {
        // At uniform pixels the mean denominators cancel: in the first case
        // R gains -60+2*80=100, B gains .6*(100-80)=12. In the second R
        // gains -80+2*60=40, G gains .3*(100-80)=6.
        for (pixel, expected) in [
            ([20, 100, 80], [120, 100, 92]),
            ([20, 80, 100], [60, 86, 100]),
            ([100, 100, 100], [100, 100, 100]),
            ([20, 255, 255], [255, 255, 255]),
        ] {
            let correction =
                ChannelCompensation::from_means(pixel.map(f32::from), 1.0, 0.5).unwrap();
            assert_eq!(correction.apply(pixel), expected);
        }
        let half = ChannelCompensation::from_means([20.0, 100.0, 80.0], 0.5, 0.5).unwrap();
        assert_eq!(half.apply([20, 100, 80]), [70, 100, 86]);
        // Balance changes the two red contributions without touching B/G.
        let warm = ChannelCompensation::from_means([20.0, 100.0, 80.0], 1.0, 0.0).unwrap();
        let cool = ChannelCompensation::from_means([20.0, 100.0, 80.0], 1.0, 1.0).unwrap();
        assert_eq!(warm.apply([20, 100, 80]), [140, 100, 92]);
        assert_eq!(cool.apply([20, 100, 80]), [100, 100, 92]);
    }

    #[test]
    fn channel_means_use_every_fourth_row_and_column_and_native_bypass_thresholds() {
        let mut pixels = [250, 1, 1].repeat(8 * 8);
        for y in [0, 4] {
            for x in [0, 4] {
                pixels[(y * 8 + x) * 3..][..3].copy_from_slice(&[20, 100, 80]);
            }
        }
        let correction = ChannelCompensation::prepare(&pixels, 8, 8, 1.0, 0.5).unwrap();
        assert_eq!(correction.apply([20, 100, 80]), [120, 100, 92]);
        for means in [
            [0.0, 0.0, 0.0],
            [10.0, 0.09, 100.0],
            [10.0, 100.0, 0.09],
            [254.1, 100.0, 100.0],
        ] {
            assert!(ChannelCompensation::from_means(means, 1.0, 0.5).is_none());
        }
        assert!(ChannelCompensation::from_means([254.0, 0.1, 0.1], 1.0, 0.5).is_some());
    }

    #[test]
    fn grayscale_primary_colors_and_rounding_are_unambiguous() {
        assert_eq!(
            [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]].map(gray),
            [76, 150, 29, 255]
        );
        assert_eq!(
            [0.5, 1.5, 2.5, 254.5, -3.0, 300.0].map(byte),
            [0, 2, 2, 254, 0, 255]
        );
    }

    #[test]
    fn water_removal_uses_atmospheric_model_transmission_floor_and_saturation() {
        assert_eq!(
            remove_water([160, 80, 100], [100.0; 3], 0.5),
            [220, 60, 100]
        );
        assert_eq!(
            remove_water([130, 70, 100], [100.0; 3], 0.01),
            [200, 0, 100]
        );
        assert_eq!(remove_water([255, 0, 100], [100.0; 3], 0.3), [255, 0, 100]);
        assert_eq!(
            remove_water([160, 80, 100], [100.0; 3], 1.0),
            [160, 80, 100]
        );
    }

    #[test]
    fn brightness_reaches_native_target_and_smooths_gamma_per_frame() {
        let mut previous = None;
        let mut gray100 = vec![100; 3 * 64];
        brighten(&mut gray100, 1.0, &mut previous);
        assert!(gray100.iter().all(|v| *v == 140)); // .55*255=140.25
        let first = previous.unwrap();
        let mut gray30 = vec![30; 3 * 64];
        brighten(&mut gray30, 1.0, &mut previous);
        let incoming = (45.0_f64 / 255.0).ln() / (30.0_f64 / 255.0).ln();
        let expected =
            incoming.mul_add(f64::from(1.0 - UPDATE_RATE), f64::from(first * UPDATE_RATE)) as f32;
        assert_eq!(previous.unwrap(), expected);
        assert!(previous.unwrap() > first);
        let mut bright = vec![200; 3 * 64];
        brighten(&mut bright, 1.0, &mut None);
        assert_eq!(bright, vec![200; 3 * 64]); // gamma>=1 is never darkened
        let mut colorful = [40, 60, 80].repeat(64);
        brighten(&mut colorful, 1.0, &mut None);
        for pixel in colorful.chunks_exact(3) {
            assert_eq!(pixel[1] - pixel[0], 20);
            assert_eq!(pixel[2] - pixel[1], 20);
        }
    }

    #[test]
    fn morphology_matches_naive_two_dimensional_windows_at_all_borders() {
        for width in 1..=9 {
            for height in 1..=7 {
                for radius in [0, 1, 3, 10] {
                    for maximum in [false, true] {
                        let input = (0..width * height)
                            .map(|i| ((i * 37 + i * i * 13) % 101) as f32)
                            .collect::<Vec<_>>();
                        let mut scratch = vec![0.0; input.len()];
                        let mut output = scratch.clone();
                        morph_square(
                            &input,
                            &mut scratch,
                            &mut output,
                            width,
                            height,
                            radius,
                            maximum,
                        );
                        for y in 0..height {
                            for x in 0..width {
                                let mut expected = if maximum {
                                    f32::NEG_INFINITY
                                } else {
                                    f32::INFINITY
                                };
                                for yy in y.saturating_sub(radius)..=(y + radius).min(height - 1) {
                                    for xx in x.saturating_sub(radius)..=(x + radius).min(width - 1)
                                    {
                                        let value = input[yy * width + xx];
                                        expected = if maximum {
                                            expected.max(value)
                                        } else {
                                            expected.min(value)
                                        };
                                    }
                                }
                                assert_eq!(
                                    output[y * width + x],
                                    expected,
                                    "{width}x{height}, radius{radius}, maximum{maximum}, ({x},{y})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn atmosphere_uses_sampled_upper_tail_then_brightest_eligible_pixel() {
        let mut pixels = vec![[10, 10, 10]; 64];
        let mut guide = vec![10.0; 64];
        let mut dark = guide.clone();
        // Only (0,0),(4,0),(0,4),(4,4) contribute to the threshold. Its
        // highest sampled value is80; unsampled pixel1 is also eligible.
        pixels[0] = [80, 80, 80];
        guide[0] = 80.0;
        dark[0] = 80.0;
        pixels[1] = [200, 100, 90];
        guide[1] = 129.0;
        dark[1] = 90.0;
        pixels[2] = [250, 250, 20];
        guide[2] = 224.0;
        dark[2] = 20.0;
        assert_eq!(
            atmospheric_light(&pixels, &guide, &dark, 8, 8),
            [200.0, 100.0, 90.0]
        );
        pixels[3] = [90, 200, 90];
        guide[3] = 129.0;
        dark[3] = 90.0;
        assert_eq!(
            atmospheric_light(&pixels, &guide, &dark, 8, 8),
            [200.0, 100.0, 90.0]
        );
    }

    #[test]
    fn quarter_size_interpolation_uses_center_samples_and_half_up_rounding() {
        let mut pixels = vec![0_u8; 8 * 8 * 3];
        // First destination center is (1.5,1.5), not the 4x4 block mean.
        // Four neighbors average [0.5,1.5,2.5], rounded up for CV_8U resize.
        for (x, y, value) in [
            (1, 1, [0, 1, 2]),
            (2, 1, [1, 2, 3]),
            (1, 2, [0, 1, 2]),
            (2, 2, [1, 2, 3]),
        ] {
            pixels[(y * 8 + x) * 3..][..3].copy_from_slice(&value);
        }
        pixels[..3].fill(255);
        let mut output = vec![[0; 3]; 4];
        resize_rgb(&pixels, 8, 8, &mut output, 2, 2);
        assert_eq!(output, [[1, 2, 3], [0, 0, 0], [0, 0, 0], [0, 0, 0]]);
        assert_eq!(resize_coordinate(0, 4, 2), 0.0);
        assert_eq!(resize_coordinate(3, 4, 2), 1.0);
        assert_eq!(bilinear(&[1.0, 3.0, 5.0, 7.0], 2, 2, 0.25, 0.75), 4.5);
    }

    #[test]
    fn legacy_pipeline_uses_bgr_lut_and_preserves_native_full_bypass() {
        let mut restored = [20, 100, 80].repeat(64 * 64);
        session(1.0).process_rgb8(&mut restored, 0.0).unwrap();
        assert!(restored.chunks_exact(3).all(|pixel| pixel == [33, 22, 11]));
        for color in [[0, 0, 0], [255, 255, 255], [20, 0, 80], [20, 100, 0]] {
            let mut pixels = color.repeat(64 * 64);
            let expected = pixels.clone();
            session(1.0).process_rgb8(&mut pixels, 0.0).unwrap();
            assert_eq!(pixels, expected);
        }
        let mut pixels = [20, 100, 80].repeat(64 * 64);
        let expected = pixels.clone();
        session(0.0).process_rgb8(&mut pixels, 0.0).unwrap();
        assert_eq!(pixels, expected);
    }

    #[test]
    fn uniform_frame_matches_independently_derived_complete_pipeline() {
        let mut bytes = [16_u32, 256, 4]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        for b in 0..65 {
            for g in 0..65 {
                for r in 0..65 {
                    bytes.extend([(b * 3) as u8, (g * 2) as u8, r as u8]);
                }
            }
        }
        let lut = IntegerLut::parse(&bytes).unwrap();
        let mut current = LegacySession::new(64, 64, 30, 1, 1.0, 0.5, lut).unwrap();
        let mut pixels = [20, 100, 80].repeat(64 * 64);
        current.process_rgb8(&mut pixels, 0.0).unwrap();
        // Compensation gives RGB(120,100,92), whose integer gray is105.
        // Brightness targets .55*255=140.25, adding35.25 to each channel:
        // RGB(155,135,127). Uniform haze has I=A so water removal preserves
        // these values for every transmission. The affine BGR table then gives
        // floor(B*.75), floor(G*.5), floor(R*.25) = BGR(95,67,38).
        assert!(pixels.chunks_exact(3).all(|pixel| pixel == [38, 67, 95]));
    }

    #[test]
    fn reset_and_non_increasing_timestamps_restart_temporal_state_with_bounded_storage() {
        let mut current = session(1.0);
        let pointers = (
            current.small.as_ptr(),
            current.guide.as_ptr(),
            current.field.as_ptr(),
            current.scratch.as_ptr(),
            current.filtered.as_ptr(),
        );
        for frame in 0..100 {
            let color = if frame % 2 == 0 {
                [20, 100, 80]
            } else {
                [10, 30, 40]
            };
            let mut pixels = color.repeat(64 * 64);
            current
                .process_rgb8(&mut pixels, f64::from(frame) / 30.0)
                .unwrap();
        }
        assert_eq!(
            pointers,
            (
                current.small.as_ptr(),
                current.guide.as_ptr(),
                current.field.as_ptr(),
                current.scratch.as_ptr(),
                current.filtered.as_ptr()
            )
        );
        let mut fresh = session(1.0);
        let mut first = [10, 30, 40].repeat(64 * 64);
        fresh.process_rgb8(&mut first, 0.0).unwrap();
        let mut repeated = [10, 30, 40].repeat(64 * 64);
        current.process_rgb8(&mut repeated, 0.0).unwrap();
        assert_eq!(current.gamma, fresh.gamma);
        assert_eq!(current.atmosphere, fresh.atmosphere);
        assert_eq!(first, repeated);
        current.reset();
        assert_eq!(
            (current.gamma, current.atmosphere, current.previous_pts),
            (None, None, None)
        );
    }

    #[test]
    fn invalid_preparation_and_frames_fail_before_mutation() {
        for (width, height, fps_num, fps_den, strength, balance) in [
            (63, 64, 30, 1, 1.0, 0.5),
            (64, 63, 30, 1, 1.0, 0.5),
            (u32::MAX, u32::MAX, 30, 1, 1.0, 0.5),
            (64, 64, 0, 1, 1.0, 0.5),
            (64, 64, 30, 0, 1.0, 0.5),
            (64, 64, 30, 1, f32::NAN, 0.5),
            (64, 64, 30, 1, 1.0, 1.1),
        ] {
            assert!(LegacySession::new(
                width,
                height,
                fps_num,
                fps_den,
                strength,
                balance,
                constant_lut([1, 2, 3])
            )
            .is_err());
        }
        let mut current = session(1.0);
        let mut pixels = [20, 100, 80].repeat(64 * 64);
        let expected = pixels.clone();
        for pts in [f64::NAN, f64::INFINITY] {
            assert!(current.process_rgb8(&mut pixels, pts).is_err());
        }
        assert!(current.process_rgb8(&mut pixels[..3], 0.0).is_err());
        assert_eq!(pixels, expected);
        assert_eq!(current.previous_pts, None);
    }
}
