//! Borrowed decoded samples and mask-aware reconstruction shared with analysis.

use super::{mask::FisheyeMask, LensFrame};
use crate::{Error, Result};

/// Decoded plane storage with byte strides; row padding is not image data.
#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvRange {
    Limited,
    Full,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaLocation {
    Left,
    Center,
}

/// Planar 8-bit 4:2:0 samples, borrowed without a full-frame RGB conversion.
#[derive(Clone, Copy, Debug)]
pub struct Yuv420Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub y: Plane<'a>,
    pub u: Plane<'a>,
    pub v: Plane<'a>,
    pub range: YuvRange,
    pub matrix: YuvMatrix,
    pub chroma_location: ChromaLocation,
}

/// NV12 has interleaved U/V samples in its second plane.
#[derive(Clone, Copy, Debug)]
pub struct Nv12Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub y: Plane<'a>,
    pub uv: Plane<'a>,
    pub range: YuvRange,
    pub matrix: YuvMatrix,
    pub chroma_location: ChromaLocation,
}

/// Decoded high-bit-depth 4:2:0 samples. Strides are bytes, including for P010.
/// P010 uses bit_depth=10, lsb_shift=6 and interleaved_chroma=true. Planar
/// YUV420P10 uses bit_depth=10, lsb_shift=0 and interleaved_chroma=false.
#[derive(Clone, Copy, Debug)]
pub struct Yuv42016Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub y: Plane<'a>,
    pub u: Plane<'a>,
    pub v: Plane<'a>,
    pub bit_depth: u8,
    pub lsb_shift: u8,
    pub big_endian: bool,
    pub interleaved_chroma: bool,
    pub range: YuvRange,
    pub matrix: YuvMatrix,
    pub chroma_location: ChromaLocation,
}

/// Source pixels retained by the caller for one synchronized lens frame.
#[derive(Clone, Copy, Debug)]
pub enum StitchSource<'a> {
    Rgb(&'a LensFrame),
    Yuv420(Yuv420Frame<'a>),
    Nv12(Nv12Frame<'a>),
    Yuv42016(Yuv42016Frame<'a>),
}

/// Statically dispatched source access after projection has checked coordinates.
/// CPU rendering borrows a LensFrame directly; analysis borrows a decoded source.
pub(super) trait ProjectionSource {
    fn dimensions(&self) -> (u32, u32);

    /// Coordinates must be finite and inside a previously validated image.
    fn sample_inside(&self, x: f64, y: f64, mask: Option<&FisheyeMask>) -> [f64; 3];
}

impl ProjectionSource for LensFrame {
    #[inline]
    fn dimensions(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    #[inline]
    fn sample_inside(&self, x: f64, y: f64, mask: Option<&FisheyeMask>) -> [f64; 3] {
        super::masked_bilinear_rgb(self, x, y, mask)
    }
}

impl ProjectionSource for StitchSource<'_> {
    #[inline]
    fn dimensions(&self) -> (u32, u32) {
        StitchSource::dimensions(self)
    }

    #[inline]
    fn sample_inside(&self, x: f64, y: f64, mask: Option<&FisheyeMask>) -> [f64; 3] {
        StitchSource::sample_inside(self, x, y, mask)
    }
}

impl StitchSource<'_> {
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Rgb(frame) => (frame.width(), frame.height()),
            Self::Yuv420(frame) => (frame.width, frame.height),
            Self::Nv12(frame) => (frame.width, frame.height),
            Self::Yuv42016(frame) => (frame.width, frame.height),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let (width, height) = self.dimensions();
        super::frame_buffer_len(width, height)?;
        let check = |plane: Plane<'_>, width: usize, height: usize| {
            let needed = plane
                .stride
                .checked_mul(height - 1)
                .and_then(|n| n.checked_add(width));
            if plane.stride < width || needed.is_none_or(|n| n > plane.data.len()) {
                Err(Error::InvalidMedia(
                    "decoded source plane is truncated or has an invalid stride".into(),
                ))
            } else {
                Ok(())
            }
        };
        let (width, height) = (width as usize, height as usize);
        match self {
            Self::Rgb(_) => Ok(()),
            Self::Yuv420(frame) => {
                check(frame.y, width, height)?;
                check(frame.u, width.div_ceil(2), height.div_ceil(2))?;
                check(frame.v, width.div_ceil(2), height.div_ceil(2))
            }
            Self::Nv12(frame) => {
                check(frame.y, width, height)?;
                check(frame.uv, width.div_ceil(2) * 2, height.div_ceil(2))
            }
            Self::Yuv42016(frame) => {
                if !(9..=16).contains(&frame.bit_depth) || frame.lsb_shift > 16 - frame.bit_depth {
                    return Err(Error::InvalidMedia(
                        "invalid high-bit-depth source sample layout".into(),
                    ));
                }
                check(frame.y, width * 2, height)?;
                check(
                    frame.u,
                    width.div_ceil(2) * if frame.interleaved_chroma { 4 } else { 2 },
                    height.div_ceil(2),
                )?;
                if frame.interleaved_chroma {
                    Ok(())
                } else {
                    check(frame.v, width.div_ceil(2) * 2, height.div_ceil(2))
                }
            }
        }
    }

    /// Color in RGB code values (0..255). Call validate before sampling.
    pub(super) fn sample(&self, x: f64, y: f64, mask: Option<&FisheyeMask>) -> [f64; 3] {
        let (width, height) = self.dimensions();
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x > f64::from(width - 1)
            || y > f64::from(height - 1)
        {
            return [0.0; 3];
        }
        self.sample_inside(x, y, mask)
    }

    fn sample_inside(&self, x: f64, y: f64, mask: Option<&FisheyeMask>) -> [f64; 3] {
        let (width, height) = self.dimensions();
        let (luma, u, v, step, range, matrix, location, bits, shift, bytes, big_endian) = match self
        {
            Self::Rgb(frame) => return super::masked_bilinear_rgb(frame, x, y, mask),
            Self::Yuv420(f) => (
                f.y,
                f.u,
                f.v,
                1,
                f.range,
                f.matrix,
                f.chroma_location,
                8,
                0,
                1,
                false,
            ),
            Self::Nv12(f) => (
                f.y,
                f.uv,
                f.uv,
                2,
                f.range,
                f.matrix,
                f.chroma_location,
                8,
                0,
                1,
                false,
            ),
            Self::Yuv42016(f) => (
                f.y,
                f.u,
                if f.interleaved_chroma { f.u } else { f.v },
                if f.interleaved_chroma { 2 } else { 1 },
                f.range,
                f.matrix,
                f.chroma_location,
                f.bit_depth,
                f.lsb_shift,
                2,
                f.big_endian,
            ),
        };
        let scale = f64::from(1_u32 << (bits - 8));
        let read = |plane: Plane<'_>, xx, yy, channel, components| {
            let offset = yy * plane.stride + (xx * components + channel) * bytes;
            if bytes == 1 {
                f64::from(plane.data[offset])
            } else {
                let pair = [plane.data[offset], plane.data[offset + 1]];
                let raw = if big_endian {
                    u16::from_be_bytes(pair)
                } else {
                    u16::from_le_bytes(pair)
                };
                f64::from((u32::from(raw) >> shift) & ((1_u32 << bits) - 1))
            }
        };
        let valid = |xx: usize, yy: usize| mask.is_none_or(|m| m.pixel_weight(xx, yy) > 0.0);
        let yy = interpolate(x, y, width as usize, height as usize, |xx, yy| {
            valid(xx, yy).then(|| [read(luma, xx, yy, 0, 1)])
        })
        .unwrap_or([16.0 * scale])[0];
        let chroma_x = (x - if location == ChromaLocation::Center {
            0.5
        } else {
            0.0
        }) * 0.5;
        let chroma_y = (y - 0.5) * 0.5;
        // Conservatively require every luma pixel associated with a chroma texel.
        // Encoded chroma can already mix scene and housing; it cannot be unmixed.
        let chroma_valid = |xx: usize, yy: usize| {
            (yy * 2..(yy * 2 + 2).min(height as usize))
                .all(|ly| (xx * 2..(xx * 2 + 2).min(width as usize)).all(|lx| valid(lx, ly)))
        };
        // Both chroma channels have identical coordinates and source support.
        // Accumulate together, retaining each channel's original tap order.
        let uv = interpolate(
            chroma_x,
            chroma_y,
            (width as usize).div_ceil(2),
            (height as usize).div_ceil(2),
            |xx, yy| {
                chroma_valid(xx, yy)
                    .then(|| [read(u, xx, yy, 0, step), read(v, xx, yy, step - 1, step)])
            },
        )
        .unwrap_or([128.0 * scale; 2]);
        yuv_to_rgb_depth(yy, uv[0], uv[1], range, matrix, bits)
    }
}

fn interpolate<const CHANNELS: usize>(
    x: f64,
    y: f64,
    width: usize,
    height: usize,
    sample: impl Fn(usize, usize) -> Option<[f64; CHANNELS]>,
) -> Option<[f64; CHANNELS]> {
    let x = x.clamp(0.0, (width - 1) as f64);
    let y = y.clamp(0.0, (height - 1) as f64);
    let low = [x.floor() as usize, y.floor() as usize];
    let high = [x.ceil() as usize, y.ceil() as usize];
    let fraction = [x - low[0] as f64, y - low[1] as f64];
    let mut sum = [0.0; CHANNELS];
    let mut support = 0.0;
    for (yy, wy) in [(low[1], 1.0 - fraction[1]), (high[1], fraction[1])] {
        for (xx, wx) in [(low[0], 1.0 - fraction[0]), (high[0], fraction[0])] {
            if wx * wy <= 0.0 {
                continue;
            }
            if let Some(values) = sample(xx, yy) {
                for (sum, value) in sum.iter_mut().zip(values) {
                    *sum += value * wx * wy;
                }
                support += wx * wy;
            }
        }
    }
    (support > 0.0).then(|| sum.map(|value| value / support))
}

#[cfg(test)]
fn yuv_to_rgb(y: f64, u: f64, v: f64, range: YuvRange, matrix: YuvMatrix) -> [f64; 3] {
    yuv_to_rgb_depth(y, u, v, range, matrix, 8)
}

fn yuv_to_rgb_depth(
    y: f64,
    u: f64,
    v: f64,
    range: YuvRange,
    matrix: YuvMatrix,
    bits: u8,
) -> [f64; 3] {
    let scale = f64::from(1_u32 << (bits - 8));
    let maximum = f64::from((1_u32 << bits) - 1);
    let (y, cb, cr) = match range {
        YuvRange::Full => (
            y / maximum,
            (u - 128.0 * scale) / maximum,
            (v - 128.0 * scale) / maximum,
        ),
        YuvRange::Limited => (
            (y - 16.0 * scale) / (219.0 * scale),
            (u - 128.0 * scale) / (224.0 * scale),
            (v - 128.0 * scale) / (224.0 * scale),
        ),
    };
    let [r, gu, gv, b] = match matrix {
        YuvMatrix::Bt601 => [1.402, 0.344136, 0.714136, 1.772],
        YuvMatrix::Bt709 => [1.5748, 0.187324, 0.468124, 1.8556],
        YuvMatrix::Bt2020 => [1.4746, 0.164553, 0.571353, 1.8814],
    };
    [y + r * cr, y - gu * cb - gv * cr, y + b * cb].map(|v| v.clamp(0.0, 1.0) * 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint_chroma_interpolation_matches_independent_scalar_accumulation() {
        fn scalar(
            width: usize,
            height: usize,
            x: f64,
            y: f64,
            sample: impl Fn(usize, usize) -> Option<f64>,
        ) -> Option<f64> {
            let x = x.clamp(0.0, (width - 1) as f64);
            let y = y.clamp(0.0, (height - 1) as f64);
            let dx = x - x.floor();
            let dy = y - y.floor();
            let mut sum = 0.0;
            let mut support = 0.0;
            for (yy, wy) in [(y.floor() as usize, 1.0 - dy), (y.ceil() as usize, dy)] {
                for (xx, wx) in [(x.floor() as usize, 1.0 - dx), (x.ceil() as usize, dx)] {
                    if let Some(value) = sample(xx, yy).filter(|_| wx * wy > 0.0) {
                        sum += value * wx * wy;
                        support += wx * wy;
                    }
                }
            }
            (support > 0.0).then(|| sum / support)
        }
        for (width, height) in [(1, 1), (2, 3), (5, 4)] {
            for mask_phase in 0..5 {
                let sample = |x: usize, y: usize| {
                    let i = y * width + x;
                    (!(i + mask_phase).is_multiple_of(5))
                        .then_some([(i + 1) as f64 * 7.3, (i * i + 1) as f64 / 3.7])
                };
                for row in -3..=height as isize * 4 + 3 {
                    for column in -3..=width as isize * 4 + 3 {
                        let x = column as f64 / 4.0;
                        let y = row as f64 / 4.0;
                        let joint = interpolate(x, y, width, height, sample);
                        let first =
                            scalar(width, height, x, y, |xx, yy| sample(xx, yy).map(|v| v[0]));
                        let second =
                            scalar(width, height, x, y, |xx, yy| sample(xx, yy).map(|v| v[1]));
                        assert_eq!(
                            joint.map(|v| v.map(f64::to_bits)),
                            first.zip(second).map(|(a, b)| [a.to_bits(), b.to_bits()]),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fully_masked_chroma_cannot_tint_a_valid_luma_sample() {
        let y = [128; 64];
        let v = [128; 16];
        let mut u = [128; 16];
        let mask = FisheyeMask {
            width: 8,
            height: 8,
            weights: (0..64).map(|i| if i % 8 < 4 { 1.0 } else { 0.0 }).collect(),
        };
        let render = |u: &[u8], location| {
            StitchSource::Yuv420(Yuv420Frame {
                width: 8,
                height: 8,
                y: Plane {
                    data: &y,
                    stride: 8,
                },
                u: Plane { data: u, stride: 4 },
                v: Plane {
                    data: &v,
                    stride: 4,
                },
                range: YuvRange::Limited,
                matrix: YuvMatrix::Bt709,
                chroma_location: location,
            })
            .sample(3.0, 3.0, Some(&mask))
        };
        let baseline = render(&u, ChromaLocation::Center);
        for row in 0..4 {
            u[row * 4 + 2] = 255;
            u[row * 4 + 3] = 255;
        }
        // The old shader interpolated 0.75*128 + 0.25*255 = 159.75 before masking.
        assert_eq!(
            yuv_to_rgb(128.0, 159.75, 128.0, YuvRange::Limited, YuvMatrix::Bt709)
                .map(|v| v.round() as u8),
            [130, 124, 197]
        );
        assert_eq!(baseline.map(|v| v.round() as u8), [130; 3]);
        assert_eq!(render(&u, ChromaLocation::Center), baseline);
        assert_eq!(render(&u, ChromaLocation::Left), baseline);
        let uv: Vec<u8> = u.iter().zip(v).flat_map(|(&u, v)| [u, v]).collect();
        let nv12 = StitchSource::Nv12(Nv12Frame {
            width: 8,
            height: 8,
            y: Plane {
                data: &y,
                stride: 8,
            },
            uv: Plane {
                data: &uv,
                stride: 8,
            },
            range: YuvRange::Limited,
            matrix: YuvMatrix::Bt709,
            chroma_location: ChromaLocation::Center,
        });
        assert_eq!(nv12.sample(3.0, 3.0, Some(&mask)), baseline);
    }

    #[test]
    fn high_bit_depth_planar_and_p010_keep_range_and_exclude_chroma() {
        let mask = FisheyeMask {
            width: 8,
            height: 8,
            weights: (0..64).map(|i| if i % 8 < 4 { 1.0 } else { 0.0 }).collect(),
        };
        for (bits, shift, interleaved) in [
            (10, 0, false),
            (10, 6, true),
            (12, 0, false),
            (16, 0, false),
        ] {
            for big_endian in [false, true] {
                let scale = 1_u16 << (bits - 8);
                let encode = |values: Vec<u16>| -> Vec<u8> {
                    values
                        .into_iter()
                        .flat_map(|v| {
                            let v = v << shift;
                            if big_endian {
                                v.to_be_bytes()
                            } else {
                                v.to_le_bytes()
                            }
                        })
                        .collect()
                };
                let y = encode(vec![128 * scale; 64]);
                let v = encode(vec![128 * scale; 16]);
                let mut chroma = Vec::new();
                for i in 0..16 {
                    chroma.push(if i % 4 >= 2 { 255 * scale } else { 128 * scale });
                    if interleaved {
                        chroma.push(128 * scale);
                    }
                }
                let u = encode(chroma);
                let source = StitchSource::Yuv42016(Yuv42016Frame {
                    width: 8,
                    height: 8,
                    y: Plane {
                        data: &y,
                        stride: 16,
                    },
                    u: Plane {
                        data: &u,
                        stride: if interleaved { 16 } else { 8 },
                    },
                    v: Plane {
                        data: &v,
                        stride: 8,
                    },
                    bit_depth: bits,
                    lsb_shift: shift,
                    big_endian,
                    interleaved_chroma: interleaved,
                    range: YuvRange::Limited,
                    matrix: YuvMatrix::Bt709,
                    chroma_location: ChromaLocation::Center,
                });
                source.validate().unwrap();
                assert_eq!(
                    source
                        .sample(3.0, 3.0, Some(&mask))
                        .map(|v| v.round() as u8),
                    [130; 3]
                );
                for range in [YuvRange::Full, YuvRange::Limited] {
                    let (black, white) = match range {
                        YuvRange::Full => (0.0, f64::from((1_u32 << bits) - 1)),
                        YuvRange::Limited => (16.0 * f64::from(scale), 235.0 * f64::from(scale)),
                    };
                    let neutral = 128.0 * f64::from(scale);
                    assert_eq!(
                        yuv_to_rgb_depth(black, neutral, neutral, range, YuvMatrix::Bt709, bits),
                        [0.0; 3]
                    );
                    assert_eq!(
                        yuv_to_rgb_depth(white, neutral, neutral, range, YuvMatrix::Bt709, bits),
                        [255.0; 3]
                    );
                }
            }
        }
    }
}
