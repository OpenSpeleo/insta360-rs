//! Housing conversion routes traced in iOS SDK 1.10.4 and Android SDK 2.1.5 ARM64.
//!
//! Generic physical curves describe lens behavior, while the converter prefers
//! the seven-coefficient tables below when `get6thOrderPolynomialCoeffs`
//! succeeds. C9 water additionally uses a measured native target model and a
//! 90-degree source scale. Keep these decisions at calibration preparation.

use super::*;

pub(super) fn apply(
    calibration: &mut ResolvedCalibration,
    camera: Option<&CameraProfile>,
    requested: &OpticalProfile,
) -> Result<bool> {
    let Some(camera) = camera else {
        return Ok(false);
    };
    let target = match (&camera.camera, requested) {
        (CameraModel::X4, OpticalProfile::InvisibleDiveCaseUnderwater) => 86,
        (CameraModel::X4, OpticalProfile::InvisibleDiveCaseAir) => 87,
        (CameraModel::X6, OpticalProfile::InvisibleDiveCaseUnderwater) => 198,
        (CameraModel::X6, OpticalProfile::InvisibleDiveCaseAir) => 199,
        (CameraModel::X4Air, OpticalProfile::InvisibleDiveCaseUnderwater)
        | (CameraModel::X4Air, OpticalProfile::InvisibleDiveCaseAir) => {
            // Android ResolveOffsetShell at 0x5416d50/0x5416e84 and
            // 0x5416eb8/0x5417014 select supplier from bare 131 or 142.
            // Matching encoded housings already returned before this helper.
            let water = matches!(requested, OpticalProfile::InvisibleDiveCaseUnderwater);
            match (calibration.lenses[0].lens_type, water) {
                (131, true) => 147,
                (142, true) => 148,
                (131, false) => 149,
                (142, false) => 150,
                _ => return Err(Error::MissingCalibration(
                    "X4 Air housing conversion requires bare supplier lens 131 or 142; this source-to-housing route is not established".into()
                )),
            }
        }
        _ => return Ok(false),
    };
    if calibration.offset_version != 6
        || calibration
            .lenses
            .iter()
            .any(|lens| lens.model != LensProjectionModel::OmniRadtanPro)
    {
        return Err(Error::MissingCalibration(format!(
            "{} housing conversion currently requires V6 calibration; an already encoded housing offset can be used without conversion",
            camera.canonical_name
        )));
    }
    let target_profile = camera.lens(target).expect("registered conversion target");
    let source = camera
        .lens(calibration.lenses[0].lens_type)
        .expect("caller validated source");
    let source_curve = conversion_curve(source.lens_id)?;
    let target_curve = conversion_curve(target)?;
    // All fitted routes compute one target model using the first lens xi:
    // Android normal 0x5b96614..62c, iOS X4 0x1e32140..154 and C9 air
    // 0x1e2e18c..a8. Source pixel scales remain per-lens. X4 alone preserves
    // each source xi after sharing the radial fit (no target-xi store).
    let target_model = if target == 198 {
        // iOS getV6distort at 0x1e2cfd4, doubles 0x529dc40 and
        // literal stores 0x1e2cff0..d014. Tail coefficients stay per-unit.
        (
            2.45543,
            9.2635,
            [2.799666, -19.355603, 32.47295, 92.466926, 0.0],
        )
    } else {
        let xi = calibration.lenses[0]
            .xi
            .ok_or_else(|| Error::MissingCalibration("V6 housing conversion requires xi".into()))?;
        let (focal, radial) =
            fit_v6_radial_profile(xi, target_curve, target_profile.fallback.full_fov_degrees)?;
        (xi, focal, radial)
    };
    for lens in &mut calibration.lenses {
        let xi = lens
            .xi
            .ok_or_else(|| Error::MissingCalibration("V6 housing conversion requires xi".into()))?;
        let source_focal = (lens.fx * lens.fy).sqrt();
        let scale = if target == 198 {
            // convertOffset at 0x1e2e218 selects getPhysical2Pixel90DegScale for
            // targets 197/198 only. Its physical polynomial is evaluated at 90°.
            physical_scale_at_90(
                xi,
                source_focal,
                &lens.distortion_coefficients[..5],
                source_curve,
            )?
        } else {
            physical_to_pixel_scale(
                xi,
                source_focal,
                &lens.distortion_coefficients[..5],
                source_curve,
                source.fallback.full_fov_degrees,
            )?
        };
        let (mut target_xi, focal, radial) = target_model;
        if camera.camera == CameraModel::X4 {
            target_xi = xi;
        }
        let pixel_focal = scale * focal;
        if !pixel_focal.is_finite() || pixel_focal <= 0.0 {
            return Err(Error::MissingCalibration(
                "housing conversion produced an invalid focal length".into(),
            ));
        }
        lens.xi = Some(target_xi);
        lens.fx = pixel_focal;
        lens.fy = pixel_focal;
        lens.distortion_coefficients[..5].copy_from_slice(&radial);
        lens.k1 = radial[0];
        lens.k2 = radial[1];
        lens.k3 = radial[2];
        lens.lens_type = target;
    }
    calibration.profile_name = requested.profile_name().map(str::to_owned);
    calibration.raw_offset = encode_v6_offset(calibration);
    calibration.validate()?;
    Ok(true)
}

fn physical_scale_at_90(xi: f64, focal: f64, radial: &[f64], curve: [f64; 7]) -> Result<f64> {
    // 0x1e3d71c adds the binary's cos(pi/2) literal to xi, followed by
    // u+radial[0]*u^3+...+radial[4]*u^11 at 0x1e3d738..780.
    let u = 1.0 / (xi + std::f64::consts::FRAC_PI_2.cos());
    let u2 = u * u;
    let mut power = u * u2;
    let mut projection = u;
    for coefficient in radial {
        projection = coefficient.mul_add(power, projection);
        power *= u2;
    }
    let scale = projection * focal / evaluate_profile(curve, 90.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(Error::MissingCalibration(
            "housing 90-degree physical scale is not positive and finite".into(),
        ));
    }
    Ok(scale)
}

fn conversion_curve(lens_id: u32) -> Result<[f64; 7]> {
    // ins::Lens::get6thOrderPolynomialCoeffs0x1e4e650; per-case data and
    // literal seventh doubles are recorded below. No runtime vendor dependency.
    Ok(match lens_id {
        // 0x1e4e828 ->0x529df30; final bits0xbd83893f54b3a0bb.
        106 => [
            0.0, 0.0222, -1.318e-5, 1.702e-6, -3.694e-8, 4.445e-10, -2.221e-12,
        ],
        // 0x1e4e870 ->0x529df00; final bits0xbd84bffeea28b748.
        107 => [
            0.0, 0.02208, -1.568e-5, 1.84e-6, -4.0e-8, 4.792e-10, -2.359e-12,
        ],
        // 0x1e4e708 ->0x529ded0; final bits0xbd821c74b061d42c.
        108 => [
            0.0, 0.02203, -9.484e-6, 1.516e-6, -3.26e-8, 4.019e-10, -2.059e-12,
        ],
        // 0x1e4e6a8 ->0x529dea0; final bits0xbd7255aaaa7991ed.
        193 => [
            0.0,
            0.0446046592,
            -2.21051865e-5,
            2.45643803e-6,
            -2.58126467e-8,
            1.61274114e-10,
            -1.04220336e-12,
        ],
        // 0x1e4e750 ->0x529de70; final bits0xbd7c3ad48befbdcf.
        197 => [
            0.0,
            0.0454209969,
            -2.66048702e-5,
            2.86818832e-6,
            -3.44498211e-8,
            2.66650288e-10,
            -1.60467867e-12,
        ],
        // 0x1e4e798 ->0x529de40; final bits0xbd9487105b00d7dd.
        198 => [
            0.0,
            0.0484677266,
            -0.000221657224,
            1.51604144e-5,
            -2.78962065e-7,
            1.94921255e-9,
            -4.66743434e-12,
        ],
        // 0x1e4e7e0 ->0x529de10; final bits0xbdb2093c2f8ce3b7.
        199 => [
            0.0,
            0.0459657399,
            -0.000404190186,
            3.03215887e-5,
            -6.40814172e-7,
            5.3997503e-9,
            -1.64037143e-11,
        ],
        _ => {
            let physical = crate::profile::physical_curve(lens_id).ok_or_else(|| {
                Error::MissingCalibration(format!(
                    "lens {lens_id} has no verified conversion curve"
                ))
            })?;
            let mut curve = [0.0; 7];
            curve[..5].copy_from_slice(&physical.coefficients);
            curve
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_90_scale_matches_independent_pinhole_equation() {
        let curve = [0.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0];
        assert!(
            (physical_scale_at_90(2.0, 900.0, &[0.0; 5], curve).unwrap() - 100.0).abs() < 1e-12
        );
        // At u=.5, .1*u^3+.2*u^5=.01875, giving466.875/4.5.
        assert!(
            (physical_scale_at_90(2.0, 900.0, &[0.1, 0.2, 0.0, 0.0, 0.0], curve).unwrap() - 103.75)
                .abs()
                < 1e-12
        );
        assert!(physical_scale_at_90(0.0, 900.0, &[0.0; 5], [0.0; 7]).is_err());
    }

    #[test]
    fn sixth_order_curves_are_distinct_from_generic_mask_curves_and_monotonic() {
        for lens in [106, 107, 108, 193, 197, 198, 199] {
            let curve = conversion_curve(lens).unwrap();
            assert_ne!(
                &curve[..5],
                crate::profile::physical_curve(lens).unwrap().coefficients
            );
            let mut previous = 0.0;
            for angle in 1..=950 {
                let value = evaluate_profile(curve, f64::from(angle) / 10.0);
                assert!(
                    value.is_finite() && value > previous,
                    "lens{lens} angle{angle}"
                );
                previous = value;
            }
        }
        assert!(conversion_curve(123456).is_err());
    }
}
