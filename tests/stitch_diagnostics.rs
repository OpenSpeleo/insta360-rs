use insta360_rs::calibration::OffsetSource;
use insta360_rs::container::{
    CropWindow, EmbeddedOffset, EmbeddedProfile, InsvMetadata, OffsetState,
};
use insta360_rs::stitch::diagnostics;
use insta360_rs::{CalibrationResolver, EquirectangularProjection, LensFrame, OpticalSelection};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct RecordingFixture {
    camera: String,
    firmware: String,
    source_dimensions: [u32; 2],
    v6_current_and_original: String,
    profiles: BTreeMap<String, Vec<u8>>,
    crop: RecordedCrop,
    native_reference: NativeReference,
    source_annotations: SourceAnnotations,
}

#[derive(Deserialize)]
struct RecordedCrop {
    source: u32,
    destination: u32,
    x_offset: i32,
    y_offset: i32,
}

#[derive(Deserialize)]
struct NativeReference {
    focal_by_lens: [f64; 2],
    radial_radius_at_90_degrees_by_lens: [f64; 2],
}

#[derive(Deserialize)]
struct SourceAnnotations {
    housing_points_by_lens: [Vec<[u32; 2]>; 2],
    scene_points_by_lens: [Vec<[u32; 2]>; 2],
}

impl RecordingFixture {
    fn load() -> Self {
        serde_json::from_str(include_str!(
            "fixtures/x5-standard-underwater-20260823.json"
        ))
        .expect("exact standard X5 optical metadata fixture")
    }

    fn metadata(&self) -> InsvMetadata {
        InsvMetadata {
            camera_name: Some(self.camera.clone()),
            firmware: Some(self.firmware.clone()),
            offsets: [false, true]
                .map(|original| EmbeddedOffset {
                    version: 6,
                    original,
                    value: self.v6_current_and_original.clone(),
                })
                .into(),
            profiles: self
                .profiles
                .iter()
                .map(|(name, payload)| EmbeddedProfile {
                    name: name.clone(),
                    payload: payload.clone(),
                })
                .collect(),
            offset_state: Some(OffsetState::DiveCase2023Underwater),
            blend_angle: Some(0),
            crop_window: Some(CropWindow {
                source_width: self.crop.source,
                source_height: self.crop.source,
                destination_width: self.crop.destination,
                destination_height: self.crop.destination,
                x_offset: self.crop.x_offset,
                y_offset: self.crop.y_offset,
                unknown_fields: Vec::new(),
            }),
            ..InsvMetadata::default()
        }
    }
}

#[test]
fn real_standard_x5_uses_native_profile_and_retains_measured_calibration() {
    let fixture = RecordingFixture::load();
    let metadata = fixture.metadata();
    let calibration = CalibrationResolver::default()
        .resolve_metadata(
            &metadata,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .expect("recorded standard housing resolves without media decoding");
    assert_eq!(
        (calibration.canvas_width, calibration.canvas_height),
        (10624, 5312)
    );
    for (index, lens) in calibration.lenses.iter().enumerate() {
        assert_eq!(lens.lens_type, 117);
        assert_eq!(lens.xi, Some(2.0));
        let reference = &fixture.native_reference;
        assert!((lens.fx - reference.focal_by_lens[index]).abs() < 1e-7);
        assert_eq!(lens.fx, lens.fy);
        let radius = lens.fx
            * (0.5
                + lens.distortion_coefficients[..5]
                    .iter()
                    .enumerate()
                    .map(|(power, coefficient)| coefficient * 0.5_f64.powi(3 + 2 * power as i32))
                    .sum::<f64>());
        assert!((radius - reference.radial_radius_at_90_degrees_by_lens[index]).abs() < 1e-7);
        let fields: Vec<f64> = fixture
            .v6_current_and_original
            .split('_')
            .map(|value| value.parse().unwrap())
            .collect();
        let source = &fields[1 + index * 27..1 + (index + 1) * 27];
        assert_eq!(&lens.distortion_coefficients[5..], &source[16..24]);
        assert_eq!(lens.euler_degrees, [source[5], source[6], source[7]]);
        assert_eq!(lens.translation, [source[8], source[9], source[10]]);
        assert_eq!(lens.cx, source[3] - 32.0 - index as f64 * 64.0);
        assert_eq!(lens.cy, source[4] - 32.0);
    }

    // Named descriptors from this same recording disagree with native117.
    // Their presence/absence must not substitute a different housing geometry.
    let mut without_descriptors = metadata.clone();
    without_descriptors.profiles.clear();
    let again = CalibrationResolver::default()
        .resolve_metadata(
            &without_descriptors,
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .unwrap();
    assert_eq!(calibration, again);
}

#[test]
fn real_standard_x5_excludes_independently_annotated_housing_points() {
    let fixture = RecordingFixture::load();
    let calibration = CalibrationResolver::default()
        .resolve_metadata(
            &fixture.metadata(),
            &OpticalSelection::default(),
            OffsetSource::Current,
        )
        .unwrap();
    let [width, height] = fixture.source_dimensions;
    let lenses = std::array::from_fn(|_| {
        LensFrame::new(
            width,
            height,
            vec![100; width as usize * height as usize * 3],
        )
        .unwrap()
    });
    let images = diagnostics::inspect(
        &lenses,
        &calibration,
        EquirectangularProjection {
            width: 128,
            height: 64,
        },
    )
    .unwrap();
    for lens in 0..2 {
        let mask = images.source_masks[lens].as_rgb8();
        let weight = |[x, y]: [u32; 2]| mask[((y * width + x) * 3) as usize];
        for &point in &fixture.source_annotations.housing_points_by_lens[lens] {
            assert_eq!(
                weight(point),
                0,
                "lens {lens}: visible housing {point:?} must be excluded"
            );
        }
        for &point in &fixture.source_annotations.scene_points_by_lens[lens] {
            assert_eq!(
                weight(point),
                255,
                "lens {lens}: scene {point:?} must remain supported"
            );
        }
    }
    assert!(
        images
            .ownership
            .as_rgb8()
            .chunks_exact(3)
            .all(|pixel| pixel[2] == 0),
        "housing correction must preserve full-sphere coverage"
    );
}
