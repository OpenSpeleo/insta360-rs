//! Integer 3D lookup tables used by the legacy underwater restoration stage.
//!
//! The byte layout and signed integer interpolation follow the installed Studio
//! 5.9.10 resource and the `RemoveWaterThenLut` shader. These tables use the third
//! channel as the fastest dimension; they are not interchangeable with CUBE LUTs.

use crate::{Error, Result};

#[derive(Clone, Debug)]
pub(crate) struct IntegerLut {
    step: usize,
    edge: usize,
    table: Vec<[u8; 3]>,
}

impl IntegerLut {
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
