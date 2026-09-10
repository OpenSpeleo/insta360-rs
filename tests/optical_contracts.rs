//! Cross-entry-point optical configuration contracts.
use insta360_rs::calibration::{CalibrationCandidate, OffsetSource};
use insta360_rs::container::{EmbeddedOffset, InsvMetadata};
use insta360_rs::{CalibrationResolver, Environment, Housing, MountingAccessory, OpticalSelection};

fn embedded() -> EmbeddedOffset {
    // Two V6 bare X5 lens records; all tangential/prism terms are zero.
    EmbeddedOffset {
        version: 6,
        original: false,
        value: concat!(
            "2_2_55_55.5_50_50_0_0_0_0_0_0_",
            "0_0_0_0_0_0_0_0_0_0_0_0_0_200_100_113_",
            "2_55_55.5_150_50_0_0_0_0_0_0_",
            "0_0_0_0_0_0_0_0_0_0_0_0_0_200_100_113_394240"
        )
        .into(),
    }
}

#[test]
fn shared_housing_ids_can_be_inspected_without_guessing_camera_specific_masks() {
    let resolver = CalibrationResolver::default();
    for (lens_id, environment) in [(86, Environment::Underwater), (87, Environment::Air)] {
        let mut offset = embedded();
        offset.value = offset.value.replace("_113_", &format!("_{lens_id}_"));
        let metadata = InsvMetadata {
            offsets: vec![offset],
            ..InsvMetadata::default()
        };
        let inspection = resolver.inspect_metadata_optics(&metadata, OffsetSource::Current);
        assert_eq!(inspection.encoded_lens_id, Some(lens_id));
        assert_eq!(
            inspection.evidence,
            Some(insta360_rs::optics::OpticalEvidence::EncodedLens)
        );
        assert_eq!(
            inspection.detected,
            Some(OpticalSelection::new(
                Housing::InvisibleDiveCase,
                environment
            ))
        );
        assert_eq!(inspection.ambiguity, None);
        // X3 and X4 agree on the housing, but require different radial masks.
        // Rendering still needs camera evidence to resolve that geometry.
        assert!(matches!(
            resolver.resolve_metadata(
                &metadata,
                &OpticalSelection::default(),
                OffsetSource::Current
            ),
            Err(insta360_rs::Error::MissingCalibration(_))
        ));
        for camera in ["Insta360 X3", "Insta360 X4"] {
            let mut identified = metadata.clone();
            identified.camera_name = Some(camera.into());
            resolver
                .resolve_metadata(
                    &identified,
                    &OpticalSelection::default(),
                    OffsetSource::Current,
                )
                .unwrap()
                .validate_for_stitching()
                .unwrap();
        }
    }
}

#[test]
fn all_resolver_entry_points_preserve_optical_resolution() {
    let resolver = CalibrationResolver::default();
    let raw = embedded();
    for mounting_accessory in [MountingAccessory::None, MountingAccessory::Auto] {
        let selection = OpticalSelection {
            mounting_accessory,
            ..OpticalSelection::new(Housing::None, Environment::Air)
        };
        // Explicit optical fields still select the bare candidate when only
        // mount detection is automatic and a different optical candidate exists.
        let candidates = [
            CalibrationCandidate::new(6, Some("bare".into()), &raw.value),
            CalibrationCandidate::new(6, Some("InvisibleDiveWater".into()), &raw.value),
        ];
        let expected = resolver.resolve_embedded_offset(&raw, &selection).unwrap();
        let actual = resolver.resolve(&candidates, &selection).unwrap();
        assert_eq!(actual.optical_resolution, expected.optical_resolution);
        let metadata = InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            offsets: vec![raw.clone()],
            ..InsvMetadata::default()
        };
        let actual = resolver
            .resolve_metadata(&metadata, &selection, OffsetSource::Current)
            .unwrap();
        assert_eq!(actual.optical_resolution, expected.optical_resolution);
    }
}

#[test]
fn candidate_resolution_rejects_incompatible_mounting_accessory() {
    let resolver = CalibrationResolver::default();
    let raw = embedded();
    let selection = OpticalSelection {
        mounting_accessory: MountingAccessory::DiveBuddy,
        ..OpticalSelection::new(Housing::None, Environment::Air)
    };
    let candidate = CalibrationCandidate::new(6, Some("bare".into()), &raw.value);
    assert!(resolver.resolve_embedded_offset(&raw, &selection).is_err());
    assert!(resolver.resolve(&[candidate], &selection).is_err());
}

#[test]
fn partial_auto_uses_the_selected_candidates_encoded_lens_identity() {
    let resolver = CalibrationResolver::default();
    let raw = embedded();
    let selection = OpticalSelection {
        environment: Environment::Air,
        ..OpticalSelection::default()
    };
    let metadata = InsvMetadata {
        camera_name: Some("Insta360 X5".into()),
        offsets: vec![raw.clone()],
        ..InsvMetadata::default()
    };
    let expected = resolver
        .resolve_metadata(&metadata, &selection, OffsetSource::Current)
        .unwrap();
    let candidate = CalibrationCandidate::new(6, Some("bare".into()), &raw.value);
    let actual = resolver.resolve(&[candidate], &selection).unwrap();
    assert_eq!(actual.optical_resolution, expected.optical_resolution);
}

#[test]
fn unnamed_candidates_can_use_encoded_identity_without_treating_optional_profiles_as_detection() {
    let resolver = CalibrationResolver::default();
    let raw = embedded();
    let unnamed = CalibrationCandidate::new(6, None, &raw.value);
    let auto = OpticalSelection::default();
    let resolved = resolver
        .resolve(std::slice::from_ref(&unnamed), &auto)
        .unwrap();
    assert_eq!(
        resolved.optical_resolution.unwrap().effective,
        OpticalSelection::new(Housing::None, Environment::Air)
    );
    // An additional optional named profile is a separate user choice, not
    // evidence that the camera used it instead of the unnamed calibration.
    let named = CalibrationCandidate::new(6, Some("optional-profile".into()), &raw.value);
    assert!(matches!(
        resolver.resolve(&[unnamed, named], &auto),
        Err(insta360_rs::Error::AmbiguousOpticalSetup { .. })
    ));
}
