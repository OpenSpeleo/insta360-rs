//! Native polynomial normalization, shared by portable CPU and GPU renderers.
//!
//! SDK 1.10.4 INSCoreMedia arm64: GetDegreeCoeffs 0x19f2164,
//! GetFov 0x19f4bd4, PolyPinholeModelToInstrinsicParams 0x19f4da0.
//! Degree and FOV jump tables are at 0x51851a0 and 0x5185384.
//! Binary SHA-256: 3b905b46e46053d9c426c4af8bb28e449666d3ddf1c03419ab564ed2b0a01409.
//! These constants are distinct from modern physical housing-conversion curves.

use super::{LensProjectionModel, ParsedLens};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Where a polynomial lens obtains its degree-domain coefficients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolynomialCoefficientSource {
    /// The four coefficients were encoded in this recording's V2 offset.
    Recorded,
    /// A dedicated V1 lens-family row in the inspected native coefficient table.
    NativeLensTable,
    /// The inspected native V1 dispatcher uses its generic equidistant default.
    /// This does not establish per-unit or physical-housing calibration.
    NativeDefault,
}

/// Derived polynomial render parameters that leave native offset fields intact.
///
/// Multiply the lens's stored `fx` and `fy` by `focal_scale`, then evaluate
/// `theta * (c[0] + c[1]*theta + c[2]*theta² + c[3]*theta³)` in radians.
/// The dimensionless scale remains valid when decoded coordinates rescale focal
/// lengths. Coefficients and native radius stay available on [`ParsedLens`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedPolynomialProjection {
    pub coefficients: [f64; 4],
    pub focal_scale: f64,
    /// Native full FOV used to normalize the encoded radius, in degrees.
    /// This is separate from clipping/blend settings selected by the renderer.
    pub reference_full_fov_degrees: f64,
    pub coefficient_source: PolynomialCoefficientSource,
    /// True when native GetFov selects its generic 190-degree branch.
    pub reference_fov_from_default: bool,
}

impl NormalizedPolynomialProjection {
    pub(super) fn matches(&self, other: &Self) -> bool {
        let close =
            |a: f64, b: f64| (a - b).abs() <= 1.0e-12 * a.abs().max(b.abs()).max(f64::MIN_POSITIVE);
        self.coefficient_source == other.coefficient_source
            && self.reference_fov_from_default == other.reference_fov_from_default
            && close(
                self.reference_full_fov_degrees,
                other.reference_full_fov_degrees,
            )
            && close(self.focal_scale, other.focal_scale)
            && self
                .coefficients
                .iter()
                .zip(other.coefficients)
                .all(|(a, b)| close(*a, b))
    }
    /// Checks finite, positive normalization and the normalized linear term.
    pub fn validate(&self) -> Result<()> {
        if self.coefficients.iter().any(|value| !value.is_finite())
            || self.coefficients[0] != 1.0
            || !self.focal_scale.is_finite()
            || self.focal_scale <= 0.0
            || !self.reference_full_fov_degrees.is_finite()
            || !(0.0..=360.0).contains(&self.reference_full_fov_degrees)
            || self.reference_full_fov_degrees == 0.0
        {
            return Err(invalid("invalid normalized polynomial projection"));
        }
        Ok(())
    }
}

fn invalid(message: &str) -> Error {
    Error::MissingCalibration(message.into())
}

pub(super) fn normalized(lens: &ParsedLens) -> Result<Option<NormalizedPolynomialProjection>> {
    if lens.lens_type == 0 {
        return Ok(None);
    }
    let (degree, source) = match lens.model {
        LensProjectionModel::PinholePolynomialV1 => {
            let (degree, dedicated) = degree_coefficients(lens.lens_type);
            (
                degree,
                if dedicated {
                    PolynomialCoefficientSource::NativeLensTable
                } else {
                    PolynomialCoefficientSource::NativeDefault
                },
            )
        }
        LensProjectionModel::PinholePolynomialV2 => {
            let &[b1, b2, b3, b4] = lens.distortion_coefficients.as_slice() else {
                return Err(invalid(
                    "V2 normalization requires four recorded degree coefficients",
                ));
            };
            ([0.0, b1, b2, b3, b4], PolynomialCoefficientSource::Recorded)
        }
        _ => return Ok(None),
    };
    let (fov, default_fov) = native_fov(lens.lens_type);
    let [_, b1, b2, b3, b4] = degree;
    let half_fov = fov * 0.5;
    // Native conversion deliberately ignores b0, even for nonzero table values.
    let denominator = half_fov * (b1 + half_fov * (b2 + half_fov * (b3 + half_fov * b4)));
    if !b1.is_finite() || b1 <= 0.0 || !denominator.is_finite() || denominator <= 0.0 {
        return Err(invalid(
            "polynomial radius normalization is non-finite or non-positive",
        ));
    }
    // Exact double constants loaded at 0x5068468 and 0x5185190/198.
    let projection = NormalizedPolynomialProjection {
        coefficients: [
            1.0,
            b2 * 57.29577951308232 / b1,
            b3 * 3282.806350011744 / b1,
            b4 * 188090.94881441945 / b1,
        ],
        focal_scale: b1 * 180.0 / (denominator * std::f64::consts::PI),
        reference_full_fov_degrees: fov,
        coefficient_source: source,
        reference_fov_from_default: default_fov,
    };
    projection.validate()?;
    Ok(Some(projection))
}

fn degree_coefficients(lens_id: u32) -> ([f64; 5], bool) {
    // The native dispatcher masks the optical ID to its low eight bits.
    let coefficients = match lens_id & 0xff {
        1 | 2 => [-0.0004, 0.0192, -7.897e-06, -1.045e-07, -2.494e-09],
        3 => [0.0, 0.033707, 0.00010995, -3.4202e-06, 9.3345e-09],
        4 => [0.0007, 0.0183, 8.474e-06, -5.955e-07, 1.014e-09],
        6 => [-0.0008, 0.018, -1.252e-05, -6.696e-08, -1.437e-09],
        8 => [-0.0025, 0.0169, -3.679e-05, 6.7575e-07, -4.1093e-09],
        9 => [0.0026, 0.0171, 1.219e-05, -2.7019e-07, -9.5596e-10],
        10 => [0.0, 0.0166, -1.1297e-05, 1.3325e-07, -2.015e-09],
        11 => [0.0, 0.0169, -1.8189e-05, 2.4828e-07, -2.6403e-09],
        12 | 25 => [-0.001, 0.0331, -1.6306e-05, 1.3047e-07, -3.2436e-09],
        13 | 17 | 26 => [-0.0028074, 0.024317, -5.045e-05, 1.1095e-06, -8.8568e-09],
        14 => [-0.00087373, 0.015051, -1.3445e-05, 2.2256e-07, -2.0802e-09],
        15 | 28 => [0.0, 0.018777, 1.3379e-05, -1.7029e-07, -3.38e-09],
        16 => [0.0, 0.018393, -2.1462e-05, 4.0165e-07, -3.7618e-09],
        18 => [0.0, 0.031036, -8.0394e-06, -3.3297e-07, -2.9722e-09],
        19 | 33 => [0.0, 0.024134, -7.4557e-05, 2.0846e-06, -1.4576e-08],
        21 => [0.0, 1.0, -0.000198467, 4.4154e-06, -1.77966e-08],
        22 => [0.0, 1.0, 0.000268828, 1.82414e-05, -1.54088e-07],
        23 => [0.0, 0.020237, 1.2379e-05, -6.4541e-07, -4.954e-10],
        24 => [0.0, 0.0256, -7.0571e-05, 2.2326e-06, -1.7215e-08],
        27 => [0.0, 0.023693, -5.9584e-05, 1.8402e-06, -1.4809e-08],
        29 => [0.0, 0.024029, -6.86e-05, 2.0052e-06, -1.4266e-08],
        30 => [0.0, 0.019269, 6.4339e-06, 9.0452e-07, -9.2109e-09],
        31 => [0.0, 0.10392, 1.8477e-05, 3.6264e-08, -2.2362e-08],
        32 => [0.0, 1.0, -0.001663, 4.0685e-05, -3.4541e-07],
        34 | 68 => [0.0, 0.05247669, -2.57248e-05, 8.14169254e-07, -7.908613e-09],
        35 => [
            0.0,
            0.09279641,
            -1.4925244e-05,
            1.7604798e-06,
            -2.254106e-09,
        ],
        37 => [0.0, 0.025880234, -5.00010007, 5.29093e-06, -4.71066e-08],
        38 => [0.0, 0.024336, -7.55532e-05, 2.13171e-06, -1.5040305e-08],
        39 => [0.0, 0.0236785, -5.894477e-05, 1.810191e-06, -1.4521993e-08],
        40 => [0.0, 0.0259398, -7.3377467e-05, 2.2925e-06, -1.7776969e-08],
        41 => [
            0.0,
            0.02413266,
            -7.451592e-05,
            2.0839511e-06,
            -1.4573211e-08,
        ],
        42 => [
            0.0,
            0.02441343667,
            -7.7904498967e-05,
            2.184488572e-06,
            -1.5325733e-08,
        ],
        43 => [0.0, 0.02586, -8.1e-05, 2.41e-06, -1.819e-08],
        44 => [0.0, 0.02434, -7.555e-05, 2.132e-06, -1.504e-08],
        45 => [
            0.0,
            0.02956668,
            -1.437188e-05,
            5.78260464e-07,
            -7.237882e-09,
        ],
        50 | 81 => [
            0.0,
            0.03638676938359,
            -5.4394828366e-05,
            1.194065668e-06,
            -1.8897389e-08,
        ],
        51 => [
            0.0,
            0.024446313655691,
            -7.9118706055e-05,
            2.24180908e-06,
            -1.5675326e-08,
        ],
        52 => [0.0, 0.02351, -4.995e-05, 1.609e-06, -1.324e-08],
        58 | 74 => [-0.004578, 0.05344, -0.0001393, 5.218e-06, -4.475e-08],
        59 => [-0.003288, 0.02443, -7.682e-05, 2.184e-06, -1.536e-08],
        61 => [0.0, 1.0, -0.00136733, 3.04998e-05, -2.61682e-07],
        62 => [0.0, 0.04216, -4.022e-05, 1.604e-06, -1.5e-08],
        63 => [0.001616, 0.08131, 0.0001412, 5.571e-06, 6.811e-08],
        64 => [0.0, 0.04272, -4.509e-05, 1.774e-06, -1.642e-08],
        70 => [0.0, 0.02235, -5.382e-05, 2.001e-06, -1.392e-08],
        71 => [0.0, 0.02249, -5.79e-05, 1.932e-06, -1.295e-08],
        75 | 80 => [0.0, 0.05271, -0.0001055, 4.62e-06, -4.118e-08],
        82 => [0.0, 0.05343, -0.0001819, 4.973e-06, -3.637e-08],
        83 => [0.0, 0.024, -6.316e-05, 2.013e-06, -1.456e-08],
        85 => [0.0, 0.02513, -1.602e-05, 3.923e-07, -4.763e-09],
        89 => [0.0, 0.07682, 0.0001, -7.677e-07, -1.688e-10],
        102 => [0.0, 0.04901, 0.0001308, -1.795e-06, 8.665e-09],
        103 => [0.0, 0.07046, -8.085e-05, 9.264e-06, -9.001e-08],
        108 => [0.0, 0.0265, -8.148e-06, 1.107e-07, -3.412e-09],
        111 => [0.0, 0.04953, 0.000107, -1.215e-06, 6.113e-09],
        112 => [0.0, 0.03139, -4.778e-05, 1.476e-06, -1.045e-08],
        113 => [0.0, 0.03159, -8.415e-05, 2.201e-06, -1.284e-08],
        121 => [0.0, 0.04758, -2.925e-05, 2.431e-06, -1.862e-08],
        122 => [0.0, 0.0572, 0.0005829, -1.6358e-05, 3.1562e-07],
        126 => [0.0, 0.06073, 0.0001221, -2.422e-07, 1.845e-08],
        131 => [0.0, 0.02237608, -1.517e-05, 1e-06, -1e-08],
        132 => [0.0, 0.0692350926, -0.0001582334, 4.2251e-06, -1.55e-08],
        140 => [
            0.0,
            0.02347874251877121,
            -2.7674053581301105e-05,
            1.3785975546613045e-06,
            -1.1205771662550173e-08,
        ],
        141 => [
            0.0,
            0.02481221252321953,
            -2.6123719548679677e-05,
            8.175717942031892e-07,
            -7.084252450785871e-09,
        ],
        142 => [
            0.0,
            0.024508114438217286,
            -2.3584121988814713e-05,
            7.382934532711228e-07,
            -6.482923649818883e-09,
        ],
        144 | 242 => [
            0.0,
            0.12438555392403998,
            0.0002479751929076328,
            -3.7340206414873895e-06,
            3.2034028362148135e-07,
        ],
        145 => [
            0.0,
            0.25824308167628196,
            -0.00015105065170424535,
            5.064253775010642e-05,
            -3.3091941273973424e-07,
        ],
        147 => [
            0.0,
            0.022950812328163737,
            6.61104751469743e-05,
            -6.106795566339533e-07,
            -5.671004884169758e-10,
        ],
        148 => [
            0.0,
            0.02260293905255172,
            6.611044993729214e-05,
            -6.106791008246924e-07,
            -5.671030078810894e-10,
        ],
        149 => [
            0.0,
            0.02104300095114176,
            0.00018984736346981944,
            -3.168501588134238e-06,
            1.2810079952167183e-08,
        ],
        150 => [
            0.0,
            0.022867685137810168,
            0.00017002572367608965,
            -3.42236016293156e-06,
            1.5446105090951508e-08,
        ],
        151 => [
            0.0,
            0.02809824274707476,
            -4.503231972709072e-05,
            2.6685688981495795e-06,
            -2.2486310010252325e-08,
        ],
        152 => [
            0.0,
            0.02929228322587749,
            -4.0803834151379306e-05,
            1.5704219256712218e-06,
            -1.4202037680070088e-08,
        ],
        154 => [0.0, 0.030634545, 1.4919e-05, 1.111e-06, -1.2e-08],
        155 => [0.0, 0.032408701, -8.8927e-05, 2.4e-06, -1.6e-08],
        156 => [0.0, 0.0319439907, -9.41567e-05, 2.5305e-06, -1.47e-08],
        176 => [0.0, 0.07343921, 0.00166989, -7.88e-05, 1.33e-06],
        177 => [0.0, 0.06491602, 0.00100608, -3.935e-05, 5.4e-07],
        178 => [0.0, 0.06512271, 0.00058398, -2.109e-05, 2.7e-07],
        179 => [0.0, 0.0660192, 0.00030978, -1.069e-05, 1.4e-07],
        190 => [0.0, 0.0687763, 0.00155428, -6.757e-05, 1.04e-06],
        191 => [0.0, 0.0506809, 0.00063007, -2.195e-05, 3.1e-07],
        193 => [
            0.0,
            37.253353,
            -0.0716624767,
            0.00310798307,
            -2.27277666e-05,
        ],
        194 => [0.0, 0.05975867, -3.482e-05, 5.6e-06, -3e-08],
        195 => [0.0, 0.0480692, -7.078e-05, 2.22e-06, -1e-08],
        _ => return ([0.0, 1.0, 0.0, 0.0, 0.0], false),
    };
    (coefficients, true)
}

fn native_fov(lens_id: u32) -> (f64, bool) {
    let fov = match lens_id & 0xff {
        1 | 8 => 220.0,
        2 | 9 | 13 | 14 | 16 | 17 | 26 | 32 => 210.0,
        3 => 170.0,
        4 => 236.0,
        6 => 211.0,
        10 => 200.3,
        11 => 214.0,
        12 | 19 | 25 | 31 | 33 | 38 | 40 | 41 | 44 | 45 | 62 | 64 | 70 | 71 | 85 | 112 | 113
        | 131 | 142 | 149 | 150 | 154 | 155 | 193 => 200.0,
        15 | 28 => 203.0,
        18 => 191.0,
        22 => 208.0,
        23 => 205.0,
        24 | 29 => 196.0,
        34 | 68 => 151.0,
        35 => 156.6,
        37 => 133.774,
        50 | 58 | 74 | 75 | 80 | 81 | 82 | 89 | 121 | 151 | 152 => 150.0,
        63 => 82.0,
        102 => 158.0,
        103 => 155.0,
        111 => 147.1,
        122 => 110.8,
        126 => 157.3,
        132 => 156.0,
        140 | 141 | 156 => 195.0,
        144 => 91.74627778172767,
        145 => 39.682267222317165,
        176 => 90.6,
        177 => 110.4,
        178 => 121.0,
        179 => 126.4,
        190 => 98.0,
        191 => 106.6,
        194 => 145.0,
        195 => 189.0,
        _ => return (190.0, true),
    };
    (fov, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_dispatch_matches_independently_read_native_table_bytes() {
        // The digest was produced from the native jump-table destinations and
        // their loaded f64 bytes, independently of this Rust table. Each row is
        // LE u32 ID, five LE f64 coefficients, LE f64 FOV, dedicated/default u8s.
        let mut encoded = Vec::new();
        for id in 1_u32..=242 {
            let (coefficients, dedicated) = degree_coefficients(id);
            let (fov, default_fov) = native_fov(id);
            encoded.extend(id.to_le_bytes());
            for value in coefficients.into_iter().chain([fov]) {
                encoded.extend(value.to_le_bytes());
            }
            encoded.extend([u8::from(dedicated), u8::from(default_fov)]);
            assert_eq!(degree_coefficients(id), degree_coefficients(id | 0x100));
            assert_eq!(native_fov(id), native_fov(id | 0x100));
            let lens = lens(LensProjectionModel::PinholePolynomialV1, id, &[]);
            // This native row has a negative radius denominator. Retain it
            // for inspection, but do not prepare unusable renderer geometry.
            assert_eq!(lens.polynomial_projection.is_none(), id == 37);
        }
        assert_eq!(
            crate::assets::sha256(&encoded).to_hex(),
            "58240e64f84da50bf89a8115606294f5dcf629c6e088f5b05b28db77ddfe7070"
        );
    }

    fn lens(model: LensProjectionModel, id: u32, coefficients: &[f64]) -> ParsedLens {
        super::super::make_lens(
            model,
            Some(1000.0),
            None,
            1000.0,
            1000.0,
            1200.0,
            1100.0,
            &[0.0, 0.0, 0.0],
            [0.0; 3],
            coefficients,
            4800,
            2400,
            id,
        )
        .unwrap()
    }

    #[test]
    fn v1_table_normalization_matches_independent_degree_radius_references() {
        // Independent 60-digit decimal evaluation of the degree polynomial,
        // excluding b0 exactly as the native normalization function does.
        for (id, focal, at45, at90) in [
            (13, 631.8906583663403, 479.3342801514986, 910.5209103966005),
            (17, 631.8906583663403, 479.3342801514986, 910.5209103966005),
            (19, 602.5615591432606, 464.19148259348185, 928.8227624704226),
            (24, 625.6266145627816, 487.07811797391895, 951.3665227934217),
            (27, 651.199414584661, 504.88105403378916, 968.8185893976843),
            (29, 607.1883226519091, 470.4056144337727, 940.5994797240195),
            (31, 710.7909034606051, 552.1683139178836, 962.3837998938501),
            (32, 624.2825838038767, 478.580522062091, 910.0892567927398),
        ] {
            let lens = lens(LensProjectionModel::PinholePolynomialV1, id, &[]);
            let p = lens.polynomial_projection.unwrap();
            assert_eq!(
                p.coefficient_source,
                PolynomialCoefficientSource::NativeLensTable
            );
            assert!((lens.fx * p.focal_scale - focal).abs() < 1.0e-9, "lens{id}");
            let radius = |angle: f64| {
                let t = angle.to_radians();
                lens.fx
                    * p.focal_scale
                    * t
                    * p.coefficients
                        .iter()
                        .enumerate()
                        .map(|(i, c)| c * t.powi(i as i32))
                        .sum::<f64>()
            };
            assert!((radius(45.0) - at45).abs() < 1.0e-9, "lens{id}");
            assert!((radius(90.0) - at90).abs() < 1.0e-9, "lens{id}");
            assert!((radius(p.reference_full_fov_degrees / 2.0) - 1000.0).abs() < 1.0e-9);
            assert_eq!(
                (lens.radius, lens.fx, lens.cx, lens.cy),
                (Some(1000.0), 1000.0, 1200.0, 1100.0)
            );
            assert!(lens.distortion_coefficients.is_empty());
        }
    }

    #[test]
    fn v2_uses_recorded_degree_coefficients_without_replacing_native_values() {
        let native = [0.02, 0.00003, -0.0000001, -0.000000001];
        let mut lens = lens(LensProjectionModel::PinholePolynomialV2, 19, &native);
        let p = lens.polynomial_projection.unwrap();
        assert_eq!(p.coefficient_source, PolynomialCoefficientSource::Recorded);
        assert_eq!(lens.distortion_coefficients, native);
        assert_eq!(lens.radius, Some(1000.0));
        assert!((1000.0 * p.focal_scale - 545.674090600784).abs() < 1.0e-9);
        for (actual, expected) in p.coefficients.into_iter().zip([
            1.0,
            0.08594366926962348,
            -0.01641403175005872,
            -0.009404547440720971,
        ]) {
            assert!((actual - expected).abs() < 1.0e-14);
        }
        lens.distortion_coefficients[1] *= 2.0;
        assert!(lens.validate().is_err());
        lens.refresh_polynomial_projection().unwrap();
        lens.validate().unwrap();
        assert_ne!(lens.polynomial_projection, Some(p));
        // Derived scale is dimensionless, so anisotropic decoded-coordinate
        // rescaling needs only the existing fx/fy coordinate scaling.
        let prepared = lens.polynomial_projection;
        lens.fx *= 0.5;
        lens.fy *= 0.25;
        lens.cx *= 0.5;
        lens.cy *= 0.25;
        lens.validate().unwrap();
        assert_eq!(lens.polynomial_projection, prepared);
    }

    #[test]
    fn native_generic_dispatch_is_explicit_and_invalid_normalization_is_not_renderable() {
        for id in [78, 86, 87, 117, 118, 119, 120, 198, 199] {
            let lens = lens(LensProjectionModel::PinholePolynomialV1, id, &[]);
            let p = lens.polynomial_projection.unwrap();
            assert_eq!(
                p.coefficient_source,
                PolynomialCoefficientSource::NativeDefault
            );
            assert!(p.reference_fov_from_default);
            assert_eq!(p.coefficients, [1.0, 0.0, 0.0, 0.0]);
            assert_eq!(p.reference_full_fov_degrees, 190.0);
        }
        for native in [
            [0.0; 4],
            [1.0, 0.0, 0.0, -1.0],
            [f64::NAN, 0.0, 0.0, 0.0],
            [f64::MAX; 4],
        ] {
            let mut lens = lens(LensProjectionModel::PinholePolynomialV2, 19, &native);
            assert!(lens.polynomial_projection.is_none());
            assert!(lens.refresh_polynomial_projection().is_err());
            assert!(lens.polynomial_projection.is_none());
        }
    }

    #[test]
    fn prepared_public_values_roundtrip_validate_and_refresh_without_losing_native_data() {
        let mut lens = lens(LensProjectionModel::PinholePolynomialV1, 17, &[]);
        for _ in 0..200 {
            lens.refresh_polynomial_projection().unwrap();
            let restored: ParsedLens =
                serde_json::from_str(&serde_json::to_string(&lens).unwrap()).unwrap();
            restored.validate().unwrap();
            assert_eq!(restored.radius, lens.radius);
            assert_eq!(
                restored.distortion_coefficients,
                lens.distortion_coefficients
            );
            assert!(restored
                .polynomial_projection
                .unwrap()
                .matches(&lens.polynomial_projection.unwrap()));
            lens = restored;
        }
        let p = lens.polynomial_projection.unwrap();
        for invalid in [
            NormalizedPolynomialProjection {
                focal_scale: 0.0,
                ..p
            },
            NormalizedPolynomialProjection {
                focal_scale: f64::INFINITY,
                ..p
            },
            NormalizedPolynomialProjection {
                coefficients: [f64::NAN; 4],
                ..p
            },
            NormalizedPolynomialProjection {
                reference_full_fov_degrees: 0.0,
                ..p
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        lens.polynomial_projection = Some(NormalizedPolynomialProjection {
            coefficient_source: PolynomialCoefficientSource::NativeDefault,
            ..p
        });
        assert!(lens.validate().is_err());
        lens.refresh_polynomial_projection().unwrap();
        lens.validate().unwrap();
    }
}
