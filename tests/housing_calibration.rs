use insta360_rs::calibration::OffsetSource;
use insta360_rs::container::{EmbeddedOffset, InsvMetadata};
use insta360_rs::{CalibrationResolver, Environment, Housing, OpticalSelection};

fn metadata(camera: &str, lens_id: u32, version: u8) -> InsvMetadata {
    let mut fields = vec!["2".to_owned()];
    for index in 0..2 {
        let mut record = vec![
            2.0,
            100.0,
            100.0,
            50.25 + f64::from(index) * 100.0,
            49.75,
            0.5,
            -0.25,
            f64::from(index) * 180.0,
            0.001,
            -0.002,
            0.003,
        ];
        record.extend([0.0; 5]);
        if version == 6 {
            record.extend((1..=8).map(|index| f64::from(index) / 10000.0));
        }
        record.extend([200.0, 100.0, f64::from(lens_id)]);
        fields.extend(record.into_iter().map(|value| value.to_string()));
    }
    fields.push(((u32::from(version) << 16) | 0x400).to_string());
    InsvMetadata {
        camera_name: Some(camera.into()),
        offsets: vec![EmbeddedOffset {
            version,
            original: false,
            value: fields.join("_"),
        }],
        ..InsvMetadata::default()
    }
}

#[test]
fn x4_v6_conversions_match_independent_qr_fit_and_preserve_per_unit_terms() {
    // Golden solutions use modified Gram-Schmidt QR on the rectangular sampled
    // native basis. Production uses normal equations with LDLT decomposition.
    let cases = [
        (
            Environment::Underwater,
            86,
            91.95117867113132,
            [
                1.7998011317921612,
                -17.605752189686086,
                68.191025906054,
                -87.57500012213676,
            ],
        ),
        (
            Environment::Air,
            87,
            89.05062420484975,
            [
                2.970514845362455,
                -33.35032003964926,
                131.95638972574358,
                -176.86868904917313,
            ],
        ),
    ];
    let resolver = CalibrationResolver::default();
    let input = metadata("Insta360 X4", 71, 6);
    let bare = resolver
        .resolve_metadata(&input, &OpticalSelection::default(), OffsetSource::Current)
        .unwrap();
    for (environment, target, focal, radial) in cases {
        let selected = OpticalSelection::new(Housing::InvisibleDiveCase, environment);
        let converted = resolver
            .resolve_metadata(&input, &selected, OffsetSource::Current)
            .unwrap();
        for (lens, original) in converted.lenses.iter().zip(&bare.lenses) {
            assert_eq!(lens.lens_type, target);
            assert!((lens.fx - focal).abs() < 1e-7, "{} versus {focal}", lens.fx);
            assert_eq!(lens.fy, lens.fx);
            assert_eq!(lens.xi, original.xi);
            for (actual, expected) in lens.distortion_coefficients.iter().zip(radial) {
                assert!((actual - expected).abs() < 1e-6);
            }
            assert_eq!(lens.distortion_coefficients[4], 0.0);
            assert_eq!(
                &lens.distortion_coefficients[5..],
                &original.distortion_coefficients[5..]
            );
            assert_eq!((lens.cx, lens.cy), (original.cx, original.cy));
            assert_eq!(lens.orientation, original.orientation);
            assert_eq!(lens.translation, original.translation);
        }
        let mut encoded = input.clone();
        encoded.offsets[0].value = converted.raw_offset.clone();
        let repeated = resolver
            .resolve_metadata(&encoded, &selected, OffsetSource::Current)
            .unwrap();
        assert_eq!(repeated.lenses, converted.lenses);
        assert_eq!(repeated.raw_offset, converted.raw_offset);
    }
}

#[test]
fn x6_water_uses_fixed_target_xi_and_source_scale_at_ninety_degrees() {
    let input = metadata("Insta360 X6", 193, 6);
    let selected = OpticalSelection::new(Housing::InvisibleDiveCase, Environment::Underwater);
    let converted = CalibrationResolver::default()
        .resolve_metadata(&input, &selected, OffsetSource::Current)
        .unwrap();
    // Independent expanded polynomial at90°, using the converter's seven-term
    // table rather than the separately exposed five-term generic physical curve.
    let angle = 90_f64;
    let physical = 0.0446046592 * angle - 2.21051865e-5 * angle.powi(2)
        + 2.45643803e-6 * angle.powi(3)
        - 2.58126467e-8 * angle.powi(4)
        + 1.61274114e-10 * angle.powi(5)
        - 1.04220336e-12 * angle.powi(6);
    let expected = 50.0 / physical * 9.2635;
    for lens in &converted.lenses {
        assert_eq!(lens.lens_type, 198);
        assert_eq!(lens.xi, Some(2.45543));
        assert!((lens.fx - expected).abs() < 1e-10);
        assert_eq!(
            &lens.distortion_coefficients[..5],
            &[2.799666, -19.355603, 32.47295, 92.466926, 0.0]
        );
        assert_eq!(
            &lens.distortion_coefficients[5..],
            &[0.0001, 0.0002, 0.0003, 0.0004, 0.0005, 0.0006, 0.0007, 0.0008]
        );
    }
}

#[test]
fn asymmetric_lenses_share_the_native_target_fit_but_keep_individual_source_scales() {
    // Independent modified Gram-Schmidt QR of the native sampled basis, with
    // first xi=2 and second xi=1.8. The source lenses have zero radial terms.
    // This detects fitting each target independently or replacing source xi
    // before calculating the second lens's physical-to-pixel scale.
    let cases = [
        (
            "Insta360 X4",
            71,
            Housing::InvisibleDiveCase,
            Environment::Underwater,
            86,
            [91.95117867113132, 101.13524190817695],
        ),
        (
            "Insta360 X4",
            71,
            Housing::InvisibleDiveCase,
            Environment::Air,
            87,
            [89.05062420484975, 97.9449807081068],
        ),
        (
            "Insta360 X5",
            113,
            Housing::DiveCasePro,
            Environment::Underwater,
            119,
            [93.42596229370689, 102.76338348812337],
        ),
        (
            "Insta360 X5",
            113,
            Housing::DiveCasePro,
            Environment::Air,
            120,
            [91.42202389737922, 100.5591622539765],
        ),
        (
            "Insta360 X6",
            193,
            Housing::InvisibleDiveCase,
            Environment::Air,
            199,
            [88.8159739015195, 97.6821924662562],
        ),
    ];
    let resolver = CalibrationResolver::default();
    for (camera, source, housing, environment, target, expected_focal) in cases {
        let mut input = metadata(camera, source, 6);
        let mut fields = input.offsets[0]
            .value
            .split('_')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        fields[28] = "1.8".into();
        input.offsets[0].value = fields.join("_");
        let selected = OpticalSelection::new(housing, environment);
        let bare = resolver
            .resolve_metadata(&input, &OpticalSelection::default(), OffsetSource::Current)
            .unwrap();
        let converted = resolver
            .resolve_metadata(&input, &selected, OffsetSource::Current)
            .unwrap();
        for (index, (lens, original)) in converted.lenses.iter().zip(&bare.lenses).enumerate() {
            assert_eq!(lens.lens_type, target);
            let expected_xi = if camera == "Insta360 X4" {
                original.xi
            } else {
                Some(2.0)
            };
            assert_eq!(lens.xi, expected_xi, "{camera} target{target} lens{index}");
            assert!(
                (lens.fx - expected_focal[index]).abs() < 1e-7,
                "{camera} target{target} lens{index}: {} versus {}",
                lens.fx,
                expected_focal[index]
            );
            assert_eq!(lens.fy, lens.fx);
            assert_eq!(
                &lens.distortion_coefficients[..5],
                &converted.lenses[0].distortion_coefficients[..5]
            );
            assert_eq!(
                &lens.distortion_coefficients[5..],
                &original.distortion_coefficients[5..]
            );
            assert_eq!((lens.cx, lens.cy), (original.cx, original.cy));
            assert_eq!(lens.orientation, original.orientation);
            assert_eq!(lens.translation, original.translation);
        }
    }
}

#[test]
fn encoded_housings_and_bounded_repeated_conversions_resolve_without_guessing() {
    let resolver = CalibrationResolver::default();
    for (camera, bare, targets) in [
        ("Insta360 X4", 71, [86, 87]),
        ("Insta360 X6", 193, [198, 199]),
    ] {
        for (target, environment) in targets
            .into_iter()
            .zip([Environment::Underwater, Environment::Air])
        {
            let selected = OpticalSelection::new(Housing::InvisibleDiveCase, environment);
            let encoded = metadata(camera, target, 6);
            let resolved = resolver
                .resolve_metadata(
                    &encoded,
                    &OpticalSelection::default(),
                    OffsetSource::Current,
                )
                .unwrap();
            assert_eq!(resolved.raw_offset, encoded.offsets[0].value);
            assert_eq!(resolved.lenses[0].lens_type, target);
            let input = metadata(camera, bare, 6);
            let reference = resolver
                .resolve_metadata(&input, &selected, OffsetSource::Current)
                .unwrap();
            for _ in 0..32 {
                assert_eq!(
                    resolver
                        .resolve_metadata(&input, &selected, OffsetSource::Current)
                        .unwrap(),
                    reference
                );
            }
            assert!(resolver
                .resolve_metadata(&metadata(camera, bare, 3), &selected, OffsetSource::Current)
                .is_err());
        }
    }
    // X4's already encoded V3 housing retains measured calibration; no V6
    // conversion is needed, even though converting a bare V3 is unsupported.
    for target in [86, 87] {
        let encoded = metadata("Insta360 X4", target, 3);
        assert_eq!(
            resolver
                .resolve_metadata(
                    &encoded,
                    &OpticalSelection::default(),
                    OffsetSource::Current
                )
                .unwrap()
                .raw_offset,
            encoded.offsets[0].value
        );
    }
}

#[test]
fn x4_air_preserves_supplier_and_shares_first_lens_target_fit() {
    use insta360_rs::profile::{camera_profile_for_name, MaskBoundaryInterpolation, ProfileSource};
    let cases = [
        (
            131,
            Environment::Underwater,
            147,
            [97.36927133353626, 107.07406513329103],
        ),
        (
            142,
            Environment::Underwater,
            148,
            [101.98543412912153, 112.13518823023144],
        ),
        (
            131,
            Environment::Air,
            149,
            [96.55636138479578, 106.18013246226612],
        ),
        (
            142,
            Environment::Air,
            150,
            [100.8747644250978, 110.91398289452994],
        ),
    ];
    let resolver = CalibrationResolver::default();
    for (source, environment, target, expected_focal) in cases {
        let mut input = metadata("Insta360 X4 Air", source, 6);
        let mut fields = input.offsets[0]
            .value
            .split('_')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        fields[28] = "1.8".into();
        input.offsets[0].value = fields.join("_");
        let selected = OpticalSelection::new(Housing::InvisibleDiveCase, environment);
        let converted = resolver
            .resolve_metadata(&input, &selected, OffsetSource::Current)
            .unwrap();
        for (index, lens) in converted.lenses.iter().enumerate() {
            assert_eq!(lens.lens_type, target);
            // Native normal conversion fits the target using lensA xi once,
            // while the source scale retains each lens's original xi/focal.
            assert_eq!(lens.xi, Some(2.0));
            assert!((lens.fx - expected_focal[index]).abs() < 1e-7);
            assert_eq!(lens.fx, lens.fy);
            assert_eq!(
                &lens.distortion_coefficients[..5],
                &converted.lenses[0].distortion_coefficients[..5]
            );
            assert_eq!(
                &lens.distortion_coefficients[5..],
                &[0.0001, 0.0002, 0.0003, 0.0004, 0.0005, 0.0006, 0.0007, 0.0008]
            );
        }
        let mut encoded = input.clone();
        encoded.offsets[0].value = converted.raw_offset.clone();
        for _ in 0..16 {
            assert_eq!(
                resolver
                    .resolve_metadata(
                        &encoded,
                        &OpticalSelection::default(),
                        OffsetSource::Current
                    )
                    .unwrap()
                    .lenses,
                converted.lenses
            );
        }
        let profile = camera_profile_for_name("X4 Air")
            .unwrap()
            .lens(target)
            .unwrap();
        assert_eq!(
            profile.fallback.full_fov_degrees,
            if target < 149 { 190.0 } else { 200.0 }
        );
        assert_eq!(profile.fallback.blend_angle_degrees, Some(190.0));
        let mask = profile.mask_recipe.unwrap();
        assert_eq!(mask.interpolation, MaskBoundaryInterpolation::Angle);
        assert_eq!(mask.lower_hemisphere_boundary.len(), 11);
        for evidence in [
            profile.lens_id_provenance,
            profile.fallback_provenance,
            mask.provenance,
        ] {
            assert_eq!(evidence.source, ProfileSource::AndroidSdk215);
            assert_eq!(evidence.software(), "Insta360 Android SDK");
            assert_eq!(evidence.version(), "2.1.5");
            assert_eq!(
                evidence.binary_sha256(),
                Some("6cea9beda80ffe53eea85f04503a07cd25ff7a573b466df6d8e54aec7575a7e0")
            );
            assert_eq!(
                evidence.source_path,
                "AndroidSDKDemo/app-debug-2.1.5_1787657291340.apk!/lib/arm64-v8a/libarvbmg.so"
            );
        }
        assert!(resolver
            .resolve_metadata(
                &metadata("Insta360 X4 Air", source, 3),
                &selected,
                OffsetSource::Current
            )
            .is_err());
    }
}
