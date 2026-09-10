use insta360_rs::{Environment, Housing, LensAccessory};
use std::fs::File;
use std::path::Path;

use insta360_rs::calibration::{
    CalibrationCandidate, EmbeddedProfilePayload, LensProjectionModel, OffsetSource,
    ParsedEmbeddedProfile,
};
use insta360_rs::container::{
    EmbeddedOffset, EmbeddedProfile, GuardDetectedType, InsvMetadata, OffsetState,
};
use insta360_rs::{
    CalibrationPolicy, CalibrationResolver, CameraModel, Error, InsvReader, OpticalSelection,
};

const FLAGS: u32 = 0x400;

fn v1_offset(radius: f64, lens_type: u32) -> String {
    let lens = |cx: f64, rotation_z: f64| vec![radius, cx, 50.0, 0.0, 0.0, rotation_z];
    let mut fields = vec!["2".to_owned()];
    fields.extend(lens(50.0, 0.0).into_iter().map(number));
    fields.extend(lens(150.0, 180.0).into_iter().map(number));
    fields.extend([
        "200".to_owned(),
        "100".to_owned(),
        (FLAGS | lens_type).to_string(),
    ]);
    fields.join("_")
}

fn v2_offset(radius: f64, lens_type: u32) -> String {
    let lens = |cx: f64, rotation_z: f64| {
        vec![
            radius,
            cx,
            50.0,
            0.0,
            0.0,
            rotation_z,
            0.0,
            0.0,
            0.0,
            1.0,
            -0.1,
            0.01,
            -0.001,
            200.0,
            100.0,
            f64::from(lens_type),
        ]
    };
    encode_records(2, &[lens(50.0, 0.0), lens(150.0, 180.0)])
}

fn v3_offset(focal: f64, lens_type: u32) -> String {
    let lens = |cx: f64, rotation_z: f64| {
        vec![
            2.0,
            focal,
            focal + 0.5,
            cx,
            50.0,
            0.0,
            0.0,
            rotation_z,
            0.0,
            0.0,
            0.0,
            0.1,
            -0.01,
            0.001,
            -0.0001,
            0.00001,
            200.0,
            100.0,
            f64::from(lens_type),
        ]
    };
    encode_records(3, &[lens(50.0, 0.0), lens(150.0, 180.0)])
}

fn v6_offset(focal: f64, lens_type: u32) -> String {
    let lens = |cx: f64, rotation_z: f64| {
        let mut values = vec![
            2.0,
            focal,
            focal + 0.5,
            cx,
            50.0,
            0.0,
            0.0,
            rotation_z,
            0.0,
            0.0,
            0.0,
        ];
        values.extend((1..=13).map(|value| f64::from(value) / 100.0));
        values.extend([200.0, 100.0, f64::from(lens_type)]);
        values
    };
    encode_records(6, &[lens(50.0, 0.0), lens(150.0, 180.0)])
}

fn encode_records(version: u8, records: &[Vec<f64>]) -> String {
    let mut fields = vec![records.len().to_string()];
    for record in records {
        fields.extend(record.iter().copied().map(number));
    }
    fields.push(((u32::from(version) << 16) | FLAGS).to_string());
    fields.join("_")
}

fn number(value: f64) -> String {
    value.to_string()
}

fn offset(version: u8, original: bool, value: String) -> EmbeddedOffset {
    EmbeddedOffset {
        version,
        original,
        value,
    }
}

#[test]
fn parses_the_four_real_offset_layouts() {
    let resolver = CalibrationResolver::new(CalibrationPolicy::PreferNewest);
    let cases = [
        (
            1,
            v1_offset(40.0, 113),
            LensProjectionModel::PinholePolynomialV1,
            0,
            None,
        ),
        (
            2,
            v2_offset(45.0, 113),
            LensProjectionModel::PinholePolynomialV2,
            4,
            None,
        ),
        (
            3,
            v3_offset(50.0, 113),
            LensProjectionModel::OmniRadtan,
            5,
            Some(2.0),
        ),
        (
            6,
            v6_offset(55.0, 113),
            LensProjectionModel::OmniRadtanPro,
            13,
            Some(2.0),
        ),
    ];

    for (version, value, model, coefficient_count, xi) in cases {
        let candidate = CalibrationCandidate::new(version, Some("bare".into()), value);
        let result = resolver
            .resolve(
                &[candidate],
                &OpticalSelection::new(Housing::None, Environment::Air),
            )
            .expect("valid offset should parse");

        assert_eq!(result.offset_version, version);
        assert_eq!(result.offset_flags, FLAGS);
        assert_eq!(result.canvas_width, 200);
        assert_eq!(result.canvas_height, 100);
        assert_eq!(result.lenses[0].model, model);
        assert_eq!(
            result.lenses[0].distortion_coefficients.len(),
            coefficient_count
        );
        assert_eq!(result.lenses[0].xi, xi);
        assert_eq!(result.lenses[0].lens_type, 113);
    }
}

#[test]
fn v1_retains_native_fields_and_prepares_the_explicit_native_default() {
    let result = CalibrationResolver::default()
        .resolve(
            &[CalibrationCandidate::new(1, None, v1_offset(40.0, 78))],
            &OpticalSelection::default(),
        )
        .expect("V1 metadata remains inspectable");

    result.validate_for_stitching().unwrap();
    let lens = &result.lenses[0];
    assert_eq!(lens.radius, Some(40.0));
    assert_eq!(lens.fx, 40.0);
    assert!(lens.distortion_coefficients.is_empty());
    let prepared = lens.polynomial_projection.unwrap();
    assert_eq!(
        prepared.coefficient_source,
        insta360_rs::PolynomialCoefficientSource::NativeDefault
    );
    assert!(prepared.reference_fov_from_default);
    assert_eq!(prepared.coefficients, [1.0, 0.0, 0.0, 0.0]);
    assert!((prepared.focal_scale - 2.0 / 190.0_f64.to_radians()).abs() < 1.0e-12);
}

#[test]
fn polynomial_serde_compatibility_requires_refresh_and_preserves_encoded_fields() {
    let calibration = CalibrationResolver::default()
        .resolve(
            &[CalibrationCandidate::new(
                1,
                Some("bare".into()),
                v1_offset(40.0, 113),
            )],
            &OpticalSelection::new(Housing::None, Environment::Air),
        )
        .unwrap();
    let prepared = calibration.lenses[0].polynomial_projection.unwrap();
    assert_eq!(
        prepared.coefficient_source,
        insta360_rs::PolynomialCoefficientSource::NativeLensTable
    );
    assert_eq!(prepared.reference_full_fov_degrees, 200.0);
    assert!(!prepared.reference_fov_from_default);
    let mut encoded = serde_json::to_value(&calibration).unwrap();
    for lens in encoded["lenses"].as_array_mut().unwrap() {
        lens.as_object_mut()
            .unwrap()
            .remove("polynomial_projection");
    }
    let mut restored: insta360_rs::ResolvedCalibration = serde_json::from_value(encoded).unwrap();
    assert!(restored.validate_for_stitching().is_err());
    for lens in &mut restored.lenses {
        lens.validate().expect("old metadata is still inspectable");
        lens.refresh_polynomial_projection().unwrap();
    }
    restored.validate_for_stitching().unwrap();
    assert_eq!(restored.raw_offset, calibration.raw_offset);
    assert_eq!(restored.lenses[0].radius, Some(40.0));
    assert!(restored.lenses[0].distortion_coefficients.is_empty());
    assert_eq!(restored.lenses[0].polynomial_projection, Some(prepared));
}

#[test]
fn singular_v2_polynomial_stays_inspectable_but_cannot_render() {
    let calibration = CalibrationResolver::default()
        .resolve(
            &[CalibrationCandidate::new(
                2,
                Some("bare".into()),
                v2_offset(45.0, 113),
            )],
            &OpticalSelection::new(Housing::None, Environment::Air),
        )
        .unwrap();
    assert_eq!(
        calibration.lenses[0].distortion_coefficients,
        [1.0, -0.1, 0.01, -0.001]
    );
    assert!(calibration.lenses[0].polynomial_projection.is_none());
    assert!(matches!(
        calibration.validate_for_stitching(),
        Err(Error::MissingCalibration(_))
    ));
    let frames = [
        insta360_rs::LensFrame::new(100, 100, vec![100; 30_000]).unwrap(),
        insta360_rs::LensFrame::new(100, 100, vec![100; 30_000]).unwrap(),
    ];
    assert!(matches!(
        insta360_rs::CpuStitcher::new().stitch_with_orientation(
            &frames,
            &calibration,
            insta360_rs::EquirectangularProjection {
                width: 64,
                height: 32
            },
            insta360_rs::Orientation::IDENTITY
        ),
        Err(Error::MissingCalibration(_))
    ));
}

#[test]
fn deserialized_calibration_requires_its_projection_parameters() {
    for (version, value, parameter) in [
        (2, v2_offset(45.0, 113), "radius"),
        (3, v3_offset(50.0, 113), "xi"),
        (6, v6_offset(55.0, 113), "xi"),
    ] {
        let calibration = CalibrationResolver::default()
            .resolve(
                &[CalibrationCandidate::new(
                    version,
                    Some("bare".into()),
                    value,
                )],
                &OpticalSelection::new(Housing::None, Environment::Air),
            )
            .unwrap();
        let mut json = serde_json::to_value(calibration).unwrap();
        json["lenses"][0].as_object_mut().unwrap().remove(parameter);
        let restored: insta360_rs::ResolvedCalibration = serde_json::from_value(json).unwrap();
        assert!(matches!(
            restored.validate_for_stitching(),
            Err(Error::MissingCalibration(_))
        ));
    }
}

#[test]
fn chooses_the_newest_usable_real_schema() {
    let candidates = vec![
        CalibrationCandidate::new(1, Some("bare".into()), v1_offset(40.0, 113)),
        CalibrationCandidate::new(3, Some("bare".into()), v3_offset(50.0, 113)),
        CalibrationCandidate::new(6, Some("bare".into()), "2_broken"),
    ];

    let result = CalibrationResolver::default()
        .resolve(
            &candidates,
            &OpticalSelection::new(Housing::None, Environment::Air),
        )
        .expect("V3 should be selected after rejecting invalid V6");

    assert_eq!(result.offset_version, 3);
    assert_eq!(result.lenses[0].fx, 50.0);
}

#[test]
fn rejects_cross_generation_counts_and_mismatched_footer_version() {
    let wrong_count = CalibrationCandidate::new(3, Some("bare".into()), v6_offset(50.0, 113));
    let wrong_footer = CalibrationCandidate::new(
        6,
        Some("bare".into()),
        v6_offset(50.0, 113).replace("394240", "197632"),
    );

    for candidate in [wrong_count, wrong_footer] {
        let error = CalibrationResolver::default()
            .resolve(
                &[candidate],
                &OpticalSelection::new(Housing::None, Environment::Air),
            )
            .expect_err("schema mismatch must be rejected");
        assert!(matches!(error, Error::MissingCalibration(_)));
    }
}

#[test]
fn metadata_resolution_never_substitutes_original_and_current() {
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![
            offset(6, false, v6_offset(55.0, 113)),
            offset(6, true, v6_offset(45.0, 113)),
        ],
        ..InsvMetadata::default()
    };
    let resolver = CalibrationResolver::default();

    let current = resolver
        .resolve_metadata(
            &metadata,
            &OpticalSelection::new(Housing::None, Environment::Air),
            OffsetSource::Current,
        )
        .expect("current offset");
    let original = resolver
        .resolve_metadata(
            &metadata,
            &OpticalSelection::new(Housing::None, Environment::Air),
            OffsetSource::Original,
        )
        .expect("original offset");

    assert_eq!(current.lenses[0].fx, 55.0);
    assert_eq!(current.offset_source, OffsetSource::Current);
    assert_eq!(original.lenses[0].fx, 45.0);
    assert_eq!(original.offset_source, OffsetSource::Original);
}

#[test]
fn metadata_resolution_accepts_an_already_converted_x5_offset() {
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 117))],
        ..InsvMetadata::default()
    };

    let result = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::new(Housing::InvisibleDiveCase, Environment::Underwater),
            OffsetSource::Current,
        )
        .expect("lens type 117 is already X5 diving-water geometry");

    assert_eq!(result.profile_name.as_deref(), Some("InvisibleDiveWater"));
    assert_eq!(result.lenses[0].lens_type, 117);
}

#[test]
fn strict_auto_uses_the_encoded_x5_lens_type_when_accessory_state_is_absent() {
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        profiles: vec![coefficient_profile("InvisibleDiveWater", [0.1; 6])],
        ..InsvMetadata::default()
    };

    let result = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .expect("base X5 lens type is unambiguous");

    assert_eq!(result.profile_name.as_deref(), Some("bare"));
    assert_eq!(result.lenses[0].lens_type, 113);
}

#[test]
fn strict_auto_converts_the_recorded_x5_dive_case_pro_state() {
    let bare = [0.0, 0.03159, -0.00008415, 0.000002201, -1.284e-8, 0.0];
    let underwater = [0.000238, 0.03112, 0.000007306, 0.0000004427, -3.892e-9, 0.0];
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        profiles: vec![
            coefficient_profile("bare", bare),
            coefficient_profile("InvisibleDiveWater", underwater),
        ],
        offset_state: Some(OffsetState::DiveCaseProUnderwater),
        blend_angle: Some(190),
        ..InsvMetadata::default()
    };

    let result = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .expect("recorded Dive Case Pro underwater state is authoritative");

    assert_eq!(
        result.profile_name.as_deref(),
        Some("InvisibleDiveWaterPro")
    );
    assert!(result.lenses.iter().all(|lens| lens.lens_type == 119));
    assert!(result
        .lens_geometry
        .into_iter()
        .flatten()
        .all(|geometry| geometry.blend_angle_recorded));
}

#[test]
fn explicit_setup_overrides_recorded_state_and_inconclusive_auto_is_rejected() {
    let explicit = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        offset_state: Some(OffsetState::DiveCaseProUnderwater),
        ..InsvMetadata::default()
    };
    let result = CalibrationResolver::default()
        .resolve_metadata(
            &explicit,
            &OpticalSelection::new(Housing::None, Environment::Air),
            OffsetSource::Current,
        )
        .expect("an explicit caller selection has highest authority");
    assert_eq!(result.profile_name.as_deref(), Some("bare"));

    let inconclusive = InsvMetadata {
        offset_state: Some(OffsetState::Automatic),
        guard_detected_type: Some(GuardDetectedType::Unknown),
        ..explicit
    };
    assert!(CalibrationResolver::default()
        .resolve_metadata(
            &inconclusive,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .is_err());
}

#[test]
fn metadata_resolution_uses_the_registered_x3_lens_geometry() {
    let metadata = InsvMetadata {
        camera_name: Some("insta360-x3".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 70))],
        blend_angle: Some(188),
        ..InsvMetadata::default()
    };

    let result = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .expect("X3 aliases and lens ID 70 are registered");

    assert_eq!(result.camera_model, Some(CameraModel::X3));
    assert_eq!(result.profile_name.as_deref(), Some("bare"));
    for geometry in result.lens_geometry {
        let geometry = geometry.expect("registered render geometry");
        assert_eq!(geometry.full_fov_degrees, 200.0);
        assert_eq!(geometry.blend_angle_degrees, 188.0);
        assert!(geometry.blend_angle_recorded);
    }
}

#[test]
fn metadata_resolution_rejects_camera_lens_and_setup_mismatches() {
    let wrong_lens = InsvMetadata {
        camera_name: Some("Insta360 X3".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        ..InsvMetadata::default()
    };
    assert!(CalibrationResolver::default()
        .resolve_metadata(
            &wrong_lens,
            &OpticalSelection::default(),
            OffsetSource::Current
        )
        .is_err());

    let wrong_setup = InsvMetadata {
        camera_name: Some("Insta360 X3".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 70))],
        ..InsvMetadata::default()
    };
    assert!(CalibrationResolver::default()
        .resolve_metadata(
            &wrong_setup,
            &OpticalSelection::new(Housing::InvisibleDiveCase, Environment::Underwater),
            OffsetSource::Current,
        )
        .is_err());
}

#[test]
fn converts_x5_v6_geometry_from_embedded_angle_radius_profiles() {
    let bare = [0.0, 0.03159, -0.00008415, 0.000002201, -1.284e-8, 0.0];
    let coefficients = [0.000238, 0.03112, 0.000007306, 0.0000004427, -3.892e-9, 0.0];
    let profile = coefficient_profile("InvisibleDiveWater", coefficients);
    let parsed = ParsedEmbeddedProfile::parse(&profile).expect("known profile protobuf");
    assert_eq!(
        parsed.payload,
        EmbeddedProfilePayload::SixCoefficientTransform(coefficients)
    );

    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        profiles: vec![coefficient_profile("bare", bare), profile],
        ..InsvMetadata::default()
    };
    let result = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::new(Housing::InvisibleDiveCase, Environment::Underwater),
            OffsetSource::Current,
        )
        .expect("the embedded profiles provide portable conversion geometry");

    assert_eq!(result.profile_name.as_deref(), Some("InvisibleDiveWater"));
    assert_eq!(result.lenses[0].lens_type, 117);
    assert_eq!(result.lenses[1].lens_type, 117);
    assert_eq!(result.lenses[0].fx, result.lenses[0].fy);
    assert_ne!(result.lenses[0].fx, 55.0);
    assert!((result.lenses[0].fx - 52.408_612_607_260_07).abs() < 1e-9);
    let expected_radial = [
        0.451_706_379_351_284_27,
        -1.131_649_612_536_728_8,
        1.977_780_319_251_478_5,
        4.401_427_788_903_362,
        0.0,
    ];
    for (actual, expected) in result.lenses[0].distortion_coefficients[..5]
        .iter()
        .zip(expected_radial)
    {
        assert!((actual - expected).abs() < 1e-9);
    }
    assert_eq!(result.lenses[0].distortion_coefficients[4], 0.0);
    assert_eq!(result.lenses[0].distortion_coefficients[5], 0.06);
    assert_eq!(result.lenses[0].distortion_coefficients[12], 0.13);
    assert!(result.raw_offset.contains("_117_"));
}

#[test]
fn profile_conversion_requires_both_source_and_target_curves() {
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        profiles: vec![coefficient_profile(
            "InvisibleDiveWater",
            [0.000238, 0.03112, 0.000007306, 0.0000004427, -3.892e-9, 0.0],
        )],
        ..InsvMetadata::default()
    };
    let error = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::new(Housing::InvisibleDiveCase, Environment::Underwater),
            OffsetSource::Current,
        )
        .expect_err("the physical pixel scale requires the encoded setup profile");

    assert!(error.to_string().contains("bare optical profile"));
}

#[test]
fn parses_classifier_profile_payload() {
    let name = "invisibleDive";
    let mut payload = vec![0x0a, name.len() as u8];
    payload.extend_from_slice(name.as_bytes());
    payload.extend([0x10, 0x02]);
    let parsed = ParsedEmbeddedProfile::parse(&EmbeddedProfile {
        name: name.into(),
        payload,
    })
    .expect("classifier protobuf");

    assert_eq!(
        parsed.payload,
        EmbeddedProfilePayload::ClassificationValue(2)
    );
}

#[test]
fn supplied_x5_sample_resolves_without_decoding_media() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("VID_20181001_225939_00_002.insv");
    if !path.is_file() {
        return;
    }

    let file = File::open(&path).expect("open supplied sample");
    let mut reader = InsvReader::new(file).expect("bounded sample reader");
    let metadata = reader.metadata().expect("sample metadata");
    let calibration = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .expect("sample current X5 calibration");

    assert_eq!(calibration.offset_version, 6);
    assert_eq!(calibration.offset_flags, FLAGS);
    assert_eq!(
        calibration.lenses[0].model,
        LensProjectionModel::OmniRadtanPro
    );
    assert_eq!(calibration.lenses[0].xi, Some(2.0));
    assert_eq!(calibration.lenses[0].distortion_coefficients.len(), 13);
    assert_eq!(
        (calibration.canvas_width, calibration.canvas_height),
        (10624, 5312)
    );
    assert_eq!(
        calibration.profile_name.as_deref(),
        Some("InvisibleDiveWaterPro")
    );
    assert_eq!(calibration.lenses[0].lens_type, 119);

    let bare = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::new(Housing::None, Environment::Air),
            OffsetSource::Current,
        )
        .expect("explicit caller setup overrides the recorded accessory state");
    assert_eq!(bare.profile_name.as_deref(), Some("bare"));
    assert_eq!(bare.lenses[0].lens_type, 113);
    assert_eq!(bare.lenses[1].lens_type, 113);

    let profile = metadata
        .profiles
        .iter()
        .find(|profile| profile.name == "InvisibleDiveWater")
        .expect("sample underwater profile");
    assert!(matches!(
        ParsedEmbeddedProfile::parse(profile)
            .expect("sample profile protobuf")
            .payload,
        EmbeddedProfilePayload::SixCoefficientTransform(_)
    ));
}

fn coefficient_profile(name: &str, coefficients: [f64; 6]) -> EmbeddedProfile {
    let mut payload = vec![0x0a, name.len() as u8];
    payload.extend_from_slice(name.as_bytes());
    for coefficient in coefficients {
        payload.push(0x11);
        payload.extend_from_slice(&coefficient.to_le_bytes());
    }
    EmbeddedProfile {
        name: name.into(),
        payload,
    }
}

#[test]
fn pro_housing_revisions_have_distinct_geometry_and_do_not_double_convert() {
    for (state, housing, environment, id, name) in [
        (
            OffsetState::DiveCase2023Underwater,
            Housing::InvisibleDiveCase,
            Environment::Underwater,
            117,
            "InvisibleDiveWater",
        ),
        (
            OffsetState::DiveCaseProUnderwater,
            Housing::DiveCasePro,
            Environment::Underwater,
            119,
            "InvisibleDiveWaterPro",
        ),
        (
            OffsetState::DiveCaseProAboveWater,
            Housing::DiveCasePro,
            Environment::Air,
            120,
            "InvisibleDiveAirPro",
        ),
    ] {
        let raw = v6_offset(55.0, id);
        let metadata = InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            offsets: vec![offset(6, false, raw.clone())],
            offset_state: Some(state),
            ..InsvMetadata::default()
        };
        let result = CalibrationResolver::default()
            .resolve_metadata(
                &metadata,
                &OpticalSelection::default(),
                OffsetSource::Current,
            )
            .unwrap();
        assert_eq!(result.raw_offset, raw);
        assert_eq!(result.lenses[0].fx, 55.0);
        assert_eq!(result.profile_name.as_deref(), Some(name));
        let resolution = result.optical_resolution.unwrap();
        assert_eq!(resolution.effective.housing, housing);
        assert_eq!(resolution.effective.environment, environment);
        assert_eq!(
            (resolution.source_lens_id, resolution.target_lens_id),
            (id, id)
        );
    }
    let original = v6_offset(55.0, 113);
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, original)],
        offset_state: Some(OffsetState::DiveCaseProUnderwater),
        ..InsvMetadata::default()
    };
    let resolver = CalibrationResolver::default();
    let underwater = resolver
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .unwrap();
    let air = resolver
        .resolve_metadata(
            &metadata,
            &OpticalSelection {
                environment: Environment::Air,
                ..OpticalSelection::default()
            },
            OffsetSource::Current,
        )
        .unwrap();
    assert_eq!(underwater.lenses[0].lens_type, 119);
    assert_eq!(air.lenses[0].lens_type, 120);
    assert_ne!(underwater.lenses[0].fx, air.lenses[0].fx);
    assert_eq!(underwater.lenses[0].cx, air.lenses[0].cx);
    assert_eq!(underwater.lenses[0].orientation, air.lenses[0].orientation);
    assert_eq!(underwater.lenses[0].translation, air.lenses[0].translation);
    assert_eq!(
        underwater.lenses[0].distortion_coefficients[5..],
        air.lenses[0].distortion_coefficients[5..]
    );
}

#[test]
fn explicit_optics_override_unknown_detection_and_conflicts_are_typed() {
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, v6_offset(55.0, 113))],
        offset_state: Some(OffsetState::Automatic),
        guard_detected_type: Some(GuardDetectedType::Unknown),
        ..InsvMetadata::default()
    };
    let resolver = CalibrationResolver::default();
    assert!(resolver
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current
        )
        .is_err());
    assert_eq!(
        resolver
            .resolve_metadata(
                &metadata,
                &OpticalSelection::new(Housing::None, Environment::Air),
                OffsetSource::Current
            )
            .unwrap()
            .lenses[0]
            .lens_type,
        113
    );
    let contradictory = OpticalSelection {
        lens_accessory: LensAccessory::ProtectorA,
        ..OpticalSelection::new(Housing::DiveCasePro, Environment::Underwater)
    };
    assert!(matches!(
        resolver.resolve_metadata(&metadata, &contradictory, OffsetSource::Current),
        Err(Error::ConflictingOptics { .. })
    ));
}

#[test]
fn sensor_crop_is_center_aware_and_does_not_crop_an_encoded_canvas_twice() {
    let original = CalibrationResolver::default()
        .resolve_embedded_offset(
            &offset(6, false, v6_offset(55.0, 113)),
            &OpticalSelection::default(),
        )
        .unwrap();
    let width = original.canvas_width / 2;
    let height = original.canvas_height;
    let crop = insta360_rs::container::CropWindow {
        source_width: width,
        source_height: height,
        destination_width: width - 8,
        destination_height: height - 12,
        x_offset: 2,
        y_offset: -3,
        unknown_fields: vec![],
    };
    let mut metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![offset(6, false, original.raw_offset.clone())],
        crop_window: Some(crop.clone()),
        ..InsvMetadata::default()
    };
    let cropped = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .unwrap();
    assert_eq!(cropped.canvas_width, 2 * (width - 8));
    assert_eq!(cropped.canvas_height, height - 12);
    for i in 0..2 {
        assert_eq!(
            cropped.lenses[i].cx,
            original.lenses[i].cx - 6.0 - i as f64 * 8.0
        );
        assert_eq!(cropped.lenses[i].cy, original.lenses[i].cy - 3.0);
        assert_eq!(cropped.lenses[i].fx, original.lenses[i].fx);
        assert_eq!(
            cropped.lenses[i].distortion_coefficients,
            original.lenses[i].distortion_coefficients
        );
    }
    assert!(
        cropped
            .optical_resolution
            .as_ref()
            .unwrap()
            .sensor_crop_applied
    );
    // Construct a recorded offset already in the destination canvas, independently
    // of the normalization routine and its raw-offset serialization behavior.
    let fields = metadata.offsets[0]
        .value
        .split('_')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut fields = fields;
    for start in [1, 28] {
        fields[start + 24] = (2 * (width - 8)).to_string();
        fields[start + 25] = (height - 12).to_string();
    }
    metadata.offsets[0].value = fields.join("_");
    let already = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .unwrap();
    assert!(!already.optical_resolution.unwrap().sensor_crop_applied);
    assert_eq!(already.lenses[0].cx, original.lenses[0].cx);
}

#[test]
fn optical_configuration_serialization_and_concurrent_resolution_stress() {
    let old = serde_json::json!({"optical_setup":"StrictAuto","calibration_policy":"PreferNewest","stabilization":"Off","seam_mode":"Fixed","backend":"Cpu","projection":null});
    assert!(serde_json::from_value::<insta360_rs::StitchConfig>(old)
        .unwrap_err()
        .to_string()
        .contains("optical_setup"));
    let config = insta360_rs::StitchConfig::underwater_photogrammetry(Housing::Auto).unwrap();
    assert_eq!(config.environment, Environment::Underwater);
    assert_eq!(config.housing, Housing::Auto);
    assert_eq!(
        config.underwater_color.mode,
        insta360_rs::UnderwaterColorMode::Off
    );
    assert_eq!(
        serde_json::from_str::<insta360_rs::StitchConfig>(&serde_json::to_string(&config).unwrap())
            .unwrap(),
        config
    );
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let metadata = InsvMetadata {
                    camera_name: Some("Insta360 X5".into()),
                    offsets: vec![offset(6, false, v6_offset(55.0, 119))],
                    offset_state: Some(OffsetState::DiveCaseProUnderwater),
                    ..InsvMetadata::default()
                };
                for _ in 0..256 {
                    let value = CalibrationResolver::default()
                        .resolve_metadata(
                            &metadata,
                            &OpticalSelection::default(),
                            OffsetSource::Current,
                        )
                        .unwrap();
                    assert_eq!(value.lenses[0].lens_type, 119);
                    assert_eq!(value.lenses[0].fx, 55.0);
                    assert_eq!(
                        value.optical_resolution.unwrap().effective.housing,
                        Housing::DiveCasePro
                    );
                }
            });
        }
    });
}
