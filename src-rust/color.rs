//! Portable application of the bundled Studio I-Log color transforms.

use rayon::prelude::*;

use crate::assets::{AssetKind, AssetPolicy, BundledAssetProvider};
use crate::{Error, Result};

// These limits bound both text parsing and the GPU-compatible sample allocation.
const MAX_CUBE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CUBE_SIZE: u32 = 129;

/// An RGB 3D lookup table, sampled with trilinear interpolation.
///
/// CUBE rows are stored with red changing fastest, then green, then blue. The
/// table describes a color transform; callers must choose a LUT that matches the
/// input color encoding. In particular, I-Log LUTs must not be applied to footage
/// that the camera has already converted to a standard color profile.
#[derive(Debug, Clone)]
pub struct CubeLut {
    size: u32,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    // A fourth, unused component permits direct upload as WGSL vec4<f32> storage.
    values: Vec<[f32; 4]>,
}

impl CubeLut {
    /// Parses an Iridas or Resolve CUBE containing a single 3D LUT.
    ///
    /// Supports quoted `TITLE`, comments, `DOMAIN_MIN`/`DOMAIN_MAX`, and Resolve's
    /// `LUT_3D_INPUT_RANGE`. Domains default to 0..1. Combining both domain syntaxes,
    /// duplicate headers, headers after sample data, 1D/shaper LUTs, nonfinite
    /// values, and incomplete or excess samples are rejected. Output values may
    /// extend beyond 0..1. Sizes must be 2..=129 and text at most 128 MiB.
    pub fn parse_cube(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CUBE_BYTES {
            return Err(invalid_cube("source exceeds the 128 MiB limit"));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|error| invalid_cube(format!("source is not UTF-8: {error}")))?;
        let mut size = None;
        let mut domain_min = None;
        let mut domain_max = None;
        let mut input_range = None;
        let mut title_seen = false;
        let mut values = Vec::new();
        let mut expected_samples = 0;

        for (index, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let line_number = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fail = |message: &str| invalid_cube(format!("line {line_number}: {message}"));
            let (keyword, remainder) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            if keyword == "TITLE" {
                if title_seen || !values.is_empty() {
                    return Err(fail("TITLE must appear at most once, before sample data"));
                }
                let title = remainder
                    .trim_start()
                    .strip_prefix('"')
                    .ok_or_else(|| fail("TITLE must contain a quoted string"))?;
                let (_, trailing) = title
                    .split_once('"')
                    .ok_or_else(|| fail("TITLE is missing its closing quote"))?;
                if !trailing.trim().is_empty() && !trailing.trim_start().starts_with('#') {
                    return Err(fail("unexpected text after TITLE"));
                }
                title_seen = true;
                continue;
            }

            // A title may contain '#'; all remaining grammar treats it as a comment.
            let line = line.split_once('#').map_or(line, |(data, _)| data).trim();
            let mut fields = line.split_ascii_whitespace();
            let Some(first) = fields.next() else {
                continue;
            };
            match first {
                "LUT_1D_SIZE" | "LUT_1D_INPUT_RANGE" => {
                    return Err(fail("1D and combined shaper/3D LUTs are unsupported"));
                }
                "LUT_3D_SIZE" | "DOMAIN_MIN" | "DOMAIN_MAX" | "LUT_3D_INPUT_RANGE"
                    if !values.is_empty() =>
                {
                    return Err(fail("headers must precede sample data"));
                }
                "LUT_3D_SIZE" => {
                    if size.is_some() {
                        return Err(fail("duplicate LUT_3D_SIZE"));
                    }
                    let parsed = fields
                        .next()
                        .and_then(|value| value.parse::<u32>().ok())
                        .filter(|value| (2..=MAX_CUBE_SIZE).contains(value))
                        .ok_or_else(|| fail("LUT_3D_SIZE must be an integer in 2..=129"))?;
                    if fields.next().is_some() {
                        return Err(fail("LUT_3D_SIZE accepts exactly one integer"));
                    }
                    expected_samples = (parsed as usize).pow(3);
                    values
                        .try_reserve_exact(expected_samples)
                        .map_err(|error| {
                            invalid_cube(format!("cannot allocate sample table: {error}"))
                        })?;
                    size = Some(parsed);
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    if input_range.is_some() {
                        return Err(fail("DOMAIN headers conflict with LUT_3D_INPUT_RANGE"));
                    }
                    let target = if first == "DOMAIN_MIN" {
                        &mut domain_min
                    } else {
                        &mut domain_max
                    };
                    if target.is_some() {
                        return Err(fail("duplicate DOMAIN header"));
                    }
                    *target = Some(parse_floats::<3>(fields, line_number)?);
                }
                "LUT_3D_INPUT_RANGE" => {
                    if input_range.is_some() || domain_min.is_some() || domain_max.is_some() {
                        return Err(fail("duplicate or conflicting LUT_3D_INPUT_RANGE"));
                    }
                    input_range = Some(parse_floats::<2>(fields, line_number)?);
                }
                _ => {
                    if size.is_none() {
                        return Err(fail("sample data requires a preceding LUT_3D_SIZE"));
                    }
                    if values.len() == expected_samples {
                        return Err(fail("too many sample rows"));
                    }
                    let rgb = parse_floats::<3>(std::iter::once(first).chain(fields), line_number)?;
                    values.push([rgb[0], rgb[1], rgb[2], 0.0]);
                }
            }
        }

        let size = size.ok_or_else(|| invalid_cube("missing LUT_3D_SIZE"))?;
        if values.len() != expected_samples {
            return Err(invalid_cube(format!(
                "expected {expected_samples} sample rows, found {}",
                values.len()
            )));
        }
        let (domain_min, domain_max) = if let Some([minimum, maximum]) = input_range {
            ([minimum; 3], [maximum; 3])
        } else {
            (
                domain_min.unwrap_or([0.0; 3]),
                domain_max.unwrap_or([1.0; 3]),
            )
        };
        for channel in 0..3 {
            let span = domain_max[channel] - domain_min[channel];
            if span <= 0.0 || !span.is_finite() || !span.recip().is_finite() {
                return Err(invalid_cube(format!(
                    "channel {channel} domain must have a finite, positive, representable span"
                )));
            }
        }
        Ok(Self {
            size,
            domain_min,
            domain_max,
            values,
        })
    }

    /// Loads a declared bundled color LUT after checking its length and SHA-256.
    ///
    /// Selection is explicit: this does not infer a camera or input color profile
    /// from an asset filename or claim qualification for other processing models.
    pub fn load_bundled(id: &str) -> Result<Self> {
        let bundle = BundledAssetProvider::manifest()?;
        let descriptor = bundle.asset(id).ok_or_else(|| {
            Error::MissingCapability(format!("bundled color LUT {id:?} is not declared"))
        })?;
        if descriptor.kind != AssetKind::ColorLut {
            return Err(Error::InvalidMedia(format!(
                "bundled asset {id:?} is not a color LUT"
            )));
        }
        let asset = bundle
            .load_verified(&BundledAssetProvider, id, AssetPolicy::Required)?
            .ok_or_else(|| {
                Error::MissingCapability(format!("bundled color LUT {id:?} is unavailable"))
            })?;
        Self::parse_cube(&asset.bytes)
    }

    /// Returns the number of grid points along each RGB input axis.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Returns the lower input bound for each RGB channel.
    pub fn domain_min(&self) -> [f32; 3] {
        self.domain_min
    }

    /// Returns the upper input bound for each RGB channel.
    pub fn domain_max(&self) -> [f32; 3] {
        self.domain_max
    }

    /// Samples the LUT, clamping input to its domain and retaining float output.
    ///
    /// NaN inputs select the corresponding lower domain bound. Infinities select
    /// the respective endpoint. Output values are not restricted to 0..1.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let edge = self.size as usize - 1;
        let position: [f32; 3] = std::array::from_fn(|channel| {
            let minimum = self.domain_min[channel];
            let maximum = self.domain_max[channel];
            let input = if rgb[channel].is_nan() {
                minimum
            } else {
                rgb[channel].clamp(minimum, maximum)
            };
            ((input - minimum) / (maximum - minimum)).clamp(0.0, 1.0) * edge as f32
        });
        let lower = position.map(|value| value as usize);
        let upper = lower.map(|value| (value + 1).min(edge));
        let fraction: [f32; 3] =
            std::array::from_fn(|channel| position[channel] - lower[channel] as f32);
        let stride = self.size as usize;
        let at = |red: usize, green: usize, blue: usize| {
            self.values[red + stride * (green + stride * blue)]
        };
        let c000 = at(lower[0], lower[1], lower[2]);
        let c100 = at(upper[0], lower[1], lower[2]);
        let c010 = at(lower[0], upper[1], lower[2]);
        let c110 = at(upper[0], upper[1], lower[2]);
        let c001 = at(lower[0], lower[1], upper[2]);
        let c101 = at(upper[0], lower[1], upper[2]);
        let c011 = at(lower[0], upper[1], upper[2]);
        let c111 = at(upper[0], upper[1], upper[2]);
        std::array::from_fn(|channel| {
            let rg0 = lerp(
                lerp(c000[channel], c100[channel], fraction[0]),
                lerp(c010[channel], c110[channel], fraction[0]),
                fraction[1],
            );
            let rg1 = lerp(
                lerp(c001[channel], c101[channel], fraction[0]),
                lerp(c011[channel], c111[channel], fraction[0]),
                fraction[1],
            );
            lerp(rg0, rg1, fraction[2])
        })
    }

    /// Applies the transform in parallel to tightly packed, interleaved RGB8 pixels.
    ///
    /// Inputs are normalized by 255 before sampling. Outputs are clamped to 0..1
    /// and rounded to the nearest byte. An incomplete RGB triplet is rejected
    /// before any pixels are modified.
    pub fn apply_rgb8(&self, pixels: &mut [u8]) -> Result<()> {
        if !pixels.len().is_multiple_of(3) {
            return Err(Error::InvalidMedia(
                "color LUT input must contain complete RGB8 triplets".to_owned(),
            ));
        }
        pixels.par_chunks_exact_mut(3).for_each(|pixel| {
            let output = self.sample([
                f32::from(pixel[0]) / 255.0,
                f32::from(pixel[1]) / 255.0,
                f32::from(pixel[2]) / 255.0,
            ]);
            for channel in 0..3 {
                pixel[channel] = (output[channel].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        Ok(())
    }

    #[cfg(feature = "gpu")]
    pub(crate) fn values(&self) -> &[[f32; 4]] {
        &self.values
    }
}

fn invalid_cube(message: impl std::fmt::Display) -> Error {
    Error::InvalidMedia(format!("invalid CUBE LUT: {message}"))
}

fn parse_floats<'a, const N: usize>(
    mut fields: impl Iterator<Item = &'a str>,
    line_number: usize,
) -> Result<[f32; N]> {
    let fail = || {
        invalid_cube(format!(
            "line {line_number}: expected exactly {N} finite numbers"
        ))
    };
    let mut values = [0.0; N];
    for value in &mut values {
        *value = fields
            .next()
            .and_then(|field| field.parse::<f32>().ok())
            .filter(|parsed| parsed.is_finite())
            .ok_or_else(fail)?;
    }
    if fields.next().is_some() {
        return Err(fail());
    }
    Ok(values)
}

#[inline]
fn lerp(left: f32, right: f32, fraction: f32) -> f32 {
    // A weighted sum avoids overflow of right-left for opposite signed outputs.
    left * (1.0 - fraction) + right * fraction
}

#[cfg(test)]
mod tests {
    use super::*;

    // For normalized r,g,b, this table represents [r + 2g + 4b, rg, gb - 0.5].
    // Cross terms exercise all axes and distinguish trilinear from tetrahedral
    // interpolation; the asymmetric first channel detects transposed row order.
    const ASYMMETRIC_ROWS: &str =
        "0 0 -0.5\n1 0 -0.5\n2 0 -0.5\n3 1 -0.5\n4 0 -0.5\n5 0 -0.5\n6 0 0.5\n7 1 0.5\n";

    fn asymmetric_lut(headers: &str) -> CubeLut {
        CubeLut::parse_cube(format!("LUT_3D_SIZE 2\n{headers}{ASYMMETRIC_ROWS}").as_bytes())
            .unwrap()
    }

    fn assert_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn interpolates_asymmetric_cube_and_preserves_out_of_gamut_outputs() {
        let lut = asymmetric_lut("");
        assert_eq!(lut.size(), 2);
        assert_eq!(lut.domain_min(), [0.0; 3]);
        assert_eq!(lut.domain_max(), [1.0; 3]);
        assert_eq!(lut.sample([1.0, 0.0, 0.0]), [1.0, 0.0, -0.5]);
        assert_eq!(lut.sample([0.0, 1.0, 0.0]), [2.0, 0.0, -0.5]);
        assert_eq!(lut.sample([0.0, 0.0, 1.0]), [4.0, 0.0, -0.5]);
        assert_eq!(lut.sample([1.0; 3]), [7.0, 1.0, 0.5]);
        assert_close(lut.sample([0.25, 0.5, 0.75]), [4.25, 0.125, -0.125]);
    }

    #[test]
    fn normalizes_domains_and_clamps_input_at_both_ends() {
        let lut = asymmetric_lut("DOMAIN_MIN -1 2 10\nDOMAIN_MAX 3 6 18\n");
        assert_close(lut.sample([0.0, 4.0, 16.0]), [4.25, 0.125, -0.125]);
        assert_eq!(lut.sample([-5.0, 9.0, 99.0]), [6.0, 0.0, 0.5]);
        assert_eq!(lut.sample([3.0, 6.0, 18.0]), [7.0, 1.0, 0.5]);
        assert_eq!(
            lut.sample([f32::NAN, f32::INFINITY, f32::NEG_INFINITY]),
            [2.0, 0.0, -0.5]
        );
        let resolve = asymmetric_lut("LUT_3D_INPUT_RANGE -1 3\n");
        assert_close(resolve.sample([0.0, 1.0, 2.0]), [4.25, 0.125, -0.125]);
    }

    #[test]
    fn accepts_comments_whitespace_titles_and_optional_domain_endpoints() {
        let source = format!(
            "\u{feff}# comment\r\n\tTITLE \"Reference #1\" # trailing\r\n\
             DOMAIN_MIN -1 -1 -1\r\nLUT_3D_SIZE 2 # grid\r\n\
             {ASYMMETRIC_ROWS} # trailing comment\n"
        );
        let lut = CubeLut::parse_cube(source.as_bytes()).unwrap();
        assert_close(lut.sample([-0.5, 0.0, 0.5]), [4.25, 0.125, -0.125]);
        let max_only = asymmetric_lut("DOMAIN_MAX 2 2 2\n");
        assert_close(max_only.sample([0.5, 1.0, 1.5]), [4.25, 0.125, -0.125]);
    }

    #[test]
    fn applies_to_rgb8_and_rejects_partial_pixels_without_mutating() {
        let lut = asymmetric_lut("");
        let mut pixels = vec![0, 0, 0, 255, 255, 255, 0, 255, 255, 128, 128, 128];
        lut.apply_rgb8(&mut pixels).unwrap();
        assert_eq!(pixels, [0, 0, 0, 255, 255, 128, 255, 0, 128, 255, 64, 0]);
        let mut malformed = vec![255, 255, 255, 1];
        assert!(lut.apply_rgb8(&mut malformed).is_err());
        assert_eq!(malformed, [255, 255, 255, 1]);
        lut.apply_rgb8(&mut []).unwrap();
    }

    #[test]
    fn rejects_bad_headers_sample_counts_and_unsupported_transforms() {
        let invalid = [
            "",
            "# only a comment",
            "0 0 0",
            "LUT_3D_SIZE",
            "LUT_3D_SIZE -2",
            "LUT_3D_SIZE 1",
            "LUT_3D_SIZE 130",
            "LUT_3D_SIZE 4294967295",
            "LUT_3D_SIZE 18446744073709551616",
            "LUT_3D_SIZE 2.0",
            "LUT_3D_SIZE 2 2",
            "LUT_3D_SIZE 2\nLUT_3D_SIZE 2",
            "LUT_3D_SIZE 2\n0 0 0",
            "LUT_1D_SIZE 2",
            "LUT_3D_SIZE 2\nLUT_1D_SIZE 2",
            "LUT_3D_SIZE 2\nLUT_1D_INPUT_RANGE 0 1",
            "TITLE no quotes",
            "TITLE \"unterminated",
            "TITLE \"title\" extra",
            "TITLE \"a\"\nTITLE \"b\"",
            "LUT_3D_SIZE 2\nUNKNOWN 1 2",
            "LUT_3D_SIZE 2\n0 0",
            "LUT_3D_SIZE 2\n0 0 0 0",
            "LUT_3D_SIZE 2\n0 0 NaN",
            "LUT_3D_SIZE 2\n0 inf 0",
            "LUT_3D_SIZE 2\n1e50 0 0",
        ];
        for source in invalid {
            assert!(CubeLut::parse_cube(source.as_bytes()).is_err(), "{source}");
        }
        for trailing in [
            "0 0 0\n",
            "LUT_3D_SIZE 2\n",
            "DOMAIN_MIN 0 0 0\n",
            "DOMAIN_MAX 1 1 1\n",
            "LUT_3D_INPUT_RANGE 0 1\n",
            "TITLE \"late\"\n",
        ] {
            let source = format!("LUT_3D_SIZE 2\n{ASYMMETRIC_ROWS}{trailing}");
            assert!(
                CubeLut::parse_cube(source.as_bytes()).is_err(),
                "{trailing}"
            );
        }
        assert!(CubeLut::parse_cube(&[0xff]).is_err());
    }

    #[test]
    fn rejects_nonfinite_empty_reversed_and_conflicting_domains() {
        for header in [
            "DOMAIN_MIN NaN 0 0\n",
            "DOMAIN_MAX 1 inf 1\n",
            "DOMAIN_MIN 1 0 0\n",
            "DOMAIN_MAX -1 1 1\n",
            "DOMAIN_MIN 0 0\n",
            "DOMAIN_MAX 1 1 1 1\n",
            "DOMAIN_MIN 0 0 0\nDOMAIN_MIN 0 0 0\n",
            "DOMAIN_MAX 1 1 1\nDOMAIN_MAX 1 1 1\n",
            "LUT_3D_INPUT_RANGE 1 0\n",
            "LUT_3D_INPUT_RANGE 0 0\n",
            "LUT_3D_INPUT_RANGE 0\n",
            "LUT_3D_INPUT_RANGE 0 1 2\n",
            "LUT_3D_INPUT_RANGE 0 inf\n",
            "LUT_3D_INPUT_RANGE 0 1\nLUT_3D_INPUT_RANGE 0 1\n",
            "DOMAIN_MIN 0 0 0\nLUT_3D_INPUT_RANGE 0 1\n",
            "LUT_3D_INPUT_RANGE 0 1\nDOMAIN_MAX 1 1 1\n",
            "LUT_3D_INPUT_RANGE -3e38 3e38\n",
            "LUT_3D_INPUT_RANGE 0 1e-45\n",
        ] {
            let source = format!("LUT_3D_SIZE 2\n{header}{ASYMMETRIC_ROWS}");
            assert!(CubeLut::parse_cube(source.as_bytes()).is_err(), "{header}");
        }
    }

    #[test]
    fn loads_all_three_bundled_cube_luts_and_checks_kind() {
        for (id, size) in [
            ("studio-i-log-x5-rec709", 65),
            ("studio-i-log-ace-pro-2-rec709", 33),
            ("studio-i-log-luna-rec709", 33),
        ] {
            let lut = CubeLut::load_bundled(id).unwrap();
            assert_eq!(lut.size(), size);
            assert!(lut.sample([0.5; 3]).iter().all(|value| value.is_finite()));
        }
        assert!(matches!(
            CubeLut::load_bundled("missing-color-lut"),
            Err(Error::MissingCapability(_))
        ));
        assert!(matches!(
            CubeLut::load_bundled("camera-config-camera-conf-insta360-x5-json"),
            Err(Error::InvalidMedia(_))
        ));
        // This asset is a proprietary model despite its ColorLut manifest role.
        assert!(CubeLut::load_bundled("desktop-colorplus-lut").is_err());
    }

    #[test]
    fn bundled_x5_changes_pixels_and_matches_independent_grid_reference() {
        let lut = CubeLut::load_bundled("studio-i-log-x5-rec709").unwrap();
        assert_close(lut.sample([0.5; 3]), [0.351995, 0.358694, 0.352789]);
        // Original cube rows at (24/25,28/29,32/33), read independently:
        // .112520 .291157 .437201 | .148486 .286809 .436286
        // .100099 .313695 .425177 | .135760 .309743 .423499
        // .102724 .290913 .474754 | .139574 .286213 .473686
        // .0897688 .312810 .462776 | .127062 .308965 .461601
        // Fractions (1/2,1/4,3/4) give weights (3,3,1,1,9,9,3,3)/32.
        assert_close(
            lut.sample([24.5 / 64.0, 28.25 / 64.0, 32.75 / 64.0]),
            [0.120_314_11, 0.294_274_84, 0.461_819_62],
        );
        let mut pixel = [96, 112, 128];
        lut.apply_rgb8(&mut pixel).unwrap();
        // Independently weighted original grid values for this byte input are
        // (0.113312757, 0.293183615, 0.440498926), rounded after scaling by 255.
        assert_eq!(pixel, [29, 75, 112]);
    }
}
