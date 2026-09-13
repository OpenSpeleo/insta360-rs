//! Integer 3D lookup tables used by the legacy underwater restoration stage.
//!
//! The byte layout and signed integer interpolation follow the installed Studio
//! 5.9.10 resource and the `RemoveWaterThenLut` shader. These tables use the third
//! channel as the fastest dimension; they are not interchangeable with CUBE LUTs.

use crate::{Error, Result};
use rayon::prelude::*;

const BLOCK_BYTES: usize = 4096 * 3;

#[derive(Clone, Debug)]
pub(crate) struct IntegerLut {
    step: usize,
    edge: usize,
    table: Vec<[u8; 3]>,
}

impl IntegerLut {
    #[cfg(all(
        feature = "underwater-ai",
        any(test, all(feature = "gpu", feature = "media"))
    ))]
    pub(super) fn entries(&self) -> &[[u8; 3]] {
        &self.table
    }

    /// Applies an RGB table to packed RGB pixels without allocating a frame.
    #[cfg(any(feature = "underwater-ai", test))]
    pub(crate) fn apply_rgb8(&self, pixels: &mut [u8]) {
        self.apply::<false>(pixels);
    }

    /// Legacy ILUT coordinates and results are BGR; public pixels stay RGB.
    pub(crate) fn apply_bgr_table_to_rgb8(&self, pixels: &mut [u8]) {
        self.apply::<true>(pixels);
    }

    fn apply<const REVERSE: bool>(&self, pixels: &mut [u8]) {
        // Dispatch once per frame. Both shipped grids have power-of-two steps;
        // their terminal vertices also make per-corner clamping unnecessary.
        match self.step {
            4 => apply_pixels::<REVERSE>(pixels, |rgb| self.sample_power_of_two::<2, 65>(rgb)),
            16 => apply_pixels::<REVERSE>(pixels, |rgb| self.sample_power_of_two::<4, 17>(rgb)),
            _ => apply_pixels::<REVERSE>(pixels, |rgb| self.sample(rgb)),
        }
    }

    #[inline]
    fn sample_power_of_two<const SHIFT: u32, const EDGE: usize>(&self, input: [u8; 3]) -> [u8; 3] {
        let [x, y, z] = input.map(|value| usize::from(value) >> SHIFT);
        let [dx, dy, dz] = input.map(|value| i32::from(value) & ((1 << SHIFT) - 1));
        let origin = (x * EDGE + y) * EDGE + z;
        let sx = EDGE * EDGE;
        let sy = EDGE;
        let (first_offset, second_offset, fractions) = if dx > dy {
            if dy > dz {
                (sx, sx + sy, [dx, dy, dz])
            } else if dx > dz {
                (sx, sx + 1, [dx, dz, dy])
            } else {
                (1, sx + 1, [dz, dx, dy])
            }
        } else if dx > dz {
            (sy, sx + sy, [dy, dx, dz])
        } else if dy > dz {
            (sy, sy + 1, [dy, dz, dx])
        } else {
            (1, sy + 1, [dz, dy, dx])
        };
        let a = self.table[origin];
        let b = self.table[origin + first_offset];
        let c = self.table[origin + second_offset];
        let d = self.table[origin + sx + sy + 1];
        std::array::from_fn(|channel| {
            let delta = (i32::from(b[channel]) - i32::from(a[channel])) * fractions[0]
                + (i32::from(c[channel]) - i32::from(b[channel])) * fractions[1]
                + (i32::from(d[channel]) - i32::from(c[channel])) * fractions[2];
            // Constant signed division is optimized without changing truncation
            // toward zero. A plain arithmetic shift would round negatives down.
            (i32::from(a[channel]) + delta / (1_i32 << SHIFT)).clamp(0, 255) as u8
        })
    }

    /// Stable test identity for the retained verified lookup allocation.
    #[cfg(test)]
    pub(super) fn allocation_identity(&self) -> usize {
        self.table.as_ptr() as usize
    }
    pub(crate) fn blended(&self, strength: f32) -> Self {
        let mut table = Vec::with_capacity(self.table.len());
        for x in 0..self.edge {
            for y in 0..self.edge {
                for z in 0..self.edge {
                    let identity = [x, y, z].map(|v| (v * self.step).min(255) as u8);
                    let restored = self.sample(identity);
                    table.push(std::array::from_fn(|c| {
                        f32::from(restored[c])
                            .mul_add(strength, (1.0 - strength) * f32::from(identity[c]))
                            .round_ties_even()
                            .clamp(0.0, 255.0) as u8
                    }));
                }
            }
        }
        Self {
            step: self.step,
            edge: self.edge,
            table,
        }
    }

    #[cfg(feature = "underwater-ai")]
    pub(crate) fn from_grid(step: usize, edge: usize, table: Vec<[u8; 3]>) -> Result<Self> {
        if !(1..=256).contains(&step) || edge != 254 / step + 2 || table.len() != edge * edge * edge
        {
            return Err(Error::InvalidMedia(
                "invalid underwater LUT grid dimensions".into(),
            ));
        }
        Ok(Self { step, edge, table })
    }

    #[cfg(feature = "underwater-ai")]
    pub(crate) fn update_grid(&mut self, mut value: impl FnMut(usize) -> [u8; 3]) {
        for (index, entry) in self.table.iter_mut().enumerate() {
            *entry = value(index);
        }
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let invalid = || Error::InvalidMedia("invalid underwater ILUT resource".into());
        let header = bytes.get(..12).ok_or_else(invalid)?;
        let integer = |offset| {
            u32::from_le_bytes(
                header[offset..offset + 4]
                    .try_into()
                    .expect("header length"),
            ) as usize
        };
        let step = integer(8);
        // CV_8UC3, 256 source levels, and a grid that covers every byte value.
        if integer(0) != 16 || integer(4) != 256 || !(1..=256).contains(&step) {
            return Err(invalid());
        }
        let edge = 254 / step + 2;
        let table_bytes = edge * edge * edge * 3;
        let end = 12 + table_bytes;
        // The original writer appends a fixed-width description and format tag.
        // Accept the table-only form as the vendor reader does; reject partial
        // trailers instead of silently accepting corrupt resource files.
        if bytes.len() != end && bytes.len() != end + 128 && bytes.len() != end + 132 {
            return Err(invalid());
        }
        let table = bytes[12..end]
            .chunks_exact(3)
            .map(|entry| [entry[0], entry[1], entry[2]])
            .collect();
        Ok(Self { step, edge, table })
    }

    pub(crate) fn sample(&self, input: [u8; 3]) -> [u8; 3] {
        let base = input.map(|v| usize::from(v) / self.step);
        let fraction = input.map(|v| i32::from(v) % self.step as i32);
        let [dx, dy, dz] = fraction;
        // Strict comparisons preserve the native shader's tie branches. On a
        // tetrahedron boundary the common edge still yields the same value.
        let order = if dx > dy {
            if dy > dz {
                [0, 1, 2]
            } else if dx > dz {
                [0, 2, 1]
            } else {
                [2, 0, 1]
            }
        } else if dx > dz {
            [1, 0, 2]
        } else if dy > dz {
            [1, 2, 0]
        } else {
            [2, 1, 0]
        };
        let value = |p: [usize; 3]| {
            let p = p.map(|v| v.min(self.edge - 1));
            self.table[(p[0] * self.edge + p[1]) * self.edge + p[2]]
        };
        let first = value(base);
        let mut previous = first;
        let mut corner = base;
        let mut delta = [0_i32; 3];
        for axis in order {
            corner[axis] += 1;
            let next = value(corner);
            for channel in 0..3 {
                delta[channel] +=
                    (i32::from(next[channel]) - i32::from(previous[channel])) * fraction[axis];
            }
            previous = next;
        }
        std::array::from_fn(|channel| {
            // Division truncates toward zero, including negative deltas. Moving
            // `first * step` into the numerator changes that contract.
            (i32::from(first[channel]) + delta[channel] / self.step as i32).clamp(0, 255) as u8
        })
    }
}

fn apply_pixels<const REVERSE: bool>(
    pixels: &mut [u8],
    sample: impl Fn([u8; 3]) -> [u8; 3] + Sync,
) {
    let apply_block = |block: &mut [u8]| {
        for pixel in block.chunks_exact_mut(3) {
            let input = if REVERSE {
                [pixel[2], pixel[1], pixel[0]]
            } else {
                [pixel[0], pixel[1], pixel[2]]
            };
            let result = sample(input);
            if REVERSE {
                pixel.copy_from_slice(&[result[2], result[1], result[0]]);
            } else {
                pixel.copy_from_slice(&result);
            }
        }
    };
    if pixels.len() < BLOCK_BYTES * 2 {
        apply_block(pixels);
    } else {
        pixels.par_chunks_mut(BLOCK_BYTES).for_each(apply_block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(step: u32, entry: impl Fn(usize, usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut bytes = [16_u32, 256, step]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let edge = (256 + step as usize - 2) / step as usize + 1;
        for x in 0..edge {
            for y in 0..edge {
                for z in 0..edge {
                    bytes.extend(entry(x, y, z));
                }
            }
        }
        bytes
    }

    fn nonlinear_table(step: u32) -> IntegerLut {
        IntegerLut::parse(&fixture(step, |x, y, z| {
            [
                ((x * 73 + y * 151 + z * 29) ^ (x * y * 11)) as u8,
                ((x * 113 + y * 17 + z * 197) ^ (y * z * 7)) as u8,
                ((x * 23 + y * 199 + z * 61) ^ (x * z * 13)) as u8,
            ]
        }))
        .unwrap()
    }

    #[test]
    fn optimized_grids_match_scalar_all_fractions_edges_and_channel_orders() {
        for step in [4, 16] {
            let lut = nonlinear_table(step);
            let last_base = 256 / step - 1;
            for base in [[0, 0, 0], [1, last_base, 2], [last_base; 3]] {
                let mut inputs = Vec::new();
                for dx in 0..step {
                    for dy in 0..step {
                        for dz in 0..step {
                            inputs.extend(
                                [
                                    base[0] * step + dx,
                                    base[1] * step + dy,
                                    base[2] * step + dz,
                                ]
                                .map(|v| v as u8),
                            );
                        }
                    }
                }
                for reverse in [false, true] {
                    let mut actual = inputs.clone();
                    if reverse {
                        lut.apply_bgr_table_to_rgb8(&mut actual);
                    } else {
                        lut.apply_rgb8(&mut actual);
                    }
                    for (input, output) in inputs.chunks_exact(3).zip(actual.chunks_exact(3)) {
                        let rgb = [input[0], input[1], input[2]];
                        let expected = if reverse {
                            let [b, g, r] = lut.sample([rgb[2], rgb[1], rgb[0]]);
                            [r, g, b]
                        } else {
                            lut.sample(rgb)
                        };
                        assert_eq!(output, expected, "step{step} input{rgb:?} reverse{reverse}");
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_vertex_changes_do_not_amplify_in_integer_interpolation() {
        // Each tetrahedron has nonnegative integer weights totaling the step.
        // The sampler is monotone in every vertex and commutes with a uniform
        // integer offset, even with signed truncation. Thus a per-vertex bound
        // also bounds every sampled channel; exercise ties and negative slopes.
        for step in [4, 16] {
            let original = nonlinear_table(step);
            let mut changed = original.clone();
            for (index, entry) in changed.table.iter_mut().enumerate() {
                for (channel, value) in entry.iter_mut().enumerate() {
                    *value = if (index + channel) % 2 == 0 {
                        value.saturating_add(1)
                    } else {
                        value.saturating_sub(1)
                    };
                }
            }
            let last_base = 256 / step - 1;
            let mut different = 0;
            for base in [[0, 0, 0], [1, last_base, 2], [last_base; 3]] {
                let mut first = Vec::new();
                for dx in 0..step {
                    for dy in 0..step {
                        for dz in 0..step {
                            first.extend(
                                [
                                    base[0] * step + dx,
                                    base[1] * step + dy,
                                    base[2] * step + dz,
                                ]
                                .map(|value| value as u8),
                            );
                        }
                    }
                }
                let mut second = first.clone();
                original.apply_rgb8(&mut first);
                changed.apply_rgb8(&mut second);
                for (index, (&first, &second)) in first.iter().zip(&second).enumerate() {
                    let delta = first.abs_diff(second);
                    assert!(
                        delta <= 1,
                        "step {step}, base {base:?}, channel {index}: {delta}"
                    );
                    different += usize::from(delta != 0);
                }
            }
            assert!(
                different > 0,
                "step {step}: fixture must alter sampled colors"
            );
        }
    }

    #[test]
    fn bulk_application_matches_scalar_across_block_tails_and_generic_steps() {
        for step in [4, 16, 10, 128] {
            let lut = nonlinear_table(step);
            for count in [0, 1, 4095, 4096, 4097, 8192, 12301] {
                let input: Vec<u8> = (0..count)
                    .flat_map(|i| [i as u8, (i * 71 + 43) as u8, (i * 13 + 97) as u8])
                    .collect();
                let mut actual = input.clone();
                lut.apply_rgb8(&mut actual);
                for (rgb, output) in input.chunks_exact(3).zip(actual.chunks_exact(3)) {
                    assert_eq!(
                        output,
                        lut.sample([rgb[0], rgb[1], rgb[2]]),
                        "step{step}, count{count}, input{rgb:?}"
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "exhaustive 24-bit color-domain qualification; run explicitly in release mode"]
    fn exhaustive_optimized_grids_match_scalar_byte_domain() {
        for step in [4, 16] {
            let lut = nonlinear_table(step);
            for r in 0..=255_u8 {
                let input: Vec<u8> = (0..=255_u8)
                    .flat_map(|g| (0..=255_u8).flat_map(move |b| [r, g, b]))
                    .collect();
                let mut actual = input.clone();
                lut.apply_rgb8(&mut actual);
                for (rgb, output) in input.chunks_exact(3).zip(actual.chunks_exact(3)) {
                    assert_eq!(
                        output,
                        lut.sample([rgb[0], rgb[1], rgb[2]]),
                        "step{step}, input{rgb:?}"
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "performance diagnostic; run serially in release mode with --nocapture"]
    fn benchmark_integer_lut_application() {
        use std::{hint::black_box, time::Instant};
        for step in [4, 16] {
            let lut = nonlinear_table(step);
            for (width, height) in [(1280, 640), (2560, 1280)] {
                let input: Vec<u8> = (0..width * height)
                    .flat_map(|i| [i as u8, (i * 71 + 43) as u8, (i * 13 + 97) as u8])
                    .collect();
                let mut pixels = input.clone();
                for optimized in [false, true] {
                    let mut times = Vec::new();
                    for round in 0..13 {
                        pixels.copy_from_slice(&input);
                        let start = Instant::now();
                        if optimized {
                            lut.apply_rgb8(&mut pixels);
                        } else {
                            // The previous AquaVision loop, including its Rayon granularity.
                            pixels.par_chunks_exact_mut(3).for_each(|rgb| {
                                rgb.copy_from_slice(&lut.sample([rgb[0], rgb[1], rgb[2]]));
                            });
                        }
                        black_box(&pixels);
                        if round >= 3 {
                            times.push(start.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                    times.sort_by(f64::total_cmp);
                    println!(
                        "{}",
                        serde_json::json!({"benchmark":"integer_lut", "step":step,"width":width,"height":height,"optimized":optimized,"median_ms":times[times.len()/2],"samples_ms":times})
                    );
                }
            }
        }
    }

    #[test]
    fn independent_affine_table_preserves_channel_order_and_all_six_regions() {
        let lut = IntegerLut::parse(&fixture(4, |x, y, z| {
            [(x * 3) as u8, (y * 2) as u8, z as u8]
        }))
        .unwrap();
        for input in [
            [11, 10, 9],
            [11, 9, 10],
            [10, 9, 11],
            [10, 11, 9],
            [9, 11, 10],
            [9, 10, 11],
            [12, 12, 12],
            [255, 255, 255],
        ] {
            assert_eq!(
                lut.sample(input),
                [
                    (u16::from(input[0]) * 3 / 4) as u8,
                    input[1] / 2,
                    input[2] / 4
                ]
            );
        }
    }

    #[test]
    fn negative_interpolation_truncates_delta_before_adding_origin() {
        let lut = IntegerLut::parse(&fixture(4, |x, _, _| [255 - x as u8; 3])).unwrap();
        assert_eq!(lut.sample([1, 0, 0]), [255; 3]);
        assert_eq!(lut.sample([5, 0, 0]), [254; 3]);
    }

    #[test]
    fn strength_blend_uses_resampled_terminal_vertices_and_ties_even() {
        let lut = IntegerLut::parse(&fixture(4, |_, _, _| [1, 3, 5])).unwrap();
        assert_eq!(lut.blended(0.5).sample([0; 3]), [0, 2, 2]);
        assert_eq!(lut.blended(1.0).sample([255; 3]), [1, 3, 5]);
        let identity = IntegerLut::parse(&fixture(4, |x, y, z| {
            [x, y, z].map(|v| (v * 4).min(255) as u8)
        }))
        .unwrap();
        // Native merging samples the existing LUT at clamped terminal byte255;
        // integer interpolation reaches254, unlike copying the raw final255.
        assert_eq!(identity.blended(1.0).table.last(), Some(&[254; 3]));
        assert_eq!(identity.blended(0.0).table.last(), Some(&[255; 3]));
    }

    #[test]
    fn malformed_tables_are_rejected_before_allocation() {
        for bytes in [
            vec![],
            vec![0; 12],
            [16_u32, 256, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
            [16_u32, 256, u32::MAX]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ] {
            assert!(IntegerLut::parse(&bytes).is_err());
        }
        let valid = fixture(128, |x, y, z| [x as u8, y as u8, z as u8]);
        for length in 0..valid.len() {
            assert!(IntegerLut::parse(&valid[..length]).is_err());
        }
        let mut with_trailer = valid.clone();
        with_trailer.extend([0; 132]);
        assert!(IntegerLut::parse(&with_trailer).is_ok());
        with_trailer.push(0);
        assert!(IntegerLut::parse(&with_trailer).is_err());
    }
}
