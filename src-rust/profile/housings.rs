//! Additional official housing references whose complete optical models are not registered.
use crate::{Environment, Housing};

/// Official accessory evidence retained without inventing missing correction values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HousingReference {
    pub camera: &'static str,
    pub housing: Housing,
    pub environment: Environment,
    pub lens_ids: &'static [u32],
    /// Software distribution and version containing the artifact.
    pub software: &'static str,
    /// Path within that distribution; never a workstation path.
    pub path: &'static str,
    pub evidence: &'static str,
    /// What is still unavailable or explicitly unsupported.
    pub limitation: &'static str,
}

const REFERENCES: &[HousingReference] = &[
    HousingReference {
        camera: "ONE X2",
        housing: Housing::SphericalDiveCase,
        environment: Environment::Underwater,
        lens_ids: &[53],
        software: "Insta360 iOS SDK 1.10.4",
        path: super::LENS_HEADER,
        evidence: "INSLensTypeOneX2SphereDrivingWater",
        limitation: "Native spherical housing conversion is explicitly unsupported; no portable conversion is enabled.",
    },
    HousingReference {
        camera: "ONE X2",
        housing: Housing::SphericalDiveCase,
        environment: Environment::Air,
        lens_ids: &[54],
        software: "Insta360 iOS SDK 1.10.4",
        path: super::LENS_HEADER,
        evidence: "INSLensTypeOneX2SphereDrivingAir",
        limitation: "Native spherical housing conversion is explicitly unsupported; no portable conversion is enabled.",
    },
    HousingReference {
        camera: "G01 modular camera",
        housing: Housing::DiveCase,
        environment: Environment::Underwater,
        lens_ids: &[],
        software: "Insta360 iOS SDK 1.10.4",
        path: "iOS_v1.10.4/INSCameraSDKSample-bluetooth/Frameworks/INSCoreMedia.xcframework/ios-arm64/INSCoreMedia.framework/Headers/INSProtectorClassify.h",
        evidence: "detectG01ModuleUnderWaterWithSource:bareOffset:distortionType:environmentUnderWater:modelIDPathList:debugInfo:",
        limitation: "Classifier/distortion selectors are established; a complete physical housing calibration is not inferred from an image classifier.",
    },
];

/// Official references supplementing the executable lens registry.
///
/// Entries here describe evidence and limitations, not enabled correction profiles.
pub fn housing_references() -> &'static [HousingReference] {
    REFERENCES
}
