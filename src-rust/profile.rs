//! Data-driven camera, lens, and optical fallback profiles.
//!
//! Per-recording offsets remain authoritative for intrinsics and extrinsics.
//! This registry only supplies camera identification and vendor-derived
//! constants that are not measurements of an individual camera.

mod curves;
mod housings;
pub use curves::{physical_curve, physical_curves, PhysicalCurve};
pub use housings::{housing_references, HousingReference};

use crate::optics::OpticalProfile;
use crate::types::{CameraModel, OpticalSelection, ProjectionGeneration};

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

/// Exact vendor distribution inspected for profile evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileSource {
    /// Insta360 iOS SDK 1.10.4, ARM64 INSCoreMedia.
    IosSdk1104,
    /// Insta360 Android SDK 2.1.5, ARM64 libarvbmg.so in the demo APK.
    AndroidSdk215,
}

/// Auditable origin of a camera-profile value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileProvenance {
    /// Exact vendor distribution supporting this item.
    pub source: ProfileSource,
    /// Form in which the evidence is distributed.
    pub kind: ProfileEvidenceKind,
    /// Confidence assigned after static review.
    pub confidence: ProfileEvidenceConfidence,
    /// Path relative to the named SDK distribution or application bundle.
    pub source_path: &'static str,
    /// Symbol, declaration, or item establishing the value.
    pub evidence: &'static str,
}

impl ProfileProvenance {
    /// Distribution containing the source path.
    pub const fn software(self) -> &'static str {
        match self.source {
            ProfileSource::IosSdk1104 => "Insta360 iOS SDK",
            ProfileSource::AndroidSdk215 => "Insta360 Android SDK",
        }
    }
    /// SDK release inspected for these constants.
    pub const fn version(self) -> &'static str {
        match self.source {
            ProfileSource::IosSdk1104 => "1.10.4",
            ProfileSource::AndroidSdk215 => "2.1.5",
        }
    }
    /// Hash of the arm64 binary when the evidence is native code.
    pub const fn binary_sha256(self) -> Option<&'static str> {
        match self.kind {
            ProfileEvidenceKind::SdkBinary => Some(match self.source {
                ProfileSource::IosSdk1104 => {
                    "3b905b46e46053d9c426c4af8bb28e449666d3ddf1c03419ab564ed2b0a01409"
                }
                ProfileSource::AndroidSdk215 => {
                    "6cea9beda80ffe53eea85f04503a07cd25ff7a573b466df6d8e54aec7575a7e0"
                }
            }),
            ProfileEvidenceKind::SdkHeader => None,
        }
    }
}

/// Lens-wide projection constants used only when a recording omits them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectionFallback {
    /// Full diagonal fisheye field of view in degrees.
    pub full_fov_degrees: f64,
    /// Full angular support of the stitch blend mask in degrees.
    pub blend_angle_degrees: Option<f64>,
}

/// One angular sample in a calibrated source-pixel fisheye boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskBoundaryPoint {
    /// Absolute source-pixel azimuth in degrees.
    pub azimuth_degrees: f64,
    /// Half field of view at this azimuth, in degrees.
    pub half_fov_degrees: f64,
}

/// Order of angular interpolation and lens projection for a radial mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaskBoundaryInterpolation {
    /// Interpolate squared source radii after projecting the angular knots.
    ProjectedRadius,
    /// Interpolate half-FOV angles before projecting the boundary.
    Angle,
}

/// Recipe for an accessory-specific, source-pixel radial mask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialMaskRecipe {
    /// Lower-hemisphere boundary samples. The upper hemisphere uses the last radius.
    pub lower_hemisphere_boundary: &'static [MaskBoundaryPoint],
    /// Native order of boundary interpolation and lens projection.
    pub interpolation: MaskBoundaryInterpolation,
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
    pub selection: OpticalSelection,
    pub(crate) optical_profile: OpticalProfile,
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
    pub fn blend_angle_radians(self) -> Option<f64> {
        self.fallback.blend_angle_degrees.map(f64::to_radians)
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
    source: ProfileSource::IosSdk1104,
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
        half_fov_degrees: 91.800_003_051_757_81,
    },
    MaskBoundaryPoint {
        azimuth_degrees: 60.0,
        half_fov_degrees: 94.0,
    },
];

const X5_DIVE_MASK: RadialMaskRecipe = RadialMaskRecipe {
    lower_hemisphere_boundary: DIVE_MASK_BOUNDARY,
    interpolation: MaskBoundaryInterpolation::ProjectedRadius,
    feather_weight_per_pixel: 0.243_902_444_839_477_54,
    provenance: ProfileProvenance {
        source: ProfileSource::IosSdk1104,
        kind: ProfileEvidenceKind::SdkBinary,
        confidence: EXACT_BINARY,
        source_path: CORE_MEDIA_BINARY,
        evidence: "TemplateBlenderImpl::calcFisheyeMaskONEX5AndProtector at 0x16d5dd4 selects 117/118; float32 table 0x5149e50; Method3 calls; L2 mask5 and scalar 0x3fcf383200000000 at 0x16d6750",
    },
};

const X5_PRO_MASK: RadialMaskRecipe = RadialMaskRecipe {
    lower_hemisphere_boundary: &[
        MaskBoundaryPoint {
            azimuth_degrees: 0.0,
            half_fov_degrees: 91.0,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 10.0,
            half_fov_degrees: 91.0,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 32.0,
            half_fov_degrees: 92.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 55.0,
            half_fov_degrees: 93.5,
        },
    ],
    interpolation: MaskBoundaryInterpolation::ProjectedRadius,
    feather_weight_per_pixel: 0.243_902_444_839_477_54,
    provenance: ProfileProvenance {
        source: ProfileSource::IosSdk1104,
        kind: ProfileEvidenceKind::SdkBinary,
        confidence: EXACT_BINARY,
        source_path: CORE_MEDIA_BINARY,
        evidence: "arm64 TemplateBlenderImpl::calcFisheyeMaskONEX5AndProtector 0x16d58fc; Pro table 0x5145630; distanceTransform L2 mask5 and scalar 0x3fcf383200000000 at 0x16d6750",
    },
};

const X4_DIVE_MASK: RadialMaskRecipe = RadialMaskRecipe {
    lower_hemisphere_boundary: &[
        MaskBoundaryPoint {
            azimuth_degrees: 0.0,
            half_fov_degrees: 91.0,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 13.0,
            half_fov_degrees: 91.0,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 18.0,
            half_fov_degrees: 91.0,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 24.0,
            half_fov_degrees: 94.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 28.0,
            half_fov_degrees: 95.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 37.0,
            half_fov_degrees: 96.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 45.0,
            half_fov_degrees: 96.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 53.5,
            half_fov_degrees: 96.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 57.0,
            half_fov_degrees: 97.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 61.0,
            half_fov_degrees: 98.5,
        },
        MaskBoundaryPoint {
            azimuth_degrees: 70.0,
            half_fov_degrees: 99.5,
        },
    ],
    interpolation: MaskBoundaryInterpolation::Angle,
    feather_weight_per_pixel: 0.243_902_444_839_477_54,
    provenance: binary_provenance(
        "TemplateBlenderImpl::calcFisheyeMaskONEX4AndProtector 0x16d4a14 selects 86/87; tables 0x5149cc0/0x5149d00/0x5149ce0/0x5149cb0/0x5149ae8; Method2 call 0x16d537c; L2 mask5 at 0x16d56b0",
    ),
};

const X6_DIVE_MASK: RadialMaskRecipe = RadialMaskRecipe {
    lower_hemisphere_boundary: DIVE_MASK_BOUNDARY,
    interpolation: MaskBoundaryInterpolation::Angle,
    feather_weight_per_pixel: 0.243_902_444_839_477_54,
    provenance: binary_provenance(
        "TemplateBlenderImpl::calcFisheyeMaskC9AndProtector 0x16d7ee0 selects 198/199; table 0x5149e50 copied to both lenses; Method2 call 0x16d8834; L2 mask5 at 0x16d8b68",
    ),
};

macro_rules! lens {
    ($id:literal, $setup:expr, $fov:literal, $blend:literal, $id_symbol:literal, $class:literal) => {
        LensProfile {
            lens_id: $id,
            selection: $setup.selection(),
            optical_profile: $setup,
            fallback: ProjectionFallback {
                full_fov_degrees: $fov,
                blend_angle_degrees: Some($blend),
            },
            mask_recipe: None,
            lens_id_provenance: ProfileProvenance {
                source: ProfileSource::IosSdk1104,
                kind: ProfileEvidenceKind::SdkHeader,
                confidence: EXACT_HEADER,
                source_path: LENS_HEADER,
                evidence: $id_symbol,
            },
            fallback_provenance: ProfileProvenance {
                source: ProfileSource::IosSdk1104,
                kind: ProfileEvidenceKind::SdkBinary,
                confidence: EXACT_BINARY,
                source_path: CORE_MEDIA_BINARY,
                evidence: concat!("-[", $class, " fov]/-[", $class, " blendAngle]"),
            },
        }
    };
}

const ONE_LENSES: &[LensProfile] = &[
    lens!(
        13,
        OpticalProfile::BareAir,
        210.0,
        200.0,
        "INSLensTypeOne",
        "INSOneLens"
    ),
    lens!(
        17,
        OpticalProfile::WaterproofCase,
        210.0,
        196.0,
        "INSLensTypeOneWaterproof",
        "INSOneWaterproofLens"
    ),
];

const ONE_R_LENSES: &[LensProfile] = &[
    lens!(
        33,
        OpticalProfile::BareAir,
        200.0,
        198.0,
        "INSLensTypeOneR577Pano",
        "INSOneR577PanoLens"
    ),
    lens!(
        38,
        OpticalProfile::DiveCaseAir,
        200.0,
        198.0,
        "INSLensTypeOneR577PanoDiving",
        "INSOneR577PanoDivingLens"
    ),
    lens!(
        40,
        OpticalProfile::DiveCaseUnderwater,
        200.0,
        198.0,
        "INSLensTypeOneR577PanoDivingWater",
        "INSOneR577PanoDivingWaterLens"
    ),
    lens!(
        39,
        OpticalProfile::ClipOnLensGuard,
        190.0,
        184.0,
        "INSLensTypeOneR577PanoProtect",
        "INSOneR577PanoProtectLens"
    ),
    lens!(
        51,
        OpticalProfile::ClipOnLensGuard,
        190.0,
        182.0,
        "INSLensTypeOneR577PanoX2Protect",
        "INSOneR577PanoX2ProtectLens"
    ),
    lens!(
        59,
        OpticalProfile::AdhesiveSphereLensGuard,
        190.0,
        184.0,
        "INSLensTypeOneR577PanoSphereProtect",
        "INSOneR577PanoSphereProtectLens"
    ),
];

const ONE_RS_LENSES: &[LensProfile] = &[
    ONE_R_LENSES[0],
    ONE_R_LENSES[1],
    ONE_R_LENSES[2],
    ONE_R_LENSES[3],
    ONE_R_LENSES[4],
    ONE_R_LENSES[5],
    lens!(
        62,
        OpticalProfile::BareAir,
        200.0,
        198.0,
        "INSLensTypeOneRS283FishEye",
        "INSOneRS283FishEyeLens"
    ),
];

const X4_AIR_LENSES: &[LensProfile] = &[
    x4_air_dive_lens(
        147,
        OpticalProfile::InvisibleDiveCaseUnderwater,
        190.0,
        "LensTypeB2HJDivingWater literal 147 at 0x1e23db8; converter 62 at 0x5b9a410",
    ),
    x4_air_dive_lens(
        148,
        OpticalProfile::InvisibleDiveCaseUnderwater,
        190.0,
        "LensTypeB2LGDivingWater literal 148 at 0x1e23dbc; converter 63 at 0x5b9a3f0",
    ),
    x4_air_dive_lens(
        149,
        OpticalProfile::InvisibleDiveCaseAir,
        200.0,
        "LensTypeB2HJDivingAir literal 149 at 0x1e23dc0; converter 64 at 0x5b9b574",
    ),
    x4_air_dive_lens(
        150,
        OpticalProfile::InvisibleDiveCaseAir,
        200.0,
        "LensTypeB2LGDivingAir literal 150 at 0x1e23dc4; converter 65 at 0x5b9cad0",
    ),
    lens!(
        131,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeB2",
        "INSB2Lens"
    ),
    lens!(
        142,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeB2LG",
        "INSB2LGLens"
    ),
    lens!(
        140,
        OpticalProfile::ProtectorAS,
        195.0,
        190.0,
        "INSLensTypeB2ASProtectHJ",
        "INSB2ASProtectHJLens"
    ),
    lens!(
        141,
        OpticalProfile::ProtectorAS,
        195.0,
        190.0,
        "INSLensTypeB2ASProtectLG",
        "INSB2ASProtectLGLens"
    ),
];

const X1_LENSES: &[LensProfile] = &[
    lens!(
        19,
        OpticalProfile::BareAir,
        200.0,
        198.0,
        "INSLensTypeOne2",
        "INSOne2Lens"
    ),
    lens!(
        24,
        OpticalProfile::DiveCaseUnderwater,
        196.0,
        190.0,
        "INSLensTypeOneXDivingWater",
        "INSOneXDivingWaterLens"
    ),
    lens!(
        27,
        OpticalProfile::WaterproofCase,
        190.0,
        185.0,
        "INSLensTypeOneXWaterproof",
        "INSOneXWaterproofLens"
    ),
    lens!(
        29,
        OpticalProfile::DiveCaseAir,
        196.0,
        190.0,
        "INSLensTypeOneXDivingAir",
        "INSOneXDivingAirLens"
    ),
];

const X2_LENSES: &[LensProfile] = &[
    lens!(
        41,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX2",
        "INSOneXS577Lens"
    ),
    lens!(
        42,
        OpticalProfile::AdhesiveSphereLensGuard,
        190.0,
        184.0,
        "INSLensTypeOneX2SphereProtect",
        "INSOneXS577SphereProtectLens"
    ),
    lens!(
        43,
        OpticalProfile::DiveCaseUnderwater,
        190.0,
        189.0,
        "INSLensTypeOneX2DrivingWater",
        "INSOneXS577ProtectDrivingWaterLens"
    ),
    lens!(
        44,
        OpticalProfile::DiveCaseAir,
        200.0,
        183.0,
        "INSLensTypeOneX2DrivingAir",
        "INSOneXS577ProtectDrivingAirLens"
    ),
    lens!(
        52,
        OpticalProfile::ClipOnLensGuard,
        190.0,
        184.0,
        "INSLensTypeOneX2Protect",
        "INSOneXS577ProtectLens"
    ),
];

const X3_LENSES: &[LensProfile] = &[
    lens!(
        70,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX3586PanoLianChuang",
        "INSOneX3586LianChuangLens"
    ),
    lens!(
        71,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX3586PanoHongJing",
        "INSOneX3586HongJingLens"
    ),
    lens!(
        76,
        OpticalProfile::ProtectorS,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingProtect",
        "INSOneX3586HongJingProtectLens"
    ),
    lens!(
        77,
        OpticalProfile::ProtectorA,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingSphereProtect",
        "INSOneX3586HongJingSphereProtectLens"
    ),
    lens!(
        78,
        OpticalProfile::DiveCaseUnderwater,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDrivingWater",
        "INSOneX3586HongJingDrivingWaterLens"
    ),
    lens!(
        79,
        OpticalProfile::DiveCaseAir,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDrivingAir",
        "INSOneX3586HongJingDrivingAirLens"
    ),
    lens!(
        84,
        OpticalProfile::ProtectorAS,
        190.0,
        186.0,
        "INSLensTypeOneX3SphereProtectPlasticMergeGlass",
        "INSOneX3SphereProtectPlasticMergeGlass"
    ),
    lens!(
        86,
        OpticalProfile::InvisibleDiveCaseUnderwater,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDivingVer2Water",
        "INSOneX3586HongJingDivingVer2WaterLens"
    ),
    lens!(
        87,
        OpticalProfile::InvisibleDiveCaseAir,
        190.0,
        186.0,
        "INSLensTypeOneX3586HongJingDivingVer2Air",
        "INSOneX3586HongJingDivingVer2AirLens"
    ),
];

const X4_LENSES: &[LensProfile] = &[
    LensProfile {
        mask_recipe: Some(X4_DIVE_MASK),
        lens_id_provenance: binary_provenance(
            "OffsetConvert::convertOffset X4 selector 47 ->86 at 0x1e31d10; calcFisheyeMaskONEX4AndProtector selects 86/87 at 0x16d4a14",
        ),
        ..X3_LENSES[7]
    },
    LensProfile {
        mask_recipe: Some(X4_DIVE_MASK),
        lens_id_provenance: binary_provenance(
            "OffsetConvert::convertOffset X4 selector 48 ->87 at 0x1e31d20; calcFisheyeMaskONEX4AndProtector selects 86/87 at 0x16d4a14",
        ),
        ..X3_LENSES[8]
    },
    lens!(
        71,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeOneX4586PanoHongJing",
        "INSOneX3586HongJingLens"
    ),
    lens!(
        106,
        OpticalProfile::ProtectorS,
        190.0,
        186.0,
        "INSLensTypeOneX4SphereProtectGlass",
        "INSOneX4SphereProtectGlassLens"
    ),
    lens!(
        107,
        OpticalProfile::ProtectorA,
        190.0,
        186.0,
        "INSLensTypeOneX4SphereProtectPlasticCement",
        "INSOneX4SphereProtectPlasticCementLens"
    ),
    lens!(
        108,
        OpticalProfile::ProtectorAS,
        190.0,
        186.0,
        "INSLensTypeOneX4SphereProtectPlasticMergeGlass",
        "INSOneX4SphereProtectPlasticMergeGlass"
    ),
];

const X5_LENSES: &[LensProfile] = &[
    lens!(
        113,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeA3",
        "INSA3Lens"
    ),
    LensProfile {
        lens_id: 114,
        selection: OpticalProfile::BareUnderwater.selection(),
        optical_profile: OpticalProfile::BareUnderwater,
        fallback: ProjectionFallback {
            full_fov_degrees: 190.0,
            blend_angle_degrees: Some(186.0),
        },
        mask_recipe: None,
        lens_id_provenance: ProfileProvenance {
            source: ProfileSource::IosSdk1104,
            kind: ProfileEvidenceKind::SdkBinary,
            confidence: EXACT_BINARY,
            source_path: CORE_MEDIA_BINARY,
            evidence: "Insta360Lens::GetFov case 114/INSOffsetConvertTypeOneA3_2_BareUnderWater",
        },
        fallback_provenance: ProfileProvenance {
            source: ProfileSource::IosSdk1104,
            kind: ProfileEvidenceKind::SdkBinary,
            confidence: EXACT_BINARY,
            source_path: CORE_MEDIA_BINARY,
            evidence: "Insta360Lens::GetFov case 114/lens blend-angle table case 114",
        },
    },
    lens!(
        115,
        OpticalProfile::ProtectorA,
        194.0,
        190.0,
        "INSLensTypeA3SphereProtectPlasticCement",
        "INSA3LensSphereProtectPlasticCement"
    ),
    LensProfile {
        mask_recipe: Some(X5_DIVE_MASK),
        ..lens!(
            117,
            OpticalProfile::InvisibleDiveCaseUnderwater,
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
            OpticalProfile::InvisibleDiveCaseAir,
            200.0,
            186.0,
            "INSLensTypeA3DivingAir",
            "INSA3LensDivingAir"
        )
    },
    LensProfile {
        lens_id: 119,
        selection: OpticalProfile::DiveCaseProUnderwater.selection(),
        optical_profile: OpticalProfile::DiveCaseProUnderwater,
        fallback: ProjectionFallback {
            full_fov_degrees: 190.0,
            blend_angle_degrees: Some(190.0),
        },
        mask_recipe: Some(X5_PRO_MASK),
        lens_id_provenance: binary_provenance("INSLensTypeA3DivingWaterPro literal 119 at 0x55f4c4c"),
        fallback_provenance: binary_provenance(
            "ins::Lens::getFov 0x1f9a30 table 0x50736b8 case119; TemplateBlenderBase constructor0x16c9974 overlap10deg, getLeftSphereAlpha0x16cfb14 halves to +/-5deg",
        ),
    },
    LensProfile {
        lens_id: 120,
        selection: OpticalProfile::DiveCaseProAir.selection(),
        optical_profile: OpticalProfile::DiveCaseProAir,
        fallback: ProjectionFallback {
            full_fov_degrees: 200.0,
            blend_angle_degrees: Some(190.0),
        },
        mask_recipe: Some(X5_PRO_MASK),
        lens_id_provenance: binary_provenance("INSLensTypeA3DivingAirPro literal 120 at 0x55f4c50"),
        fallback_provenance: binary_provenance(
            "ins::Lens::getFov 0x1f9a30 table 0x50736b8 case120; TemplateBlenderBase constructor0x16c9974 overlap10deg, getLeftSphereAlpha0x16cfb14 halves to +/-5deg",
        ),
    },
];

const X6_LENSES: &[LensProfile] = &[
    lens!(
        193,
        OpticalProfile::BareAir,
        200.0,
        190.0,
        "INSLensTypeC9",
        "INSC9Lens"
    ),
    LensProfile {
        lens_id: 198,
        selection: OpticalProfile::InvisibleDiveCaseUnderwater.selection(),
        optical_profile: OpticalProfile::InvisibleDiveCaseUnderwater,
        fallback: ProjectionFallback {
            full_fov_degrees: 190.0,
            blend_angle_degrees: Some(190.0),
        },
        mask_recipe: Some(X6_DIVE_MASK),
        lens_id_provenance: binary_provenance(
            "arvrender::CameraLensType::LensTypeC9DivingWater literal 198 at 0x55f4c80",
        ),
        fallback_provenance: binary_provenance(
            "ins::Lens::getFov 0x1f9a30 table 0x50736b8 case198; TemplateBlenderBase constructor0x16c9974 overlap10deg, getLeftSphereAlpha0x16cfb14 halves to +/-5deg",
        ),
    },
    LensProfile {
        lens_id: 199,
        selection: OpticalProfile::InvisibleDiveCaseAir.selection(),
        optical_profile: OpticalProfile::InvisibleDiveCaseAir,
        fallback: ProjectionFallback {
            full_fov_degrees: 190.0,
            blend_angle_degrees: Some(190.0),
        },
        mask_recipe: Some(X6_DIVE_MASK),
        lens_id_provenance: binary_provenance(
            "arvrender::CameraLensType::LensTypeC9DivingAir literal 199 at 0x55f4c84",
        ),
        fallback_provenance: binary_provenance(
            "ins::Lens::getFov 0x1f9a30 table 0x50736b8 case199; TemplateBlenderBase constructor0x16c9974 overlap10deg, getLeftSphereAlpha0x16cfb14 halves to +/-5deg",
        ),
    },
];

const fn x4_air_dive_lens(
    id: u32,
    optical_profile: OpticalProfile,
    fov: f64,
    evidence: &'static str,
) -> LensProfile {
    LensProfile {
        lens_id: id,
        selection: optical_profile.selection(),
        optical_profile,
        fallback: ProjectionFallback {
            full_fov_degrees: fov,
            blend_angle_degrees: Some(190.0),
        },
        mask_recipe: Some(RadialMaskRecipe {
            provenance: android_binary_provenance(
                "calcFisheyeMaskB2AndProtector at 0x40a10a8 selects 147..150; eleven knots shared by both lenses; Method2 calls 0x40a1768/0x40a192c; L2 mask5 at 0x40a1aac and scalar 0x3fcf383200000000",
            ),
            ..X4_DIVE_MASK
        }),
        lens_id_provenance: android_binary_provenance(evidence),
        fallback_provenance: android_binary_provenance(
            "ins::Lens::getFov0x3fede38 table 0x1b9b7c8:147/148190deg,149/150200deg; TemplateBlenderBase constructor0x4092974 overlap10deg, getLeftSphereAlpha0x4099fa8 halves to +/-5deg",
        ),
    }
}

const fn android_binary_provenance(evidence: &'static str) -> ProfileProvenance {
    ProfileProvenance {
        source: ProfileSource::AndroidSdk215,
        kind: ProfileEvidenceKind::SdkBinary,
        confidence: ProfileEvidenceConfidence::Exact,
        source_path: "AndroidSDKDemo/app-debug-2.1.5_1787657291340.apk!/lib/arm64-v8a/libarvbmg.so",
        evidence,
    }
}

const fn header_provenance(evidence: &'static str) -> ProfileProvenance {
    ProfileProvenance {
        source: ProfileSource::IosSdk1104,
        kind: ProfileEvidenceKind::SdkHeader,
        confidence: ProfileEvidenceConfidence::Corroborated,
        source_path: CAMERA_HEADER,
        evidence,
    }
}

const fn binary_provenance(evidence: &'static str) -> ProfileProvenance {
    ProfileProvenance {
        source: ProfileSource::IosSdk1104,
        kind: ProfileEvidenceKind::SdkBinary,
        confidence: ProfileEvidenceConfidence::Corroborated,
        source_path: CORE_MEDIA_BINARY,
        evidence,
    }
}

static CAMERA_PROFILES: [CameraProfile; 10] = [
    CameraProfile {
        camera: CameraModel::One,
        canonical_name: "Insta360 ONE",
        aliases: &["Insta360 ONE", "ONE"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: ONE_LENSES,
        provenance: &[binary_provenance("kInsta360CameraNameOne")],
    },
    CameraProfile {
        camera: CameraModel::OneR,
        canonical_name: "Insta360 ONE R",
        aliases: &["Insta360 ONE R", "ONE R", "OneR"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: ONE_R_LENSES,
        provenance: &[binary_provenance("kInsta360CameraNameOneR")],
    },
    CameraProfile {
        camera: CameraModel::OneRS,
        canonical_name: "Insta360 ONE RS",
        aliases: &[
            "Insta360 ONE RS",
            "ONE RS",
            "OneRS",
            "Insta360 ONE RS 1-Inch 360 Edition",
        ],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: ONE_RS_LENSES,
        provenance: &[binary_provenance("kInsta360CameraNameOneRS")],
    },
    CameraProfile {
        camera: CameraModel::X4Air,
        canonical_name: "Insta360 X4 Air",
        aliases: &["Insta360 X4 Air", "X4AIR", "B2", "Insta360 B2"],
        projection_generations: PROJECTION_GENERATIONS,
        projection_provenance: PROJECTION_PROVENANCE,
        lenses: X4_AIR_LENSES,
        provenance: &[binary_provenance(
            "kInsta360CameraNameB2; product mapping corroborated Android SDK 2.1.5 CameraType.X4AIR",
        )],
    },
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
        assert_eq!(camera_profiles().len(), 10);
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
                if let Some(blend) = lens.fallback.blend_angle_degrees {
                    assert!(blend.is_finite());
                    assert!((180.0..=360.0).contains(&lens.fallback.full_fov_degrees));
                    assert!((180.0..=lens.fallback.full_fov_degrees).contains(&blend));
                }
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
            lens_profile(&CameraModel::X3, 71).unwrap().optical_profile,
            OpticalProfile::BareAir
        );
        assert_eq!(
            lens_profile(&CameraModel::X4, 71).unwrap().optical_profile,
            OpticalProfile::BareAir
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
                blend_angle_degrees: Some(198.0)
            }
        );
        assert!((x1.half_fov_radians() - 100.0_f64.to_radians()).abs() < f64::EPSILON);
        assert!((x1.blend_angle_radians().unwrap() - 198.0_f64.to_radians()).abs() < f64::EPSILON);

        let x2_water = lens_profile(&CameraModel::X2, 43).unwrap();
        assert_eq!(
            x2_water.fallback,
            ProjectionFallback {
                full_fov_degrees: 190.0,
                blend_angle_degrees: Some(189.0)
            }
        );

        let x5_water = lens_profile(&CameraModel::X5, 117).unwrap();
        let mask = x5_water.mask_recipe.unwrap();
        assert_eq!(mask.lower_hemisphere_boundary, DIVE_MASK_BOUNDARY);
        assert_eq!(mask.feather_weight_per_pixel, 0.243_902_444_839_477_54);
        assert_eq!(
            lens_profile(&CameraModel::X5, 113).unwrap().mask_recipe,
            None
        );

        let x6 = lens_profile(&CameraModel::X6, 193).unwrap();
        assert_eq!(
            x6.fallback,
            ProjectionFallback {
                full_fov_degrees: 200.0,
                blend_angle_degrees: Some(190.0)
            }
        );
        assert_eq!(
            x6.fallback_provenance.confidence,
            ProfileEvidenceConfidence::Exact
        );
    }
}
