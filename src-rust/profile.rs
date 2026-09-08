//! Data-driven camera, lens, and optical fallback profiles.
//!
//! Per-recording offsets remain authoritative for intrinsics and extrinsics.
//! This registry only supplies camera identification and vendor-derived
//! constants that are not measurements of an individual camera.

use crate::types::{CameraModel, OpticalSetup, ProjectionGeneration};

const LENS_HEADER: &str = "iOS_v1.10.4/INSCameraSDKSample-bluetooth/Frameworks/INSCoreMedia.xcframework/ios-arm64/INSCoreMedia.framework/Headers/INSLensOffset.h";
const OFFSET_HEADER: &str = "iOS_v1.10.4/INSCameraSDKSample-bluetooth/Frameworks/INSCoreMedia.xcframework/ios-arm64/INSCoreMedia.framework/Headers/INSOffsetUtil.h";
const CAMERA_HEADER: &str = "iOS_v1.10.4/INSCameraSDKSample-bluetooth/Frameworks/INSCoreMedia.xcframework/ios-arm64/INSCoreMedia.framework/Headers/INSExtraMetadata.h";
const CORE_MEDIA_BINARY: &str = "iOS_v1.10.4/INSCameraSDKSample-bluetooth/Frameworks/INSCoreMedia.xcframework/ios-arm64/INSCoreMedia.framework/INSCoreMedia";

/// Kind of licensed vendor evidence supporting a registry value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileEvidenceKind {
    /// A declaration or comment in a public vendor header.
    SdkHeader,
    /// A literal or symbol recovered by static inspection of a vendor library.
    SdkBinary,
}

/// Strength of the evidence attached to a registry value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileEvidenceConfidence {
    /// The value is declared directly or returned as a literal; it is not an estimate.
    Exact,
    /// Independent vendor declarations agree on the product/internal-name mapping.
    Corroborated,
}

/// Auditable origin of a camera-profile value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileProvenance {
    /// Form in which the evidence is distributed.
    pub kind: ProfileEvidenceKind,
    /// Confidence assigned after static review.
    pub confidence: ProfileEvidenceConfidence,
    /// Repository-relative path to the licensed evidence.
    pub source_path: &'static str,
    /// Symbol, declaration, or item establishing the value.
    pub evidence: &'static str,
}

/// Lens-wide projection constants used only when a recording omits them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectionFallback {
    /// Full diagonal fisheye field of view in degrees.
    pub full_fov_degrees: f64,
    /// Full angular support of the stitch blend mask in degrees.
    pub blend_angle_degrees: f64,
}

/// One angular sample in a calibrated source-pixel fisheye boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskBoundaryPoint {
    /// Absolute source-pixel azimuth in degrees.
    pub azimuth_degrees: f64,
    /// Half field of view at this azimuth, in degrees.
    pub half_fov_degrees: f64,
}

/// Recipe for an accessory-specific, source-pixel radial mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialMaskRecipe {
    /// Lower-hemisphere boundary samples. The upper hemisphere uses the last radius.
    pub lower_hemisphere_boundary: &'static [MaskBoundaryPoint],
    /// Linear mask-weight increase per source pixel inside the boundary.
    pub feather_weight_per_pixel: f64,
    /// Static evidence for the recipe.
    pub provenance: ProfileProvenance,
}

/// Registry entry for one encoded vendor lens identifier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensProfile {
    /// Integer stored in each lens record of an Insta360 offset.
    pub lens_id: u32,
    /// Optical setup represented by that identifier.
    pub optical_setup: OpticalSetup,
    /// Vendor fallback used only when no equivalent recorded value is available.
    pub fallback: ProjectionFallback,
    /// Optional accessory-specific fisheye-mask construction recipe.
    pub mask_recipe: Option<RadialMaskRecipe>,
    /// Static evidence for the lens identifier and setup mapping.
    pub lens_id_provenance: ProfileProvenance,
    /// Static evidence for the FOV and blend-angle fallback.
    pub fallback_provenance: ProfileProvenance,
}

impl LensProfile {
    /// Half of the full fisheye FOV, in radians, for projection clipping.
    pub fn half_fov_radians(self) -> f64 {
        self.fallback.full_fov_degrees.to_radians() * 0.5
    }

    /// Full blend-mask angle in radians.
    pub fn blend_angle_radians(self) -> f64 {
        self.fallback.blend_angle_degrees.to_radians()
    }
}

/// Immutable profile for one supported consumer-camera family.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraProfile {
    /// Public camera model.
    pub camera: CameraModel,
    /// Preferred public product name.
    pub canonical_name: &'static str,
    /// Accepted public and vendor-internal camera-name aliases.
    pub aliases: &'static [&'static str],
    /// Offset projection generations the portable parser can consume.
    ///
    /// This is an acceptance list, not a claim that every camera emits every
    /// generation. Callers must inspect the offset actually present in media.
    pub projection_generations: &'static [ProjectionGeneration],
    /// Static evidence for the accepted projection-generation values.
    pub projection_provenance: ProfileProvenance,
    /// Lens identifiers established for this camera family.
    pub lenses: &'static [LensProfile],
    /// Static evidence for the product/internal-name mapping.
    pub provenance: &'static [ProfileProvenance],
}

impl CameraProfile {
    /// Returns whether this registry accepts an offset projection generation.
    pub fn accepts_projection_generation(&self, generation: ProjectionGeneration) -> bool {
        self.projection_generations.contains(&generation)
    }

    /// Looks up a lens within this camera family.
    pub fn lens(&self, lens_id: u32) -> Option<&'static LensProfile> {
        self.lenses.iter().find(|lens| lens.lens_id == lens_id)
    }
}

const EXACT_HEADER: ProfileEvidenceConfidence = ProfileEvidenceConfidence::Exact;
const EXACT_BINARY: ProfileEvidenceConfidence = ProfileEvidenceConfidence::Exact;

const PROJECTION_PROVENANCE: ProfileProvenance = ProfileProvenance {
    kind: ProfileEvidenceKind::SdkHeader,
    confidence: EXACT_HEADER,
    source_path: OFFSET_HEADER,
    evidence: "INSOffsetUtilOffsetVersionV1/V2/V3/V6",
};

const PROJECTION_GENERATIONS: &[ProjectionGeneration] = &[
    ProjectionGeneration::V1,
    ProjectionGeneration::V2,
    ProjectionGeneration::V3,
    ProjectionGeneration::V6,
];

const DIVE_MASK_BOUNDARY: &[MaskBoundaryPoint] = &[
    MaskBoundaryPoint {
        azimuth_degrees: 0.0,
        half_fov_degrees: 90.5,
    },
    MaskBoundaryPoint {
        azimuth_degrees: 10.0,
        half_fov_degrees: 90.5,
    },
    MaskBoundaryPoint {
        azimuth_degrees: 20.0,
        half_fov_degrees: 91.8,
    },
    MaskBoundaryPoint {
        azimuth_degrees: 60.0,
        half_fov_degrees: 94.0,
    },
];

const X5_DIVE_MASK: RadialMaskRecipe = RadialMaskRecipe {
    lower_hemisphere_boundary: DIVE_MASK_BOUNDARY,
    feather_weight_per_pixel: 0.249_816_324_652_783_8,
    provenance: ProfileProvenance {
        kind: ProfileEvidenceKind::SdkBinary,
        confidence: EXACT_BINARY,
        source_path: CORE_MEDIA_BINARY,
        evidence: "TemplateBlenderImpl::calcFisheyeMaskONEX5AndProtector",
    },
};

macro_rules! lens {
    ($id:literal, $setup:expr, $fov:literal, $blend:literal, $id_symbol:literal, $class:literal) => {
        LensProfile {
            lens_id: $id,
            optical_setup: $setup,
            fallback: ProjectionFallback {
                full_fov_degrees: $fov,
                blend_angle_degrees: $blend,
            },
            mask_recipe: None,
            lens_id_provenance: ProfileProvenance {
                kind: ProfileEvidenceKind::SdkHeader,
                confidence: EXACT_HEADER,
                source_path: LENS_HEADER,
                evidence: $id_symbol,
            },
            fallback_provenance: ProfileProvenance {
                kind: ProfileEvidenceKind::SdkBinary,
                confidence: EXACT_BINARY,
                source_path: CORE_MEDIA_BINARY,
                evidence: concat!("-[", $class, " fov]/-[", $class, " blendAngle]"),
            },
        }
    };
}

const X1_LENSES: &[LensProfile] = &[
    lens!(
        19,
        OpticalSetup::BareAir,
        200.0,
        198.0,
        "INSLensTypeOne2",
        "INSOne2Lens"
    ),
    lens!(
        24,
        OpticalSetup::DiveCaseUnderwater,
        196.0,
        190.0,
        "INSLensTypeOneXDivingWater",
        "INSOneXDivingWaterLens"
    ),
    lens!(
        27,
        OpticalSetup::WaterproofCase,
        190.0,
        185.0,
        "INSLensTypeOneXWaterproof",
        "INSOneXWaterproofLens"
    ),
    lens!(
        29,
        OpticalSetup::DiveCaseAir,
        196.0,
        190.0,
        "INSLensTypeOneXDivingAir",
        "INSOneXDivingAirLens"
    ),
];

const X2_LENSES: &[LensProfile] = &[
    lens!(
        41,
        OpticalSetup::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX2",
        "INSOneXS577Lens"
    ),
    lens!(
        42,
        OpticalSetup::AdhesiveSphereLensGuard,
        190.0,
        184.0,
        "INSLensTypeOneX2SphereProtect",
        "INSOneXS577SphereProtectLens"
    ),
    lens!(
        43,
        OpticalSetup::DiveCaseUnderwater,
        190.0,
        189.0,
        "INSLensTypeOneX2DrivingWater",
        "INSOneXS577ProtectDrivingWaterLens"
    ),
    lens!(
        44,
        OpticalSetup::DiveCaseAir,
        200.0,
        183.0,
        "INSLensTypeOneX2DrivingAir",
        "INSOneXS577ProtectDrivingAirLens"
    ),
    lens!(
        52,
        OpticalSetup::ClipOnLensGuard,
        190.0,
        184.0,
        "INSLensTypeOneX2Protect",
        "INSOneXS577ProtectLens"
    ),
];

const X3_LENSES: &[LensProfile] = &[
    lens!(
        70,
        OpticalSetup::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX3586PanoLianChuang",
        "INSOneX3586LianChuangLens"
    ),
    lens!(
        71,
        OpticalSetup::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX3586PanoHongJing",
        "INSOneX3586HongJingLens"
    ),
    lens!(
        76,
        OpticalSetup::ProtectorS,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingProtect",
        "INSOneX3586HongJingProtectLens"
    ),
    lens!(
        77,
        OpticalSetup::ProtectorA,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingSphereProtect",
        "INSOneX3586HongJingSphereProtectLens"
    ),
    lens!(
        78,
        OpticalSetup::DiveCaseUnderwater,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDrivingWater",
        "INSOneX3586HongJingDrivingWaterLens"
    ),
    lens!(
        79,
        OpticalSetup::DiveCaseAir,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDrivingAir",
        "INSOneX3586HongJingDrivingAirLens"
    ),
    lens!(
        84,
        OpticalSetup::ProtectorAS,
        190.0,
        186.0,
        "INSLensTypeOneX3SphereProtectPlasticMergeGlass",
        "INSOneX3SphereProtectPlasticMergeGlass"
    ),
    lens!(
        86,
        OpticalSetup::InvisibleDiveCaseUnderwater,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDivingVer2Water",
        "INSOneX3586HongJingDivingVer2WaterLens"
    ),
    lens!(
        87,
        OpticalSetup::InvisibleDiveCaseAir,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDivingVer2Air",
        "INSOneX3586HongJingDivingVer2AirLens"
    ),
];

const X4_LENSES: &[LensProfile] = &[
    lens!(
        71,
        OpticalSetup::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX4586PanoHongJing",
        "INSOneX3586HongJingLens"
    ),
    lens!(
        106,
        OpticalSetup::ProtectorS,
        190.0,
        186.0,
        "INSLensTypeOneX4SphereProtectGlass",
        "INSOneX4SphereProtectGlassLens"
    ),
    lens!(
        107,
        OpticalSetup::ProtectorA,
        190.0,
        186.0,
        "INSLensTypeOneX4SphereProtectPlasticCement",
        "INSOneX4SphereProtectPlasticCementLens"
    ),
    lens!(
        108,
        OpticalSetup::ProtectorAS,
        190.0,
        186.0,
        "INSLensTypeOneX4SphereProtectPlasticMergeGlass",
        "INSOneX4SphereProtectPlasticMergeGlass"
    ),
];

const X5_LENSES: &[LensProfile] = &[
    lens!(
        113,
        OpticalSetup::BareAir,
        200.0,
        190.0,
        "INSLensTypeA3",
        "INSA3Lens"
    ),
    LensProfile {
        lens_id: 114,
        optical_setup: OpticalSetup::BareUnderwater,
        fallback: ProjectionFallback {
            full_fov_degrees: 190.0,
            blend_angle_degrees: 186.0,
        },
        mask_recipe: None,
        lens_id_provenance: ProfileProvenance {
            kind: ProfileEvidenceKind::SdkBinary,
            confidence: EXACT_BINARY,
            source_path: CORE_MEDIA_BINARY,
            evidence: "Insta360Lens::GetFov case 114/INSOffsetConvertTypeOneA3_2_BareUnderWater",
        },
        fallback_provenance: ProfileProvenance {
            kind: ProfileEvidenceKind::SdkBinary,
            confidence: EXACT_BINARY,
            source_path: CORE_MEDIA_BINARY,
            evidence: "Insta360Lens::GetFov case 114/lens blend-angle table case 114",
        },
    },
    lens!(
        115,
        OpticalSetup::ProtectorA,
        194.0,
        190.0,
        "INSLensTypeA3SphereProtectPlasticCement",
        "INSA3LensSphereProtectPlasticCement"
    ),
    LensProfile {
        mask_recipe: Some(X5_DIVE_MASK),
        ..lens!(
            117,
            OpticalSetup::InvisibleDiveCaseUnderwater,
            190.0,
            186.0,
            "INSLensTypeA3DivingWater",
            "INSA3LensDivingWater"
        )
    },
    LensProfile {
        mask_recipe: Some(X5_DIVE_MASK),
        ..lens!(
            118,
            OpticalSetup::InvisibleDiveCaseAir,
            200.0,
            186.0,
            "INSLensTypeA3DivingAir",
            "INSA3LensDivingAir"
        )
    },
];

const X6_LENSES: &[LensProfile] = &[lens!(
    193,
    OpticalSetup::BareAir,
    200.0,
    190.0,
    "INSLensTypeC9",
    "INSC9Lens"
)];

const fn header_provenance(evidence: &'static str) -> ProfileProvenance {
    ProfileProvenance {
        kind: ProfileEvidenceKind::SdkHeader,
        confidence: ProfileEvidenceConfidence::Corroborated,
        source_path: CAMERA_HEADER,
        evidence,
    }
}

const fn binary_provenance(evidence: &'static str) -> ProfileProvenance {
    ProfileProvenance {
        kind: ProfileEvidenceKind::SdkBinary,
        confidence: ProfileEvidenceConfidence::Corroborated,
        source_path: CORE_MEDIA_BINARY,
        evidence,
    }
}

static CAMERA_PROFILES: [CameraProfile; 6] = [
    CameraProfile {
        camera: CameraModel::X1,
        canonical_name: "Insta360 ONE X",
        aliases: &["Insta360 ONE X", "ONE X", "X1", "Insta360 One2", "One2"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X1_LENSES,
        provenance: &[header_provenance("kInsta360CameraNameOne2 (OneX)")],
    },
    CameraProfile {
        camera: CameraModel::X2,
        canonical_name: "Insta360 ONE X2",
        aliases: &["Insta360 ONE X2", "ONE X2", "X2", "OneXS"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X2_LENSES,
        provenance: &[header_provenance("kInsta360CameraNameOneX2")],
    },
    CameraProfile {
        camera: CameraModel::X3,
        canonical_name: "Insta360 X3",
        aliases: &["Insta360 X3", "ONE X3", "X3"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X3_LENSES,
        provenance: &[header_provenance("kInsta360CameraNameX3")],
    },
    CameraProfile {
        camera: CameraModel::X4,
        canonical_name: "Insta360 X4",
        aliases: &["Insta360 X4", "ONE X4", "X4"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X4_LENSES,
        provenance: &[header_provenance("kInsta360CameraNameX4")],
    },
    CameraProfile {
        camera: CameraModel::X5,
        canonical_name: "Insta360 X5",
        aliases: &["Insta360 X5", "ONE X5", "X5", "Insta360 A3", "A3"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X5_LENSES,
        provenance: &[binary_provenance("kInsta360CameraNameA3/\"Insta360 X5\"")],
    },
    CameraProfile {
        camera: CameraModel::X6,
        canonical_name: "Insta360 X6",
        aliases: &["Insta360 X6", "ONE X6", "X6", "Insta360 C9", "C9"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X6_LENSES,
        provenance: &[binary_provenance("kInsta360CameraNameC9/\"Insta360 X6\"")],
    },
];

/// Returns the complete immutable camera registry.
pub fn camera_profiles() -> &'static [CameraProfile] {
    &CAMERA_PROFILES
}

/// Looks up a profile by camera model.
///
/// An [`CameraModel::Unknown`] value is also checked against registered aliases
/// so callers can resolve raw metadata before canonicalizing it.
pub fn camera_profile(camera: &CameraModel) -> Option<&'static CameraProfile> {
    match camera {
        CameraModel::Unknown(name) => camera_profile_for_name(name),
        known => CAMERA_PROFILES
            .iter()
            .find(|profile| &profile.camera == known),
    }
}

/// Looks up a camera by a case- and punctuation-insensitive vendor/product alias.
pub fn camera_profile_for_name(name: &str) -> Option<&'static CameraProfile> {
    has_normalized_char(name).then_some(())?;
    CAMERA_PROFILES.iter().find(|profile| {
        profile
            .aliases
            .iter()
            .any(|alias| normalized_name_eq(name, alias))
    })
}

/// Looks up one lens identifier in a known camera family.
pub fn lens_profile(camera: &CameraModel, lens_id: u32) -> Option<&'static LensProfile> {
    camera_profile(camera)?.lens(lens_id)
}

/// Returns every camera-specific interpretation of a lens identifier.
///
/// Some vendor identifiers are deliberately shared; notably ID 71 represents an
/// X3 supplier variant and is also the X4 bare-lens identifier.
pub fn lens_profiles_for_id(
    lens_id: u32,
) -> impl Iterator<Item = (&'static CameraProfile, &'static LensProfile)> {
    CAMERA_PROFILES.iter().flat_map(move |camera| {
        camera
            .lenses
            .iter()
            .filter(move |lens| lens.lens_id == lens_id)
            .map(move |lens| (camera, lens))
    })
}

fn has_normalized_char(name: &str) -> bool {
    name.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

fn normalized_name_eq(left: &str, right: &str) -> bool {
    let normalize = |value: u8| {
        value
            .is_ascii_alphanumeric()
            .then(|| value.to_ascii_lowercase())
    };
    left.bytes()
        .filter_map(normalize)
        .eq(right.bytes().filter_map(normalize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_internally_valid() {
        assert_eq!(camera_profiles().len(), 6);
        for (profile_index, profile) in camera_profiles().iter().enumerate() {
            assert!(!profile.canonical_name.is_empty());
            assert!(!profile.aliases.is_empty());
            assert!(!profile.lenses.is_empty());
            assert_eq!(profile.projection_generations, PROJECTION_GENERATIONS);

            for alias in profile.aliases {
                assert!(has_normalized_char(alias));
                assert_eq!(camera_profile_for_name(alias), Some(profile));
            }
            for lens in profile.lenses {
                assert!(lens.fallback.full_fov_degrees.is_finite());
                assert!(lens.fallback.blend_angle_degrees.is_finite());
                assert!((180.0..=360.0).contains(&lens.fallback.full_fov_degrees));
                assert!((180.0..=lens.fallback.full_fov_degrees)
                    .contains(&lens.fallback.blend_angle_degrees));
                assert_eq!(profile.lens(lens.lens_id), Some(lens));
            }

            for other in &camera_profiles()[profile_index + 1..] {
                assert_ne!(profile.camera, other.camera);
                for alias in profile.aliases {
                    assert!(!other
                        .aliases
                        .iter()
                        .any(|other_alias| normalized_name_eq(alias, other_alias)));
                }
            }
        }
    }

    #[test]
    fn aliases_accept_format_variants_and_reject_unknown_names() {
        assert_eq!(
            camera_profile_for_name("insta360-one-x").unwrap().camera,
            CameraModel::X1
        );
        assert_eq!(
            camera_profile_for_name(" ONE_X2 ").unwrap().camera,
            CameraModel::X2
        );
        assert_eq!(
            camera_profile_for_name("a3").unwrap().camera,
            CameraModel::X5
        );
        assert_eq!(
            camera_profile(&CameraModel::Unknown("C9".into()))
                .unwrap()
                .camera,
            CameraModel::X6
        );
        assert_eq!(camera_profile_for_name(""), None);
        assert_eq!(camera_profile_for_name("---"), None);
        assert_eq!(camera_profile_for_name("Insta360 GO 3"), None);
    }

    #[test]
    fn projection_generation_conversion_accepts_only_implemented_layouts() {
        for version in [1, 2, 3, 6] {
            let generation = ProjectionGeneration::from_offset_version(version).unwrap();
            assert_eq!(generation.offset_version(), version);
            assert!(camera_profiles()
                .iter()
                .all(|profile| profile.accepts_projection_generation(generation)));
        }
        for version in [0, 4, 5, 7, u8::MAX] {
            assert_eq!(ProjectionGeneration::from_offset_version(version), None);
        }
    }

    #[test]
    fn lens_lookup_is_camera_specific_when_ids_are_shared() {
        assert_eq!(
            lens_profile(&CameraModel::X3, 71).unwrap().optical_setup,
            OpticalSetup::BareAir
        );
        assert_eq!(
            lens_profile(&CameraModel::X4, 71).unwrap().optical_setup,
            OpticalSetup::BareAir
        );
        let matches = lens_profiles_for_id(71).collect::<Vec<_>>();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].0.camera, CameraModel::X3);
        assert_eq!(matches[1].0.camera, CameraModel::X4);

        // The vendor implementation marks the X2 sphere-dive converters behind IDs 53/54 as unsupported.
        assert_eq!(lens_profile(&CameraModel::X2, 53), None);
        assert_eq!(lens_profile(&CameraModel::X2, 54), None);
        assert_eq!(lens_profile(&CameraModel::X6, 999), None);
    }

    #[test]
    fn exact_fallbacks_and_specialized_mask_are_exposed() {
        let x1 = lens_profile(&CameraModel::X1, 19).unwrap();
        assert_eq!(
            x1.fallback,
            ProjectionFallback {
                full_fov_degrees: 200.0,
                blend_angle_degrees: 198.0
            }
        );
        assert!((x1.half_fov_radians() - 100.0_f64.to_radians()).abs() < f64::EPSILON);
        assert!((x1.blend_angle_radians() - 198.0_f64.to_radians()).abs() < f64::EPSILON);

        let x2_water = lens_profile(&CameraModel::X2, 43).unwrap();
        assert_eq!(
            x2_water.fallback,
            ProjectionFallback {
                full_fov_degrees: 190.0,
                blend_angle_degrees: 189.0
            }
        );

        let x5_water = lens_profile(&CameraModel::X5, 117).unwrap();
        let mask = x5_water.mask_recipe.unwrap();
        assert_eq!(mask.lower_hemisphere_boundary, DIVE_MASK_BOUNDARY);
        assert_eq!(mask.feather_weight_per_pixel, 0.249_816_324_652_783_8);
        assert_eq!(
            lens_profile(&CameraModel::X5, 113).unwrap().mask_recipe,
            None
        );

        let x6 = lens_profile(&CameraModel::X6, 193).unwrap();
        assert_eq!(
            x6.fallback,
            ProjectionFallback {
                full_fov_degrees: 200.0,
                blend_angle_degrees: 190.0
            }
        );
        assert_eq!(
            x6.fallback_provenance.confidence,
            ProfileEvidenceConfidence::Exact
        );
    }
}
