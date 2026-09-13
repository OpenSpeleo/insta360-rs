//! Verified Studio video model inference through an independent MNN CPU build.
//! Inference alone does not qualify a camera's seam extraction or compositing.
use crate::Result;

pub const INPUT_WIDTH: usize = 64;
pub const INPUT_HEIGHT: usize = 544;
pub const FLOW_WIDTH: usize = 16;
pub const FLOW_HEIGHT: usize = 136;

/// Planar horizontal/vertical flow tensors in native model-output coordinates.
#[derive(Debug)]
pub struct SeamFlow {
    pub flow_f: Vec<f32>,
    pub flow_b: Vec<f32>,
}

/// One reused inference session. Each session belongs to one render planner.
#[derive(Debug)]
pub struct SeamModel {
    #[cfg(feature = "ai-stitching")]
    model: crate::mnn::Model,
}

/// Cheap build capability; this does not initialize models or scan recordings.
pub fn unavailable_reason() -> Option<&'static str> {
    if cfg!(feature = "ai-stitching") {
        None
    } else {
        Some("AI stitching requires the ai-stitching feature and its pinned independent MNN CPU runtime")
    }
}

impl SeamModel {
    pub fn new() -> Result<Self> {
        #[cfg(feature = "ai-stitching")]
        {
            use crate::assets::{AssetPolicy, BundledAssetProvider};
            let bundle = BundledAssetProvider::manifest()?;
            let asset = bundle
                .load_verified(
                    &BundledAssetProvider,
                    "ai-seam-studio-video-213-mnn",
                    AssetPolicy::Required,
                )?
                .expect("required asset");
            Ok(Self {
                model: crate::mnn::Model::open(&asset.bytes, 213)?,
            })
        }
        #[cfg(not(feature = "ai-stitching"))]
        Err(crate::Error::MissingCapability(
            unavailable_reason().expect("disabled feature").into(),
        ))
    }

    /// Inputs are NCHW `[1,3,544,64]`: the native preprocessing replicates
    /// grayscale byte intensity (0..255) into all three planes. Masks are
    /// `[1,1,544,64]` floating coverage (0..1).
    /// Outputs are NCHW `[1,2,136,16]`; callers must apply the qualified
    /// strip-coordinate transform instead of treating these as panorama pixels.
    pub fn infer(
        &mut self,
        first: &[f32],
        second: &[f32],
        mask_first: &[f32],
        mask_second: &[f32],
    ) -> Result<SeamFlow> {
        #[cfg(feature = "ai-stitching")]
        {
            if first
                .iter()
                .chain(second)
                .any(|value| !(0.0..=255.0).contains(value))
                || mask_first
                    .iter()
                    .chain(mask_second)
                    .any(|value| !(0.0..=1.0).contains(value))
            {
                return Err(crate::Error::InvalidMedia(
                    "AI seam grayscale or coverage range mismatch".into(),
                ));
            }
            let mut flow = SeamFlow {
                flow_f: vec![0.0; 2 * FLOW_HEIGHT * FLOW_WIDTH],
                flow_b: vec![0.0; 2 * FLOW_HEIGHT * FLOW_WIDTH],
            };
            self.model.run(
                &[first, second, mask_first, mask_second],
                &mut [&mut flow.flow_f, &mut flow.flow_b],
            )?;
            Ok(flow)
        }
        #[cfg(not(feature = "ai-stitching"))]
        {
            let _ = (first, second, mask_first, mask_second);
            Err(crate::Error::MissingCapability(
                unavailable_reason().expect("disabled feature").into(),
            ))
        }
    }
}

#[cfg(all(test, feature = "ai-stitching"))]
pub(crate) mod tests {
    use super::*;

    fn gray(x: i32, y: i32) -> f32 {
        let (gx, rx) = (x.div_euclid(8), x.rem_euclid(8));
        let (gy, ry) = (y.div_euclid(8), y.rem_euclid(8));
        let lattice = |a: i32, b: i32| -> i32 {
            let mut value = (a as u32)
                .wrapping_mul(0x9e37_79b1)
                .wrapping_add((b as u32).wrapping_mul(0x85eb_ca77));
            value ^= value >> 16;
            value = value.wrapping_mul(0x7feb_352d);
            (value >> 24) as i32
        };
        ((lattice(gx, gy) * (8 - rx) * (8 - ry)
            + lattice(gx + 1, gy) * rx * (8 - ry)
            + lattice(gx, gy + 1) * (8 - rx) * ry
            + lattice(gx + 1, gy + 1) * rx * ry)
            / 64) as f32
    }

    pub(crate) fn input(dx: i32, dy: i32) -> Vec<f32> {
        let plane: Vec<_> = (0..544)
            .flat_map(|y| (0..64).map(move |x| gray(x - dx, y - dy)))
            .collect();
        plane.repeat(3)
    }

    #[test]
    fn every_flow_value_matches_independent_direct_mnn_reference() {
        // The reference uses the direct MNN C++ API, without this adapter or
        // shim. See tests/reference/README.md for generation and input recipe.
        let bytes = include_bytes!("../tests/fixtures/seam-mnn-reference-v1.bin");
        assert_eq!(&bytes[..8], b"ISAIREF1");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 7);
        let mut references = bytes[12..]
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()));
        let first = input(0, 0);
        let mut model = SeamModel::new().unwrap();
        let session = model.model.allocation_identity();
        let cases = [
            (0, 0, false),
            (4, 0, false),
            (-4, 0, false),
            (0, 4, false),
            (0, -4, false),
            (4, 4, false),
            (4, 0, true),
        ];
        for (case, (dx, dy, masked)) in cases.into_iter().enumerate() {
            let second = input(dx, dy);
            let mask_first: Vec<_> = (0..544 * 64)
                .map(|i| u8::from(!masked || (8..56).contains(&(i % 64))) as f32)
                .collect();
            let mask_second: Vec<_> = (0..544 * 64)
                .map(|i| u8::from(!masked || (4..60).contains(&(i % 64))) as f32)
                .collect();
            let result = model
                .infer(&first, &second, &mask_first, &mask_second)
                .unwrap();
            assert_eq!(
                model.model.allocation_identity(),
                session,
                "reuse one session"
            );
            for (index, actual) in result.flow_f.iter().chain(&result.flow_b).enumerate() {
                let expected = references.next().expect("complete reference tensor");
                let tolerance = 0.0005 + expected.abs() * 0.0005;
                assert!(
                    (actual - expected).abs() <= tolerance,
                    "case {case} element {index}: {actual} vs {expected}"
                );
            }
            // Direction is independently known from the generated image shift.
            // Accuracy varies with synthetic texture; this is not a claim of
            // pixel-exact optical-flow estimation or camera qualification.
            if !masked && (dx == 0 || dy == 0) {
                for (channel, direction) in [(0, dx.signum()), (1, dy.signum())] {
                    if direction == 0 {
                        continue;
                    }
                    let median = |values: &[f32]| {
                        let mut interior: Vec<_> = (16..120)
                            .flat_map(|y| (4..12).map(move |x| values[channel * 2176 + y * 16 + x]))
                            .collect();
                        interior.sort_by(f32::total_cmp);
                        interior[interior.len() / 2]
                    };
                    assert!(median(&result.flow_f) * direction as f32 > 0.25);
                    assert!(median(&result.flow_b) * (direction as f32) < -0.25);
                }
            }
        }
        assert!(references.next().is_none());
    }

    #[test]
    fn rejects_wrong_shapes_nonfinite_values_and_invalid_coverage() {
        let mut model = SeamModel::new().unwrap();
        let mut first = input(0, 0);
        let second = first.clone();
        let mut mask = vec![1.0; 544 * 64];
        assert!(model.infer(&[], &second, &mask, &mask).is_err());
        first[19] = f32::NAN;
        assert!(model.infer(&first, &second, &mask, &mask).is_err());
        first[19] = 256.0;
        assert!(model.infer(&first, &second, &mask, &mask).is_err());
        first[19] = second[19];
        mask[7] = 1.1;
        assert!(model.infer(&first, &second, &mask, &mask).is_err());
        mask[7] = 1.0;
        assert!(model.infer(&first, &second, &mask[..10], &mask).is_err());
        model.infer(&first, &second, &mask, &mask).unwrap();
    }

    #[test]
    fn verifies_original_and_decoded_resource_identity_before_native_parsing() {
        let original = insta360_rs_data_ai_stitch_video::PAYLOADS
            .iter()
            .find(|(path, _)| path.ends_with("/model.ins"))
            .unwrap()
            .1;
        let decoded = crate::mnn::decode_model(original, 213).unwrap();
        assert_eq!(decoded.len(), 4_002_160);
        assert!(crate::mnn::decode_model(&original[..2000], 213).is_err());
        assert!(crate::mnn::decode_model(original, 214).is_err());
        let mut corrupt = original.to_vec();
        corrupt[4000] ^= 1;
        assert!(crate::mnn::decode_model(&corrupt, 213).is_err());
    }
}

#[cfg(all(test, not(feature = "ai-stitching")))]
mod disabled_tests {
    #[test]
    fn missing_independent_runtime_is_a_typed_capability_error() {
        assert!(super::unavailable_reason()
            .unwrap()
            .contains("ai-stitching"));
        assert!(matches!(
            super::SeamModel::new(),
            Err(crate::Error::MissingCapability(_))
        ));
    }
}
