//! Shared optical selection and recorded-state resolution.

use crate::types::{Environment, Housing, LensAccessory, MountingAccessory, OpticalSelection};
use serde::{Deserialize, Serialize};

/// Optical accessory or environment encoded by a lens calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub(crate) enum OpticalProfile {
    /// Require recorded metadata to select an unambiguous setup.
    StrictAuto,
    /// Bare lenses used in air.
    BareAir,
    /// Bare lenses used underwater.
    BareUnderwater,
    /// Legacy waterproof housing.
    WaterproofCase,
    /// Legacy dive housing used in air.
    DiveCaseAir,
    /// Legacy dive housing used underwater.
    DiveCaseUnderwater,
    /// Full-invisible dive housing used in air.
    InvisibleDiveCaseAir,
    /// Full-invisible dive housing used underwater.
    InvisibleDiveCaseUnderwater,
    /// Pro dive housing used underwater.
    DiveCaseProUnderwater,
    /// Pro dive housing used in air.
    DiveCaseProAir,
    /// Spherical dive housing used underwater.
    SphericalDiveCaseUnderwater,
    /// Spherical dive housing used in air.
    SphericalDiveCaseAir,
    /// Clip-on curved lens guard.
    ClipOnLensGuard,
    /// Adhesive spherical lens guard.
    AdhesiveSphereLensGuard,
    /// Standard/plastic A-grade lens protector.
    ProtectorA,
    /// Premium/glass S-grade lens protector.
    ProtectorS,
    /// Average curve used for automatic A/S protector selection.
    ProtectorAS,
    /// ND16 neutral-density filter.
    Nd16,
    /// ND32 neutral-density filter.
    Nd32,
    /// ND64 neutral-density filter.
    Nd64,
    /// ND128 neutral-density filter.
    Nd128,
}

impl OpticalProfile {
    /// Returns the embedded optical profile name used by recorded metadata.
    pub fn profile_name(&self) -> Option<&'static str> {
        match self {
            Self::StrictAuto => None,
            Self::BareAir => Some("bare"),
            Self::BareUnderwater => Some("BareUnderwater"),
            Self::WaterproofCase => Some("Waterproof"),
            Self::DiveCaseAir => Some("DivingAir"),
            Self::DiveCaseUnderwater => Some("DivingWater"),
            Self::InvisibleDiveCaseAir => Some("InvisibleDiveAir"),
            Self::InvisibleDiveCaseUnderwater => Some("InvisibleDiveWater"),
            Self::DiveCaseProUnderwater => Some("InvisibleDiveWaterPro"),
            Self::DiveCaseProAir => Some("InvisibleDiveAirPro"),
            Self::SphericalDiveCaseUnderwater => Some("SphericalDiveWater"),
            Self::SphericalDiveCaseAir => Some("SphericalDiveAir"),
            Self::ClipOnLensGuard => Some("Protect"),
            Self::AdhesiveSphereLensGuard => Some("SphereProtect"),
            Self::ProtectorA => Some("ProtectorA"),
            Self::ProtectorS => Some("ProtectorS"),
            Self::ProtectorAS => Some("ProtectorAS"),
            Self::Nd16 => Some("ND16"),
            Self::Nd32 => Some("ND32"),
            Self::Nd64 => Some("ND64"),
            Self::Nd128 => Some("ND128"),
        }
    }
}

impl OpticalProfile {
    pub(crate) const fn selection(self) -> OpticalSelection {
        use Environment::{Air, Underwater};
        match self {
            Self::StrictAuto => OpticalSelection {
                housing: Housing::Auto,
                environment: Environment::Auto,
                lens_accessory: LensAccessory::Auto,
                mounting_accessory: MountingAccessory::Auto,
            },
            Self::BareAir => OpticalSelection::new(Housing::None, Air),
            Self::BareUnderwater => OpticalSelection::new(Housing::None, Underwater),
            Self::WaterproofCase => OpticalSelection::new(Housing::VentureCase, Underwater),
            Self::DiveCaseAir => OpticalSelection::new(Housing::DiveCase, Air),
            Self::DiveCaseUnderwater => OpticalSelection::new(Housing::DiveCase, Underwater),
            Self::InvisibleDiveCaseAir => OpticalSelection::new(Housing::InvisibleDiveCase, Air),
            Self::InvisibleDiveCaseUnderwater => {
                OpticalSelection::new(Housing::InvisibleDiveCase, Underwater)
            }
            Self::DiveCaseProAir => OpticalSelection::new(Housing::DiveCasePro, Air),
            Self::DiveCaseProUnderwater => OpticalSelection::new(Housing::DiveCasePro, Underwater),
            Self::SphericalDiveCaseAir => OpticalSelection::new(Housing::SphericalDiveCase, Air),
            Self::SphericalDiveCaseUnderwater => {
                OpticalSelection::new(Housing::SphericalDiveCase, Underwater)
            }
            Self::ClipOnLensGuard => {
                OpticalSelection::with_lens_accessory(LensAccessory::ClipOnLensGuard)
            }
            Self::AdhesiveSphereLensGuard => {
                OpticalSelection::with_lens_accessory(LensAccessory::AdhesiveSphereLensGuard)
            }
            Self::ProtectorA => OpticalSelection::with_lens_accessory(LensAccessory::ProtectorA),
            Self::ProtectorS => OpticalSelection::with_lens_accessory(LensAccessory::ProtectorS),
            Self::ProtectorAS => OpticalSelection::with_lens_accessory(LensAccessory::ProtectorAS),
            Self::Nd16 => OpticalSelection::with_lens_accessory(LensAccessory::Nd16),
            Self::Nd32 => OpticalSelection::with_lens_accessory(LensAccessory::Nd32),
            Self::Nd64 => OpticalSelection::with_lens_accessory(LensAccessory::Nd64),
            Self::Nd128 => OpticalSelection::with_lens_accessory(LensAccessory::Nd128),
        }
    }

    pub(crate) fn from_selection(selection: OpticalSelection) -> crate::Result<Self> {
        const PROFILES: &[OpticalProfile] = &[
            OpticalProfile::BareAir,
            OpticalProfile::BareUnderwater,
            OpticalProfile::WaterproofCase,
            OpticalProfile::DiveCaseAir,
            OpticalProfile::DiveCaseUnderwater,
            OpticalProfile::InvisibleDiveCaseAir,
            OpticalProfile::InvisibleDiveCaseUnderwater,
            OpticalProfile::DiveCaseProAir,
            OpticalProfile::DiveCaseProUnderwater,
            OpticalProfile::SphericalDiveCaseAir,
            OpticalProfile::SphericalDiveCaseUnderwater,
            OpticalProfile::ClipOnLensGuard,
            OpticalProfile::AdhesiveSphereLensGuard,
            OpticalProfile::ProtectorA,
            OpticalProfile::ProtectorS,
            OpticalProfile::ProtectorAS,
            OpticalProfile::Nd16,
            OpticalProfile::Nd32,
            OpticalProfile::Nd64,
            OpticalProfile::Nd128,
        ];
        if selection == OpticalSelection::default() {
            return Ok(Self::StrictAuto);
        }
        PROFILES.iter().copied().find(|profile| {
            let value = profile.selection();
            value.housing == selection.housing && value.environment == selection.environment && value.lens_accessory == selection.lens_accessory
        }).ok_or_else(|| crate::Error::ConflictingOptics { selection, reason: "housing, environment and lens accessory do not form a supported optical combination".into() })
    }
}

/// Which metadata supplied the detected optical selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpticalEvidence {
    /// Firmware-recorded accessory/environment state, including stored guard results.
    RecordedState,
    /// The unambiguous lens ID in the selected offset.
    EncodedLens,
}

/// Optical choices retained for inspection and reproducible rendering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpticalResolution {
    pub requested: OpticalSelection,
    pub detected: OpticalSelection,
    pub effective: OpticalSelection,
    pub evidence: OpticalEvidence,
    /// Whether the metadata sensor window changed the source coordinate mapping.
    pub sensor_crop_applied: bool,
    pub source_lens_id: u32,
    pub target_lens_id: u32,
}

/// Detection results available during bounded probing, even when rendering is unsupported.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpticalInspection {
    pub detected: Option<OpticalSelection>,
    pub evidence: Option<OpticalEvidence>,
    pub encoded_lens_id: Option<u32>,
    /// Reason automatic selection could not be established.
    pub ambiguity: Option<String>,
}

pub(crate) fn merge_selection(
    requested: OpticalSelection,
    detected: OpticalSelection,
) -> crate::Result<OpticalSelection> {
    let effective = OpticalSelection {
        housing: if requested.housing == Housing::Auto {
            detected.housing
        } else {
            requested.housing
        },
        environment: if requested.environment == Environment::Auto {
            detected.environment
        } else {
            requested.environment
        },
        lens_accessory: if requested.lens_accessory == LensAccessory::Auto {
            detected.lens_accessory
        } else {
            requested.lens_accessory
        },
        mounting_accessory: if requested.mounting_accessory == MountingAccessory::Auto {
            detected.mounting_accessory
        } else {
            requested.mounting_accessory
        },
    };
    if effective.housing == Housing::Auto
        || effective.environment == Environment::Auto
        || effective.lens_accessory == LensAccessory::Auto
        || effective.mounting_accessory == MountingAccessory::Auto
    {
        return Err(crate::Error::AmbiguousOpticalSetup {
            candidates: vec![format!("unresolved optical request {effective:?}")],
        });
    }
    if effective.mounting_accessory == MountingAccessory::DiveBuddy
        && (effective.environment != Environment::Underwater
            || matches!(
                effective.housing,
                Housing::Auto | Housing::None | Housing::VentureCase
            ))
    {
        return Err(crate::Error::ConflictingOptics { selection: effective, reason: "Dive Buddy requires an underwater dive housing; no separate mount calibration is established".into() });
    }
    OpticalProfile::from_selection(effective)?;
    Ok(effective)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mount_preserves_detected_mount_and_explicit_none_overrides_it() {
        let detected = OpticalSelection {
            mounting_accessory: MountingAccessory::DiveBuddy,
            ..OpticalSelection::new(Housing::DiveCase, Environment::Underwater)
        };
        assert_eq!(
            merge_selection(OpticalSelection::default(), detected).unwrap(),
            detected
        );
        let explicit_none = OpticalSelection {
            mounting_accessory: MountingAccessory::None,
            ..OpticalSelection::default()
        };
        assert_eq!(
            merge_selection(explicit_none, detected)
                .unwrap()
                .mounting_accessory,
            MountingAccessory::None
        );
        assert!(merge_selection(
            OpticalSelection::default(),
            OpticalSelection {
                mounting_accessory: MountingAccessory::Auto,
                ..detected
            }
        )
        .is_err());
    }
}
