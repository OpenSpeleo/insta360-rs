//! Parsing and selection of the factory calibration embedded in Insta360 media.

mod housing_conversion;
mod polynomial;
pub use polynomial::{NormalizedPolynomialProjection, PolynomialCoefficientSource};

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use crate::container::{
    EmbeddedOffset, EmbeddedProfile, GuardDetectedType, InsvMetadata, OffsetState,
};
use crate::motion::Orientation;
use crate::optics::{merge_selection, OpticalEvidence, OpticalProfile, OpticalResolution};
use crate::profile::{
    camera_profile_for_name, camera_profiles, lens_profile, lens_profiles_for_id, CameraProfile,
    LensProfile,
};
use crate::types::{CalibrationPolicy, CameraModel, OpticalSelection, ProjectionGeneration};
use crate::{Error, Result};

const SUPPORTED_VERSIONS: [u8; 4] = [6, 3, 2, 1];
const MAX_OFFSET_BYTES: usize = 64 * 1024;
const MAX_LENSES: usize = 8;
const MAX_PROFILE_BYTES: usize = 4 * 1024;
const PROFILE_COEFFICIENT_COUNT: usize = 6;

// X5/A3 lens identifiers recovered from INSLensOffset.h and the INSCoreMedia
// symbols. Only identifiers whose optical setup is unambiguous are mapped.
const X5_BARE_LENS_TYPE: u32 = 113;
const X5_DIVING_WATER_LENS_TYPE: u32 = 117;
const X5_DIVING_AIR_LENS_TYPE: u32 = 118;

/// Selects one of the two copies of an offset stored by Insta360 firmware.
///
/// The current and original copies are deliberately never substituted for one
/// another: the current copy may already contain a shell conversion, while the
/// original copy is the unmodified factory calibration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OffsetSource {
    #[default]
    Current,
    Original,
}

impl OffsetSource {
    fn matches(self, offset: &EmbeddedOffset) -> bool {
        offset.original == matches!(self, Self::Original)
    }
}

/// Native camera projection encoded by an Insta360 offset generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LensProjectionModel {
    /// V1's radius-based polynomial pinhole model.
    PinholePolynomialV1,
    /// V2's radius-based polynomial pinhole model with four coefficients.
    PinholePolynomialV2,
    /// V3's unified omnidirectional model with five radtan coefficients.
    OmniRadtan,
    /// V6's unified omnidirectional model with thirteen radtan coefficients.
    OmniRadtanPro,
}

/// Decoded shape of an optical-profile protobuf submessage.
///
/// `SixCoefficientTransform` is the sixth-order angle-to-physical-radius
/// polynomial used by the vendor's offset converter. For coefficients `c`, the
/// radius is `sum(c[i] * theta_degrees.powi(i))`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EmbeddedProfilePayload {
    SixCoefficientTransform([f64; PROFILE_COEFFICIENT_COUNT]),
    ClassificationValue(u64),
}

/// A validated optical-profile descriptor from INSV metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedEmbeddedProfile {
    pub name: String,
    pub payload: EmbeddedProfilePayload,
}

impl ParsedEmbeddedProfile {
    /// Parses the small protobuf message used by X5 profile descriptors.
    pub fn parse(profile: &EmbeddedProfile) -> Result<Self> {
        parse_profile_payload(profile)
    }
}

/// One encoded calibration found in the media metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationCandidate {
    pub version: u8,
    pub profile_name: Option<String>,
    pub offset: String,
}

impl CalibrationCandidate {
    /// Creates a versioned offset candidate, optionally associated with an optical profile.
    pub fn new(version: u8, profile_name: Option<String>, offset: impl Into<String>) -> Self {
        Self {
            version,
            profile_name,
            offset: offset.into(),
        }
    }
}

/// Calibrated parameters for one fisheye lens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedLens {
    /// Offset-native projection model. Consumers should use this together with
    /// `xi` and `distortion_coefficients`; `k1..k3` are compatibility aliases.
    pub model: LensProjectionModel,
    /// Radius parameter used by V1/V2 polynomial-pinhole offsets.
    pub radius: Option<f64>,
    /// Unified omnidirectional mirror parameter used by V3/V6.
    pub xi: Option<f64>,
    pub cx: f64,
    pub cy: f64,
    pub fx: f64,
    pub fy: f64,
    pub k1: f64,
    pub k2: f64,
    pub k3: f64,
    /// Complete native coefficient vector: 0/4/5/13 values for V1/V2/V3/V6.
    pub distortion_coefficients: Vec<f64>,
    /// Prepared radian-domain projection for V1/V2, preserving the native
    /// coefficient vector and recorded radius. Older serialized records may
    /// omit this field; call [`Self::refresh_polynomial_projection`] before
    /// stitching them or after editing native polynomial parameters.
    #[serde(default)]
    pub polynomial_projection: Option<NormalizedPolynomialProjection>,
    /// The three Euler fields exactly as encoded by the offset.
    pub euler_degrees: [f64; 3],
    /// Body/sphere-to-camera rotation (`r_c_b_` in the vendor implementation). The optical axis
    /// is camera-local positive Z.
    pub orientation: Orientation,
    pub translation: [f64; 3],
    /// Dimensions of the side-by-side calibration canvas.
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub lens_type: u32,
}

impl ParsedLens {
    /// Recomputes shared CPU/GPU polynomial normalization from native fields.
    /// Other projection generations clear the optional polynomial parameters.
    /// Failure leaves previously prepared parameters unchanged.
    pub fn refresh_polynomial_projection(&mut self) -> Result<()> {
        let projection = polynomial::normalized(self)?;
        self.polynomial_projection = projection;
        Ok(())
    }

    /// Checks that the decoded parameters are finite and internally usable.
    pub fn validate(&self) -> Result<()> {
        let scalars = [
            self.cx,
            self.cy,
            self.fx,
            self.fy,
            self.k1,
            self.k2,
            self.k3,
            self.translation[0],
            self.translation[1],
            self.translation[2],
        ];
        if !scalars.iter().all(|value| value.is_finite()) {
            return Err(Error::MissingCalibration(
                "lens calibration contains a non-finite value".into(),
            ));
        }
        if self.fx <= 0.0 || self.fy <= 0.0 {
            return Err(Error::MissingCalibration(
                "lens focal lengths must be positive".into(),
            ));
        }
        if self.canvas_width == 0 || self.canvas_height == 0 {
            return Err(Error::MissingCalibration(
                "calibration canvas dimensions must be non-zero".into(),
            ));
        }
        if self
            .radius
            .into_iter()
            .chain(self.xi)
            .chain(self.euler_degrees)
            .chain(self.distortion_coefficients.iter().copied())
            .any(|value| !value.is_finite())
        {
            return Err(Error::MissingCalibration(
                "lens calibration contains a non-finite native parameter".into(),
            ));
        }
        let expected_coefficients = match self.model {
            LensProjectionModel::PinholePolynomialV1 => 0,
            LensProjectionModel::PinholePolynomialV2 => 4,
            LensProjectionModel::OmniRadtan => 5,
            LensProjectionModel::OmniRadtanPro => 13,
        };
        if self.distortion_coefficients.len() != expected_coefficients {
            return Err(Error::MissingCalibration(format!(
                "{:?} requires {expected_coefficients} distortion coefficients, found {}",
                self.model,
                self.distortion_coefficients.len()
            )));
        }
        match self.model {
            LensProjectionModel::PinholePolynomialV1 | LensProjectionModel::PinholePolynomialV2
                if !self.radius.is_some_and(|radius| radius > 0.0) =>
            {
                return Err(Error::MissingCalibration(
                    "polynomial lens calibration requires a positive radius".into(),
                ));
            }
            LensProjectionModel::OmniRadtan | LensProjectionModel::OmniRadtanPro
                if self.xi.is_none() =>
            {
                return Err(Error::MissingCalibration(
                    "omnidirectional lens calibration requires xi".into(),
                ));
            }
            _ => {}
        }
        if let Some(projection) = self.polynomial_projection {
            projection.validate()?;
            let expected = polynomial::normalized(self)?.ok_or_else(|| {
                Error::MissingCalibration(
                    "polynomial projection parameters do not match the lens model".into(),
                )
            })?;
            if !projection.matches(&expected) {
                return Err(Error::MissingCalibration("polynomial projection parameters are stale or inconsistent with the native lens fields".into()));
            }
        }
        self.orientation.validate().map(|_| ())
    }
}

/// Lens-wide angular geometry resolved before CPU or GPU rendering begins.
///
/// Intrinsics and extrinsics still come exclusively from the recording's
/// offset. These values are vendor-derived lens-family constants used for
/// projection clipping and overlap weighting.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedLensGeometry {
    /// Full diagonal fisheye field of view in degrees.
    pub full_fov_degrees: f64,
    /// Full angular support of the stitch blend mask in degrees.
    pub blend_angle_degrees: f64,
    /// True when `blend_angle_degrees` came from INSV tag 128 rather than the registry.
    pub blend_angle_recorded: bool,
}

impl ResolvedLensGeometry {
    /// Half field of view in radians, used for per-lens projection clipping.
    pub fn half_fov_radians(self) -> f64 {
        self.full_fov_degrees.to_radians() * 0.5
    }

    /// Full stitch blend angle in radians.
    pub fn blend_angle_radians(self) -> f64 {
        self.blend_angle_degrees.to_radians()
    }

    fn validate(self) -> Result<()> {
        if !self.full_fov_degrees.is_finite()
            || !self.blend_angle_degrees.is_finite()
            || !(180.0..=360.0).contains(&self.full_fov_degrees)
            || !(180.0..=self.full_fov_degrees).contains(&self.blend_angle_degrees)
        {
            return Err(Error::MissingCalibration(
                "resolved lens FOV/blend geometry is invalid".into(),
            ));
        }
        Ok(())
    }
}

/// A selected and parsed two-lens factory calibration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedCalibration {
    /// Canonical camera family when metadata identified one.
    #[serde(default)]
    pub camera_model: Option<CameraModel>,
    #[serde(default)]
    pub optical_resolution: Option<OpticalResolution>,
    pub offset_version: u8,
    pub offset_source: OffsetSource,
    /// Low-bit flags from the offset's packed trailing word.
    pub offset_flags: u32,
    pub profile_name: Option<String>,
    pub lenses: [ParsedLens; 2],
    /// Geometry resolved once and shared by CPU and GPU stitchers.
    #[serde(default)]
    pub lens_geometry: [Option<ResolvedLensGeometry>; 2],
    pub canvas_width: u32,
    pub canvas_height: u32,
    /// Selected source offset text, replaced with converted native V6 text when
    /// housing conversion runs. Sensor-coordinate normalization and later public
    /// field edits do not update this string. Use the resolved lens and canvas
    /// fields for rendering or serializing resolved geometry.
    pub raw_offset: String,
}

impl ResolvedCalibration {
    /// Checks both lens records and their shared calibration canvas.
    pub fn validate(&self) -> Result<()> {
        if !SUPPORTED_VERSIONS.contains(&self.offset_version) {
            return Err(Error::MissingCalibration(format!(
                "unsupported offset version {}",
                self.offset_version
            )));
        }
        if self.canvas_width == 0 || self.canvas_height == 0 {
            return Err(Error::MissingCalibration(
                "resolved calibration has an empty canvas".into(),
            ));
        }
        for lens in &self.lenses {
            lens.validate()?;
            if lens.canvas_width != self.canvas_width || lens.canvas_height != self.canvas_height {
                return Err(Error::MissingCalibration(
                    "lens calibration canvas dimensions disagree".into(),
                ));
            }
        }
        for geometry in self.lens_geometry.into_iter().flatten() {
            geometry.validate()?;
        }
        Ok(())
    }

    /// Verifies that the parsed calibration can be consumed by both portable renderers.
    ///
    /// V1/V2 require prepared normalization consistent with their native fields.
    pub fn validate_for_stitching(&self) -> Result<()> {
        self.validate()?;
        for (index, lens) in self.lenses.iter().enumerate() {
            if lens.lens_type != 0 {
                if matches!(
                    lens.model,
                    LensProjectionModel::PinholePolynomialV1
                        | LensProjectionModel::PinholePolynomialV2
                ) && lens.polynomial_projection.is_none()
                {
                    // Metadata parsing retains finite native fields even when
                    // their projection cannot be normalized. Surface that
                    // specific failure here, before creating render outputs.
                    polynomial::normalized(lens)?;
                    return Err(Error::MissingCalibration("V1/V2 stitching requires prepared polynomial projection parameters; refresh the native lens normalization first".into()));
                }
                self.geometry_for_lens(index)?;
            }
        }
        Ok(())
    }

    /// Returns render geometry for one lens, or a typed error when its lens ID
    /// is only parseable and has no evidence-backed projection fallback.
    pub fn geometry_for_lens(&self, index: usize) -> Result<ResolvedLensGeometry> {
        self.lens_geometry
            .get(index)
            .copied()
            .flatten()
            .ok_or_else(|| {
                let lens_type = self
                    .lenses
                    .get(index)
                    .map(|lens| lens.lens_type)
                    .unwrap_or_default();
                Error::MissingCalibration(format!(
                    "lens {index} type {lens_type} has no evidence-backed FOV/blend geometry"
                ))
            })
    }
}

/// Selects an optical profile and the newest usable embedded offset.
#[derive(Clone, Copy, Debug, Default)]
pub struct CalibrationResolver {
    policy: CalibrationPolicy,
}

impl CalibrationResolver {
    /// Creates a resolver using the requested version-selection policy.
    pub fn new(policy: CalibrationPolicy) -> Self {
        Self { policy }
    }

    /// Inspects recorded optical choices without requiring rendering support or converting geometry.
    pub fn inspect_metadata_optics(
        &self,
        metadata: &InsvMetadata,
        source: OffsetSource,
    ) -> crate::optics::OpticalInspection {
        use crate::optics::OpticalInspection;
        let candidates: Vec<_> = metadata
            .offsets
            .iter()
            .filter(|offset| source.matches(offset))
            .map(|offset| CalibrationCandidate::new(offset.version, None, &offset.value))
            .collect();
        let parsed = self.resolve_unprofiled(&candidates, source);
        let encoded_lens_id = parsed.as_ref().ok().and_then(|calibration| {
            (calibration.lenses[0].lens_type == calibration.lenses[1].lens_type)
                .then_some(calibration.lenses[0].lens_type)
        });
        let (selection, evidence) = if metadata.offset_state.is_some() {
            (
                resolve_recorded_optical_setup(metadata, &OpticalProfile::StrictAuto),
                OpticalEvidence::RecordedState,
            )
        } else {
            let selection = parsed.and_then(|calibration| {
                let camera = metadata
                    .camera_name
                    .as_deref()
                    .and_then(camera_profile_for_name)
                    .or_else(|| infer_camera_profile(&calibration));
                encoded_lens_id
                    .and_then(|id| resolved_optical_profile(camera, id))
                    .ok_or_else(|| {
                        Error::MissingCalibration(
                            "encoded lens IDs do not establish an unambiguous optical selection"
                                .into(),
                        )
                    })
            });
            (selection, OpticalEvidence::EncodedLens)
        };
        match selection {
            Ok(profile) => OpticalInspection {
                detected: Some(profile.selection()),
                evidence: Some(evidence),
                encoded_lens_id,
                ambiguity: None,
            },
            Err(error) => OpticalInspection {
                detected: None,
                evidence: Some(evidence),
                encoded_lens_id,
                ambiguity: Some(error.to_string()),
            },
        }
    }

    /// Resolves a calibration directly from parsed INSV metadata.
    ///
    /// `source` is strict: selecting [`OffsetSource::Current`] never falls back
    /// to an original record, and selecting [`OffsetSource::Original`] never
    /// falls back to a current record. A V6 X5 offset can be converted when the
    /// metadata contains both its encoded and requested six-coefficient optical
    /// profiles.
    pub fn resolve_metadata(
        &self,
        metadata: &InsvMetadata,
        optical_selection: &OpticalSelection,
        source: OffsetSource,
    ) -> Result<ResolvedCalibration> {
        let declared_camera = metadata
            .camera_name
            .as_deref()
            .map(|camera_name| {
                camera_profile_for_name(camera_name)
                    .ok_or_else(|| Error::UnsupportedCamera(camera_name.into()))
            })
            .transpose()?;

        let candidates: Vec<CalibrationCandidate> = metadata
            .offsets
            .iter()
            .filter(|offset| source.matches(offset))
            .map(|offset| CalibrationCandidate::new(offset.version, None, &offset.value))
            .collect();
        if candidates.is_empty() {
            return Err(Error::MissingCalibration(format!(
                "no {} offset records were found",
                source.label()
            )));
        }

        let mut calibration = self.resolve_unprofiled(&candidates, source)?;
        let camera = declared_camera.or_else(|| infer_camera_profile(&calibration));
        validate_camera_generation(camera, calibration.offset_version)?;
        let crop_applied = normalize_sensor_crop(&mut calibration, metadata.crop_window.as_ref())?;
        let (resolved_setup, mut optical_resolution) =
            resolve_optical_selection(Some(metadata), *optical_selection, camera, &calibration)?;
        if camera.is_some_and(|profile| profile.camera == CameraModel::X5) {
            apply_x5_setup(&mut calibration, &resolved_setup, &metadata.profiles)?;
        } else {
            apply_registered_setup(&mut calibration, camera, &resolved_setup)?;
        }
        optical_resolution.sensor_crop_applied = crop_applied;
        optical_resolution.target_lens_id = calibration.lenses[0].lens_type;
        calibration.optical_resolution = Some(optical_resolution);
        calibration.camera_model = camera.map(|profile| profile.camera.clone());
        populate_render_geometry(&mut calibration, camera, metadata.blend_angle)?;
        calibration.validate()?;
        Ok(calibration)
    }

    /// Resolves one embedded offset without substituting another metadata copy.
    ///
    /// This is useful to inspect or validate a caller-selected record. The
    /// encoded lens IDs establish optical detection. Registered conversions
    /// using native curves are available; conversions requiring separate
    /// metadata profile descriptors or an ambiguous camera identity fail.
    pub fn resolve_embedded_offset(
        &self,
        offset: &EmbeddedOffset,
        optical_selection: &OpticalSelection,
    ) -> Result<ResolvedCalibration> {
        let source = if offset.original {
            OffsetSource::Original
        } else {
            OffsetSource::Current
        };
        let candidate = CalibrationCandidate::new(offset.version, None, &offset.value);
        let mut calibration = parse_offset(&candidate, source)?;
        let camera = infer_camera_profile(&calibration);
        let (optical_setup, mut resolution) =
            resolve_optical_selection(None, *optical_selection, camera, &calibration)?;
        if camera.is_some_and(|profile| profile.camera == CameraModel::X5) {
            apply_x5_setup(&mut calibration, &optical_setup, &[])?;
        } else {
            apply_registered_setup(&mut calibration, camera, &optical_setup)?;
        }
        resolution.target_lens_id = calibration.lenses[0].lens_type;
        calibration.optical_resolution = Some(resolution);
        calibration.camera_model = camera.map(|profile| profile.camera.clone());
        populate_render_geometry(&mut calibration, camera, None)?;
        calibration.validate()?;
        Ok(calibration)
    }

    /// Resolves one optical setup to its newest usable two-lens calibration.
    pub fn resolve(
        &self,
        candidates: &[CalibrationCandidate],
        optical_selection: &OpticalSelection,
    ) -> Result<ResolvedCalibration> {
        if candidates.is_empty() {
            return Err(Error::MissingCalibration(
                "no offset records were found".into(),
            ));
        }

        let optical_setup = if optical_selection.housing == crate::Housing::Auto
            || optical_selection.environment == crate::Environment::Auto
            || optical_selection.lens_accessory == crate::LensAccessory::Auto
        {
            // The candidate collection must first identify one profile. Its
            // encoded lens identity then fills the caller's Auto components.
            OpticalProfile::StrictAuto
        } else {
            // Mounting does not select a lens profile. Resolve its Auto value
            // and validate it with encoded evidence in the candidate loop.
            OpticalProfile::from_selection(*optical_selection)?
        };
        let selected_profile = select_profile(candidates, &optical_setup)?;
        let mut matching: Vec<&CalibrationCandidate> = candidates
            .iter()
            .filter(|candidate| profile_matches(candidate, selected_profile.as_deref()))
            .collect();

        match self.policy {
            CalibrationPolicy::PreferNewest => matching
                .sort_by_key(|candidate| Reverse(version_priority(candidate.version).unwrap_or(0))),
        }

        let mut errors = Vec::new();
        for candidate in matching {
            if version_priority(candidate.version).is_none() {
                continue;
            }
            match parse_offset(candidate, OffsetSource::Current).and_then(|mut calibration| {
                let camera = infer_camera_profile(&calibration);
                let (effective, mut resolution) =
                    resolve_optical_selection(None, *optical_selection, camera, &calibration)?;
                apply_registered_setup(&mut calibration, camera, &effective)?;
                resolution.target_lens_id = calibration.lenses[0].lens_type;
                calibration.optical_resolution = Some(resolution);
                calibration.camera_model = camera.map(|profile| profile.camera.clone());
                populate_render_geometry(&mut calibration, camera, None)?;
                calibration.validate()?;
                Ok(calibration)
            }) {
                Ok(calibration) => return Ok(calibration),
                Err(error) => errors.push(format!("V{}: {error}", candidate.version)),
            }
        }

        let detail = if errors.is_empty() {
            "no supported offset version exists for the selected profile".into()
        } else {
            errors.join("; ")
        };
        Err(Error::MissingCalibration(detail))
    }

    fn resolve_unprofiled(
        &self,
        candidates: &[CalibrationCandidate],
        source: OffsetSource,
    ) -> Result<ResolvedCalibration> {
        let mut matching: Vec<&CalibrationCandidate> = candidates.iter().collect();
        match self.policy {
            CalibrationPolicy::PreferNewest => matching
                .sort_by_key(|candidate| Reverse(version_priority(candidate.version).unwrap_or(0))),
        }

        let mut errors = Vec::new();
        for candidate in matching {
            if version_priority(candidate.version).is_none() {
                continue;
            }
            match parse_offset(candidate, source) {
                Ok(calibration) => return Ok(calibration),
                Err(error) => errors.push(format!("V{}: {error}", candidate.version)),
            }
        }

        let detail = if errors.is_empty() {
            format!("no supported {} offset version was found", source.label())
        } else {
            errors.join("; ")
        };
        Err(Error::MissingCalibration(detail))
    }
}

// INSOffsetCalculator crop bridge0x3cce1c negates recorded offsets before
// ins::OffsetConvert::convertOffset0x1e38d08. Its per-lens loop0x1e38f34
// scales pixel centers (c+0.5)*scale-0.5, subtracts the centered crop and
// recorded offsets, and scales focal/radius. The output here remains a
// side-by-side calibration canvas; source images are rescaled by renderers.
fn normalize_sensor_crop(
    calibration: &mut ResolvedCalibration,
    crop: Option<&crate::container::CropWindow>,
) -> Result<bool> {
    let Some(crop) = crop else {
        return Ok(false);
    };
    let source = (crop.source_width, crop.source_height);
    let destination = (crop.destination_width, crop.destination_height);
    if source.0 == 0 || source.1 == 0 || destination.0 == 0 || destination.1 == 0 {
        return Err(Error::MissingCalibration(
            "sensor crop dimensions must be nonzero".into(),
        ));
    }
    if source == destination && crop.x_offset == 0 && crop.y_offset == 0 {
        return Ok(false);
    }
    if !calibration.canvas_width.is_multiple_of(2) {
        return Err(Error::MissingCalibration(
            "sensor crop requires a side-by-side calibration canvas".into(),
        ));
    }
    let width = calibration.canvas_width / 2;
    let height = calibration.canvas_height;
    // A current offset can already describe the encoded crop. Do not crop it twice.
    if (width, height) == destination && source != destination {
        return Ok(false);
    }
    if u64::from(width) * u64::from(source.1) != u64::from(height) * u64::from(source.0) {
        return Err(Error::MissingCalibration(
            "sensor crop source aspect ratio disagrees with the calibration canvas".into(),
        ));
    }
    if calibration.offset_version == 1 {
        return Err(Error::MissingCapability(
            "V1 sensor crop conversion is not supported by the inspected native converter".into(),
        ));
    }
    let canvas_width = destination
        .0
        .checked_mul(2)
        .ok_or_else(|| Error::MissingCalibration("sensor crop canvas width overflows".into()))?;
    let scale = f64::from(source.0) / f64::from(width);
    let left = (f64::from(source.0) - f64::from(destination.0)) * 0.5 + f64::from(crop.x_offset);
    let top = (f64::from(source.1) - f64::from(destination.1)) * 0.5 + f64::from(crop.y_offset);
    for (index, lens) in calibration.lenses.iter_mut().enumerate() {
        let local_cx = lens.cx - index as f64 * f64::from(width);
        lens.cx = (local_cx + 0.5) * scale - 0.5 - left + index as f64 * f64::from(destination.0);
        lens.cy = (lens.cy + 0.5) * scale - 0.5 - top;
        lens.fx *= scale;
        lens.fy *= scale;
        lens.radius = lens.radius.map(|radius| radius * scale);
        lens.canvas_width = canvas_width;
        lens.canvas_height = destination.1;
    }
    calibration.canvas_width = canvas_width;
    calibration.canvas_height = destination.1;
    calibration.validate()?;
    Ok(true)
}

fn resolve_optical_selection(
    metadata: Option<&InsvMetadata>,
    requested: OpticalSelection,
    camera: Option<&CameraProfile>,
    calibration: &ResolvedCalibration,
) -> Result<(OpticalProfile, OpticalResolution)> {
    let encoded = resolved_optical_profile(camera, calibration.lenses[0].lens_type);
    let recorded = metadata
        .filter(|metadata| metadata.offset_state.is_some())
        .map(|metadata| resolve_recorded_optical_setup(metadata, &OpticalProfile::StrictAuto));
    let evidence = if recorded.is_some() {
        OpticalEvidence::RecordedState
    } else {
        OpticalEvidence::EncodedLens
    };
    let fully_explicit = requested.housing != crate::Housing::Auto
        && requested.environment != crate::Environment::Auto
        && requested.lens_accessory != crate::LensAccessory::Auto;
    let detected = match recorded {
        Some(Ok(profile)) => profile.selection(),
        Some(Err(_)) if fully_explicit => OpticalSelection::default(),
        Some(Err(error)) => return Err(error),
        None => encoded.map(OpticalProfile::selection).unwrap_or_default(),
    };
    let effective = merge_selection(requested, detected)?;
    let profile = OpticalProfile::from_selection(effective)?;
    Ok((
        profile,
        OpticalResolution {
            requested,
            detected,
            effective,
            evidence,
            sensor_crop_applied: false,
            source_lens_id: calibration.lenses[0].lens_type,
            target_lens_id: calibration.lenses[0].lens_type,
        },
    ))
}

fn resolve_recorded_optical_setup(
    metadata: &InsvMetadata,
    requested: &OpticalProfile,
) -> Result<OpticalProfile> {
    if !matches!(requested, OpticalProfile::StrictAuto) {
        return Ok(*requested);
    }

    let Some(state) = metadata.offset_state else {
        return Ok(OpticalProfile::StrictAuto);
    };
    match state {
        OffsetState::Common => Ok(OpticalProfile::BareAir),
        OffsetState::SphereProtector => Ok(OpticalProfile::AdhesiveSphereLensGuard),
        OffsetState::DiveCaseUnderwater => Ok(OpticalProfile::DiveCaseUnderwater),
        OffsetState::DiveCase2023Underwater => Ok(OpticalProfile::InvisibleDiveCaseUnderwater),
        OffsetState::DiveCaseProUnderwater => Ok(OpticalProfile::DiveCaseProUnderwater),
        OffsetState::X4PlasticLensGuard => Ok(OpticalProfile::ProtectorA),
        OffsetState::X4GlassLensGuard => Ok(OpticalProfile::ProtectorS),
        OffsetState::Automatic => resolve_detected_guard(metadata.guard_detected_type),
        OffsetState::Nd16 => Ok(OpticalProfile::Nd16),
        OffsetState::Nd32 => Ok(OpticalProfile::Nd32),
        OffsetState::Nd64 => Ok(OpticalProfile::Nd64),
        OffsetState::DiveCaseProAboveWater => Ok(OpticalProfile::DiveCaseProAir),
        OffsetState::Nd128 => Ok(OpticalProfile::Nd128),
        OffsetState::Other(value) => Err(Error::MissingCalibration(format!(
            "recorded offset state {value} is not supported by this crate"
        ))),
    }
}

fn resolve_detected_guard(detected: Option<GuardDetectedType>) -> Result<OpticalProfile> {
    match detected {
        Some(GuardDetectedType::Plastic) => Ok(OpticalProfile::ProtectorA),
        Some(GuardDetectedType::Glass) => Ok(OpticalProfile::ProtectorS),
        Some(GuardDetectedType::Off) => Ok(OpticalProfile::BareAir),
        Some(GuardDetectedType::AveragePlasticGlass) => Ok(OpticalProfile::ProtectorAS),
        Some(GuardDetectedType::Nd16) => Ok(OpticalProfile::Nd16),
        Some(GuardDetectedType::Nd32) => Ok(OpticalProfile::Nd32),
        Some(GuardDetectedType::Nd64) => Ok(OpticalProfile::Nd64),
        Some(GuardDetectedType::Nd128) => Ok(OpticalProfile::Nd128),
        Some(GuardDetectedType::Unknown) | None => Err(Error::MissingCalibration(
            "offset state requests automatic accessory detection, but metadata has no conclusive guard result"
                .into(),
        )),
        Some(GuardDetectedType::Other(value)) => Err(Error::MissingCalibration(format!(
            "automatic accessory result {value} is not supported by this crate"
        ))),
    }
}

impl OffsetSource {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Original => "original",
        }
    }
}

fn select_profile(
    candidates: &[CalibrationCandidate],
    setup: &OpticalProfile,
) -> Result<Option<String>> {
    if let Some(requested) = setup.profile_name() {
        let requested_is_bare = matches!(setup, OpticalProfile::BareAir);
        let available = candidates.iter().any(|candidate| {
            candidate
                .profile_name
                .as_deref()
                .is_some_and(|profile| profile.eq_ignore_ascii_case(requested))
                || (requested_is_bare && candidate.profile_name.is_none())
        });
        if !available {
            return Err(Error::MissingCalibration(format!(
                "the recording does not contain the requested {requested} profile"
            )));
        }
        return Ok(
            if requested_is_bare
                && !candidates.iter().any(|candidate| {
                    candidate
                        .profile_name
                        .as_deref()
                        .is_some_and(|profile| profile.eq_ignore_ascii_case(requested))
                })
            {
                None
            } else {
                Some(requested.into())
            },
        );
    }

    let profiles: BTreeSet<Option<String>> = candidates
        .iter()
        .map(|candidate| {
            candidate
                .profile_name
                .as_deref()
                .filter(|profile| !profile.trim().is_empty())
                .map(str::to_ascii_lowercase)
        })
        .collect();
    if profiles.len() == 1 {
        return Ok(profiles.into_iter().next().flatten());
    }

    Err(Error::AmbiguousOpticalSetup {
        candidates: profiles
            .into_iter()
            .map(|profile| profile.unwrap_or_else(|| "bare/unspecified".into()))
            .collect(),
    })
}

fn profile_matches(candidate: &CalibrationCandidate, selected: Option<&str>) -> bool {
    match (candidate.profile_name.as_deref(), selected) {
        (None, None) => true,
        (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn version_priority(version: u8) -> Option<u8> {
    SUPPORTED_VERSIONS
        .iter()
        .position(|candidate| *candidate == version)
        .map(|position| (SUPPORTED_VERSIONS.len() - position) as u8)
}

pub(crate) fn parse_offset(
    candidate: &CalibrationCandidate,
    offset_source: OffsetSource,
) -> Result<ResolvedCalibration> {
    let input = candidate
        .offset
        .trim_matches(|character: char| character.is_whitespace() || character == '\0');
    if input.is_empty() || input.len() > MAX_OFFSET_BYTES {
        return Err(Error::MissingCalibration(
            "offset string is empty or exceeds the safety limit".into(),
        ));
    }

    let tokens: Vec<&str> = input.split('_').collect();
    let lens_count = tokens
        .first()
        .ok_or_else(|| Error::MissingCalibration("offset string has no lens count".into()))?
        .parse::<usize>()
        .map_err(|_| Error::MissingCalibration("offset lens count is not an integer".into()))?;
    if lens_count == 0 || lens_count > MAX_LENSES {
        return Err(Error::MissingCalibration(format!(
            "offset lens count {lens_count} is outside the supported range"
        )));
    }
    if lens_count != 2 {
        return Err(Error::MissingCalibration(format!(
            "X5 stitching requires two lenses, found {lens_count}"
        )));
    }

    let expected = field_count(candidate.version, lens_count)?;
    if tokens.len() != expected {
        return Err(Error::MissingCalibration(format!(
            "V{} offset expected {expected} fields, found {}",
            candidate.version,
            tokens.len()
        )));
    }

    let numbers: Vec<f64> = tokens[1..]
        .iter()
        .enumerate()
        .map(|(index, token)| {
            token.parse::<f64>().map_err(|_| {
                Error::MissingCalibration(format!("offset field {} is not a number", index + 1))
            })
        })
        .collect::<Result<_>>()?;
    if !numbers.iter().all(|number| number.is_finite()) {
        return Err(Error::MissingCalibration(
            "offset contains a non-finite number".into(),
        ));
    }

    let (first, second, offset_flags) = parse_lenses(candidate.version, &numbers)?;
    if first.canvas_width != second.canvas_width || first.canvas_height != second.canvas_height {
        return Err(Error::MissingCalibration(
            "the two lens records use different canvas dimensions".into(),
        ));
    }

    let calibration = ResolvedCalibration {
        camera_model: None,
        optical_resolution: None,
        offset_version: candidate.version,
        offset_source,
        offset_flags,
        profile_name: candidate.profile_name.clone(),
        canvas_width: first.canvas_width,
        canvas_height: first.canvas_height,
        lenses: [first, second],
        lens_geometry: [None, None],
        raw_offset: input.into(),
    };
    calibration.validate()?;
    Ok(calibration)
}

fn field_count(version: u8, lens_count: usize) -> Result<usize> {
    let count = match version {
        // V1 stores six fields per lens, followed by global width, height and
        // a packed lens-type/flags word.
        1 => 1 + lens_count * 6 + 3,
        // V2+ store dimensions and lens type inside each record, followed by a
        // packed flags/version word for the whole offset.
        2 => 1 + lens_count * 16 + 1,
        3 => 1 + lens_count * 19 + 1,
        6 => 1 + lens_count * 27 + 1,
        _ => Err(Error::MissingCalibration(format!(
            "offset version {version} is unsupported"
        )))?,
    };
    Ok(count)
}

fn parse_lenses(version: u8, values: &[f64]) -> Result<(ParsedLens, ParsedLens, u32)> {
    match version {
        1 => {
            let canvas_width = positive_integer(values[12], "canvas width")?;
            let canvas_height = positive_integer(values[13], "canvas height")?;
            let packed = non_negative_integer(values[14], "packed V1 lens information")?;
            // Lens identifiers in the vendor implementation occupy the low ten bits; bit 10 is
            // present in every observed valid offset as the common flags word.
            let lens_type = packed & 0x03ff;
            let flags = packed & !0x03ff;
            let first = parse_v1_lens(&values[..6], canvas_width, canvas_height, lens_type)?;
            let mut second = parse_v1_lens(&values[6..12], canvas_width, canvas_height, lens_type)?;
            apply_dual_lens_back_rotation(&mut second)?;
            Ok((first, second, flags))
        }
        2 => parse_record_lenses(version, values, 16, parse_v2_lens),
        3 => parse_record_lenses(version, values, 19, parse_v3_lens),
        6 => parse_record_lenses(version, values, 27, parse_v6_lens),
        _ => unreachable!("field count rejects unsupported versions"),
    }
}

fn parse_record_lenses(
    version: u8,
    values: &[f64],
    record_len: usize,
    parse: fn(&[f64]) -> Result<ParsedLens>,
) -> Result<(ParsedLens, ParsedLens, u32)> {
    let first = parse(&values[..record_len])?;
    let mut second = parse(&values[record_len..record_len * 2])?;
    apply_dual_lens_back_rotation(&mut second)?;
    let packed = non_negative_integer(values[record_len * 2], "packed offset flags")?;
    let encoded_version = packed >> 16;
    if encoded_version != u32::from(version) {
        return Err(Error::MissingCalibration(format!(
            "V{version} trailing word encodes offset version {encoded_version}"
        )));
    }
    Ok((first, second, packed & 0xffff))
}

fn parse_v1_lens(
    values: &[f64],
    canvas_width: u32,
    canvas_height: u32,
    lens_type: u32,
) -> Result<ParsedLens> {
    make_lens(
        LensProjectionModel::PinholePolynomialV1,
        Some(values[0]),
        None,
        values[0],
        values[0],
        values[1],
        values[2],
        &values[3..6],
        [0.0; 3],
        &[],
        canvas_width,
        canvas_height,
        lens_type,
    )
}

fn parse_v2_lens(values: &[f64]) -> Result<ParsedLens> {
    make_lens(
        LensProjectionModel::PinholePolynomialV2,
        Some(values[0]),
        None,
        values[0],
        values[0],
        values[1],
        values[2],
        &values[3..6],
        [values[6], values[7], values[8]],
        &values[9..13],
        positive_integer(values[13], "canvas width")?,
        positive_integer(values[14], "canvas height")?,
        non_negative_integer(values[15], "lens type")?,
    )
}

fn parse_v3_lens(values: &[f64]) -> Result<ParsedLens> {
    make_lens(
        LensProjectionModel::OmniRadtan,
        None,
        Some(values[0]),
        values[1],
        values[2],
        values[3],
        values[4],
        &values[5..8],
        [values[8], values[9], values[10]],
        &values[11..16],
        positive_integer(values[16], "canvas width")?,
        positive_integer(values[17], "canvas height")?,
        non_negative_integer(values[18], "lens type")?,
    )
}

fn parse_v6_lens(values: &[f64]) -> Result<ParsedLens> {
    make_lens(
        LensProjectionModel::OmniRadtanPro,
        None,
        Some(values[0]),
        values[1],
        values[2],
        values[3],
        values[4],
        &values[5..8],
        [values[8], values[9], values[10]],
        &values[11..24],
        positive_integer(values[24], "canvas width")?,
        positive_integer(values[25], "canvas height")?,
        non_negative_integer(values[26], "lens type")?,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_lens(
    model: LensProjectionModel,
    radius: Option<f64>,
    xi: Option<f64>,
    fx: f64,
    fy: f64,
    cx: f64,
    cy: f64,
    euler: &[f64],
    translation: [f64; 3],
    distortion: &[f64],
    canvas_width: u32,
    canvas_height: u32,
    lens_type: u32,
) -> Result<ParsedLens> {
    let euler_degrees = [euler[0], euler[1], euler[2]];
    // INSCoreMedia's inlined OffsetExtrinsicToQuaternion implementation adds
    // +pi/2 to the second encoded Euler value before composing Z * Y * X.
    let rotation = Orientation::from_euler_degrees(
        euler_degrees[0],
        euler_degrees[1] + 90.0,
        euler_degrees[2],
    )?;

    let mut lens = ParsedLens {
        model,
        radius,
        xi,
        cx,
        cy,
        fx,
        fy,
        k1: distortion.first().copied().unwrap_or(0.0),
        k2: distortion.get(1).copied().unwrap_or(0.0),
        k3: distortion.get(2).copied().unwrap_or(0.0),
        distortion_coefficients: distortion.to_vec(),
        polynomial_projection: None,
        euler_degrees,
        orientation: rotation,
        translation,
        canvas_width,
        canvas_height,
        lens_type,
    };
    // Retain inspectable native records whose projection is singular or
    // non-positive; rendering rejects them with the normalization error.
    lens.polynomial_projection = polynomial::normalized(&lens).ok().flatten();
    Ok(lens)
}

fn apply_dual_lens_back_rotation(lens: &mut ParsedLens) -> Result<()> {
    // X5's second physical lens faces the opposite body direction. The offset
    // Euler values are per-module corrections and remain close for both lens
    // records; the dual-lens arrangement supplies the half-turn separately.
    let back = Orientation::from_axis_angle([0.0, 1.0, 0.0], PI)?;
    lens.orientation = back * lens.orientation;
    Ok(())
}

fn infer_camera_profile(calibration: &ResolvedCalibration) -> Option<&'static CameraProfile> {
    let mut matches = camera_profiles().iter().filter(|profile| {
        calibration
            .lenses
            .iter()
            .all(|lens| profile.lens(lens.lens_type).is_some())
    });
    let profile = matches.next()?;
    matches.next().is_none().then_some(profile)
}

fn validate_camera_generation(camera: Option<&CameraProfile>, offset_version: u8) -> Result<()> {
    let Some(camera) = camera else {
        return Ok(());
    };
    let generation =
        ProjectionGeneration::from_offset_version(offset_version).ok_or_else(|| {
            Error::MissingCalibration(format!(
                "offset version {offset_version} has no portable projection generation"
            ))
        })?;
    if !camera.accepts_projection_generation(generation) {
        return Err(Error::MissingCalibration(format!(
            "{} does not accept V{offset_version} offsets",
            camera.canonical_name
        )));
    }
    Ok(())
}

fn consensus_lens_profile(lens_type: u32) -> Option<&'static LensProfile> {
    let mut matches = lens_profiles_for_id(lens_type).map(|(_, lens)| lens);
    let first = matches.next()?;
    matches
        .all(|candidate| {
            candidate.optical_profile == first.optical_profile
                && candidate.fallback == first.fallback
                && candidate.mask_recipe == first.mask_recipe
        })
        .then_some(first)
}

// Accessory inspection needs agreement on the optical selection only. Shared
// lens IDs may still require a known camera before resolving FOV or mask data.
fn resolved_optical_profile(
    camera: Option<&CameraProfile>,
    lens_type: u32,
) -> Option<OpticalProfile> {
    if let Some(camera) = camera {
        return camera.lens(lens_type).map(|lens| lens.optical_profile);
    }
    let mut matches = lens_profiles_for_id(lens_type).map(|(_, lens)| lens.optical_profile);
    let first = matches.next()?;
    matches.all(|candidate| candidate == first).then_some(first)
}

fn resolved_lens_profile(
    camera: Option<&CameraProfile>,
    lens_type: u32,
) -> Option<&'static LensProfile> {
    camera
        .and_then(|profile| profile.lens(lens_type))
        .or_else(|| {
            camera
                .is_none()
                .then(|| consensus_lens_profile(lens_type))
                .flatten()
        })
}

fn apply_registered_setup(
    calibration: &mut ResolvedCalibration,
    camera: Option<&CameraProfile>,
    requested: &OpticalProfile,
) -> Result<()> {
    let [first, second] = &calibration.lenses;
    let first_type = first.lens_type;
    if first.lens_type != second.lens_type {
        return Err(Error::MissingCalibration(format!(
            "lens records disagree on lens type ({} and {})",
            first.lens_type, second.lens_type
        )));
    }
    let profile = resolved_lens_profile(camera, first.lens_type).ok_or_else(|| {
        let camera_name = camera
            .map(|profile| profile.canonical_name)
            .unwrap_or("the unresolved camera");
        Error::MissingCalibration(format!(
            "{camera_name} lens type {} has no evidence-backed optical-setup mapping",
            first.lens_type
        ))
    })?;

    if matches!(requested, OpticalProfile::StrictAuto) || *requested == profile.optical_profile {
        calibration.profile_name = profile.optical_profile.profile_name().map(str::to_owned);
        return Ok(());
    }

    if housing_conversion::apply(calibration, camera, requested)? {
        return Ok(());
    }

    Err(Error::MissingCalibration(format!(
        "the recorded lens type {} represents {:?}, not {:?}; portable conversion is not available for this camera/setup pair",
        first_type, profile.optical_profile, requested
    )))
}

fn populate_render_geometry(
    calibration: &mut ResolvedCalibration,
    camera: Option<&CameraProfile>,
    recorded_blend_angle: Option<i32>,
) -> Result<()> {
    let recorded_blend_angle = match recorded_blend_angle {
        None | Some(0) => None,
        Some(value) if (180..=360).contains(&value) => Some(f64::from(value)),
        Some(value) => {
            return Err(Error::MissingCalibration(format!(
                "recorded blend angle {value} is outside 180..=360 degrees"
            )));
        }
    };

    for (index, lens) in calibration.lenses.iter().enumerate() {
        if lens.lens_type == 0 {
            calibration.lens_geometry[index] = None;
            continue;
        }
        let profile = resolved_lens_profile(camera, lens.lens_type).ok_or_else(|| {
            let camera_name = camera
                .map(|profile| profile.canonical_name)
                .unwrap_or("unresolved camera");
            Error::MissingCalibration(format!(
                "{camera_name} lens type {} has no evidence-backed FOV/blend geometry",
                lens.lens_type
            ))
        })?;
        calibration.lens_geometry[index] = Some(ResolvedLensGeometry {
            full_fov_degrees: profile.fallback.full_fov_degrees,
            blend_angle_degrees: recorded_blend_angle.or(profile.fallback.blend_angle_degrees).ok_or_else(|| Error::MissingCalibration(format!("lens type {} requires recorded blend-angle tag128; no verified native default is available", lens.lens_type)))?,
            blend_angle_recorded: recorded_blend_angle.is_some(),
        });
    }
    Ok(())
}

fn apply_x5_setup(
    calibration: &mut ResolvedCalibration,
    requested: &OpticalProfile,
    profiles: &[EmbeddedProfile],
) -> Result<()> {
    let first_type = calibration.lenses[0].lens_type;
    let second_type = calibration.lenses[1].lens_type;
    if first_type != second_type {
        return Err(Error::MissingCalibration(format!(
            "X5 lens records disagree on lens type ({first_type} and {second_type})"
        )));
    }

    let encoded_setup = x5_setup_for_lens_type(first_type);
    if matches!(requested, OpticalProfile::StrictAuto) {
        let setup = encoded_setup.ok_or_else(|| {
            Error::MissingCalibration(format!(
                "X5 lens type {first_type} has no evidence-backed optical-setup mapping"
            ))
        })?;
        calibration.profile_name = setup.profile_name().map(str::to_owned);
        return Ok(());
    }

    if encoded_setup.as_ref() == Some(requested) {
        calibration.profile_name = requested.profile_name().map(str::to_owned);
        return Ok(());
    }

    let requested_name = requested.profile_name().unwrap_or("unknown");
    let encoded_name = encoded_setup
        .as_ref()
        .and_then(OpticalProfile::profile_name)
        .unwrap_or("an unmapped setup");
    if calibration.offset_version != 6
        || calibration
            .lenses
            .iter()
            .any(|lens| lens.model != LensProjectionModel::OmniRadtanPro)
    {
        return Err(Error::MissingCalibration(format!(
            "selected {} V{} offset encodes X5 lens type {first_type} ({encoded_name}), not {requested_name}; portable optical-profile conversion requires a V6 offset",
            calibration.offset_source.label(),
            calibration.offset_version,
        )));
    }

    let source_fov = x5_fov_for_lens_type(first_type).ok_or_else(|| {
        Error::MissingCalibration(format!(
            "X5 lens type {first_type} has no evidence-backed field of view for profile conversion"
        ))
    })?;
    let (target_type, target_fov) = x5_conversion_target(requested).ok_or_else(|| {
        Error::MissingCalibration(format!(
            "portable X5 V6 conversion to {requested_name} is not evidence-backed"
        ))
    })?;
    // Generic InvisibleDiveWater/Air names do not identify a housing revision.
    // Native Pro conversion selects its exact lens curve by ID, never by those names.
    let pro_conversion = matches!(first_type, 119 | 120) || matches!(target_type, 119 | 120);
    let curve = |id| -> Result<[f64; PROFILE_COEFFICIENT_COUNT]> {
        let curve = crate::profile::physical_curve(id).ok_or_else(|| {
            Error::MissingCalibration(format!("lens {id} has no verified physical curve"))
        })?;
        let mut coefficients = [0.0; PROFILE_COEFFICIENT_COUNT];
        coefficients[..5].copy_from_slice(&curve.coefficients);
        Ok(coefficients)
    };
    let source_profile = if pro_conversion {
        curve(first_type)?
    } else {
        find_six_coefficient_profile(profiles, encoded_name)?
    };
    let target_profile = if pro_conversion {
        curve(target_type)?
    } else {
        find_six_coefficient_profile(profiles, requested_name)?
    };

    convert_x5_v6_profile(
        calibration,
        target_type,
        source_fov,
        target_fov,
        source_profile,
        target_profile,
    )?;
    calibration.profile_name = Some(requested_name.to_owned());
    calibration.raw_offset = encode_v6_offset(calibration);
    calibration.validate()
}

fn find_six_coefficient_profile(
    profiles: &[EmbeddedProfile],
    name: &str,
) -> Result<[f64; PROFILE_COEFFICIENT_COUNT]> {
    let profile = profiles
        .iter()
        .find(|profile| profile.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            Error::MissingCalibration(format!(
                "the metadata does not contain the {name} optical profile required for V6 conversion"
            ))
        })?;
    match ParsedEmbeddedProfile::parse(profile)?.payload {
        EmbeddedProfilePayload::SixCoefficientTransform(coefficients) => Ok(coefficients),
        EmbeddedProfilePayload::ClassificationValue(_) => Err(Error::MissingCalibration(format!(
            "the metadata profile {name} is a classifier hint, not an optical transform"
        ))),
    }
}

fn x5_conversion_target(setup: &OpticalProfile) -> Option<(u32, f64)> {
    let lens_type = match setup {
        OpticalProfile::BareAir => X5_BARE_LENS_TYPE,
        OpticalProfile::InvisibleDiveCaseUnderwater => X5_DIVING_WATER_LENS_TYPE,
        OpticalProfile::InvisibleDiveCaseAir => X5_DIVING_AIR_LENS_TYPE,
        OpticalProfile::DiveCaseProUnderwater => 119,
        OpticalProfile::DiveCaseProAir => 120,
        _ => return None,
    };
    let fov = lens_profile(&CameraModel::X5, lens_type)?
        .fallback
        .full_fov_degrees;
    Some((lens_type, fov))
}

fn x5_fov_for_lens_type(lens_type: u32) -> Option<f64> {
    lens_profile(&CameraModel::X5, lens_type).map(|profile| profile.fallback.full_fov_degrees)
}

fn convert_x5_v6_profile(
    calibration: &mut ResolvedCalibration,
    target_lens_type: u32,
    source_fov_degrees: f64,
    target_fov_degrees: f64,
    source_profile: [f64; PROFILE_COEFFICIENT_COUNT],
    target_profile: [f64; PROFILE_COEFFICIENT_COUNT],
) -> Result<()> {
    // INSCoreMedia's arm64 implementation provides the otherwise unpublished
    // portable conversion in these symbols:
    //
    // * ins::OffsetConvert::getPhysical2PixelScale
    // * ins::OffsetConvert::getV6DistortAndFocalFromLens
    // * ins::OffsetConvert::converOffsetNormal
    //
    // It samples angles every 0.1 degree. The target curve is least-squares
    // fitted to [u, u^3, u^5, u^7, u^9], where
    // u=sin(theta)/(cos(theta)+xi). The original curve and V6 radial model
    // establish pixels per physical-radius unit. Selectors54..57 map117..120
    // through table0x529e4d8 into the V6 branch0x1e2e0b4. Its target fit
    // uses the first lens xi once at0x1e2e18c..1a8, then stores that xi for
    // both lenses at0x1e2e34c..358. Each source scale still uses its own xi.
    // Tangential/thin-prism slots5..12, principal points and extrinsics survive.
    let target_xi = calibration.lenses[0]
        .xi
        .ok_or_else(|| Error::MissingCalibration("X5 V6 profile conversion requires xi".into()))?;
    let (target_physical_focal, target_radial) =
        fit_v6_radial_profile(target_xi, target_profile, target_fov_degrees)?;
    for lens in &mut calibration.lenses {
        let xi = lens.xi.ok_or_else(|| {
            Error::MissingCalibration("X5 V6 profile conversion requires xi".into())
        })?;
        if lens.distortion_coefficients.len() != 13 {
            return Err(Error::MissingCalibration(
                "X5 V6 profile conversion requires thirteen distortion coefficients".into(),
            ));
        }
        let source_focal = (lens.fx * lens.fy).sqrt();
        let scale = physical_to_pixel_scale(
            xi,
            source_focal,
            &lens.distortion_coefficients[..5],
            source_profile,
            source_fov_degrees,
        )?;
        let target_pixel_focal = scale * target_physical_focal;
        if !target_pixel_focal.is_finite() || target_pixel_focal <= 0.0 {
            return Err(Error::MissingCalibration(
                "converted X5 V6 focal length is not positive and finite".into(),
            ));
        }

        lens.xi = Some(target_xi);
        lens.fx = target_pixel_focal;
        lens.fy = target_pixel_focal;
        lens.distortion_coefficients[..5].copy_from_slice(&target_radial);
        lens.k1 = target_radial[0];
        lens.k2 = target_radial[1];
        lens.k3 = target_radial[2];
        lens.lens_type = target_lens_type;
    }
    Ok(())
}

const PROFILE_SAMPLE_STEP_DEGREES: f64 = 0.1;

fn physical_to_pixel_scale<const N: usize>(
    xi: f64,
    focal: f64,
    radial: &[f64],
    profile: [f64; N],
    fov_degrees: f64,
) -> Result<f64> {
    let sample_count = ((fov_degrees * 0.5) / PROFILE_SAMPLE_STEP_DEGREES).ceil() as usize;
    let mut physical_squared = 0.0;
    let mut physical_pixel = 0.0;
    for sample in 0..sample_count {
        let theta_degrees = sample as f64 * PROFILE_SAMPLE_STEP_DEGREES;
        let theta = theta_degrees.to_radians();
        let u = theta.sin() / (theta.cos() + xi);
        let u2 = u * u;
        let mut power = u * u2;
        let mut projected = u;
        for coefficient in radial.iter().take(5) {
            projected += coefficient * power;
            power *= u2;
        }
        let physical = evaluate_profile(profile, theta_degrees);
        let pixel = focal * projected;
        physical_squared += physical * physical;
        physical_pixel += physical * pixel;
    }
    let scale = physical_pixel / physical_squared;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(Error::MissingCalibration(
            "source optical profile does not yield a positive physical-to-pixel scale".into(),
        ));
    }
    Ok(scale)
}

fn fit_v6_radial_profile<const N: usize>(
    xi: f64,
    profile: [f64; N],
    fov_degrees: f64,
) -> Result<(f64, [f64; 5])> {
    let sample_count = ((fov_degrees * 0.5) / PROFILE_SAMPLE_STEP_DEGREES + 0.5) as usize;
    let mut normal = [[0.0; 5]; 5];
    let mut rhs = [0.0; 5];
    for sample in 0..sample_count {
        let theta_degrees = sample as f64 * PROFILE_SAMPLE_STEP_DEGREES;
        let theta = theta_degrees.to_radians();
        let u = theta.sin() / (theta.cos() + xi);
        let u2 = u * u;
        let row = [u, u * u2, u * u2.powi(2), u * u2.powi(3), u * u2.powi(4)];
        let physical = evaluate_profile(profile, theta_degrees);
        for column in 0..5 {
            rhs[column] += row[column] * physical;
            for other in 0..5 {
                normal[column][other] += row[column] * row[other];
            }
        }
    }

    let fitted = solve_symmetric_ldlt(normal, rhs)?;
    let focal = fitted[0];
    if !focal.is_finite() || focal <= 0.0 {
        return Err(Error::MissingCalibration(
            "target optical profile fit produced an invalid physical focal length".into(),
        ));
    }
    let radial = [
        fitted[1] / focal,
        fitted[2] / focal,
        fitted[3] / focal,
        fitted[4] / focal,
        0.0,
    ];
    if !radial.iter().all(|value| value.is_finite()) {
        return Err(Error::MissingCalibration(
            "target optical profile fit produced non-finite radial coefficients".into(),
        ));
    }
    Ok((focal, radial))
}

fn evaluate_profile<const N: usize>(coefficients: [f64; N], theta_degrees: f64) -> f64 {
    coefficients
        .into_iter()
        .rev()
        .fold(0.0, |value, coefficient| {
            value * theta_degrees + coefficient
        })
}

fn solve_symmetric_ldlt(mut matrix: [[f64; 5]; 5], rhs: [f64; 5]) -> Result<[f64; 5]> {
    let mut diagonal = [0.0; 5];
    let maximum_diagonal = (0..5)
        .map(|index| matrix[index][index].abs())
        .fold(0.0_f64, f64::max);
    let tolerance = f64::EPSILON * maximum_diagonal.max(1.0);

    for row in 0..5 {
        for column in 0..row {
            let mut value = matrix[row][column];
            for previous in 0..column {
                value -= matrix[row][previous] * diagonal[previous] * matrix[column][previous];
            }
            if diagonal[column].abs() <= tolerance {
                return Err(Error::MissingCalibration(
                    "optical-profile fit is singular".into(),
                ));
            }
            matrix[row][column] = value / diagonal[column];
        }
        let mut value = matrix[row][row];
        for previous in 0..row {
            value -= matrix[row][previous] * matrix[row][previous] * diagonal[previous];
        }
        if !value.is_finite() || value.abs() <= tolerance {
            return Err(Error::MissingCalibration(
                "optical-profile fit is singular".into(),
            ));
        }
        diagonal[row] = value;
    }

    let mut solved = rhs;
    for row in 0..5 {
        for column in 0..row {
            solved[row] -= matrix[row][column] * solved[column];
        }
    }
    for (value, divisor) in solved.iter_mut().zip(diagonal) {
        *value /= divisor;
    }
    for row in (0..5).rev() {
        for column in row + 1..5 {
            solved[row] -= matrix[column][row] * solved[column];
        }
    }
    Ok(solved)
}

fn encode_v6_offset(calibration: &ResolvedCalibration) -> String {
    let mut fields = Vec::with_capacity(56);
    fields.push(calibration.lenses.len().to_string());
    for lens in &calibration.lenses {
        fields.extend([
            lens.xi.expect("validated V6 lens has xi").to_string(),
            lens.fx.to_string(),
            lens.fy.to_string(),
            lens.cx.to_string(),
            lens.cy.to_string(),
            lens.euler_degrees[0].to_string(),
            lens.euler_degrees[1].to_string(),
            lens.euler_degrees[2].to_string(),
            lens.translation[0].to_string(),
            lens.translation[1].to_string(),
            lens.translation[2].to_string(),
        ]);
        fields.extend(lens.distortion_coefficients.iter().map(ToString::to_string));
        fields.extend([
            lens.canvas_width.to_string(),
            lens.canvas_height.to_string(),
            lens.lens_type.to_string(),
        ]);
    }
    fields.push(
        ((u32::from(calibration.offset_version) << 16) | calibration.offset_flags).to_string(),
    );
    fields.join("_")
}

fn x5_setup_for_lens_type(lens_type: u32) -> Option<OpticalProfile> {
    lens_profile(&CameraModel::X5, lens_type).map(|profile| profile.optical_profile)
}

fn parse_profile_payload(profile: &EmbeddedProfile) -> Result<ParsedEmbeddedProfile> {
    if profile.payload.is_empty() || profile.payload.len() > MAX_PROFILE_BYTES {
        return Err(Error::MissingCalibration(format!(
            "profile {} has an empty or oversized protobuf payload",
            profile.name
        )));
    }

    let mut cursor = 0usize;
    let mut encoded_name = None;
    let mut fixed64_values = Vec::new();
    let mut classification_values = Vec::new();
    while cursor < profile.payload.len() {
        let key = read_profile_varint(&profile.payload, &mut cursor)?;
        let field = key >> 3;
        let wire = (key & 7) as u8;
        match (field, wire) {
            (1, 2) => {
                let length = usize::try_from(read_profile_varint(&profile.payload, &mut cursor)?)
                    .map_err(|_| profile_error(profile, "string length exceeds usize"))?;
                let end = cursor
                    .checked_add(length)
                    .filter(|end| *end <= profile.payload.len())
                    .ok_or_else(|| profile_error(profile, "truncated name field"))?;
                let name = std::str::from_utf8(&profile.payload[cursor..end])
                    .map_err(|_| profile_error(profile, "name is not UTF-8"))?;
                encoded_name = Some(name.to_owned());
                cursor = end;
            }
            (2, 1) => {
                let end = cursor
                    .checked_add(8)
                    .filter(|end| *end <= profile.payload.len())
                    .ok_or_else(|| profile_error(profile, "truncated fixed64 coefficient"))?;
                let bytes: [u8; 8] = profile.payload[cursor..end]
                    .try_into()
                    .expect("fixed-size slice was checked");
                fixed64_values.push(f64::from_le_bytes(bytes));
                cursor = end;
            }
            (2, 0) => {
                classification_values.push(read_profile_varint(&profile.payload, &mut cursor)?);
            }
            (_, _) => skip_profile_field(&profile.payload, &mut cursor, wire)
                .map_err(|detail| profile_error(profile, &detail))?,
        }
    }

    let encoded_name = encoded_name.ok_or_else(|| profile_error(profile, "missing name field"))?;
    if encoded_name != profile.name {
        return Err(profile_error(
            profile,
            &format!(
                "outer name {:?} does not match protobuf name {encoded_name:?}",
                profile.name
            ),
        ));
    }
    let payload = match (fixed64_values.as_slice(), classification_values.as_slice()) {
        (values, []) if values.len() == PROFILE_COEFFICIENT_COUNT => {
            if !values.iter().all(|value| value.is_finite()) {
                return Err(profile_error(profile, "contains a non-finite coefficient"));
            }
            EmbeddedProfilePayload::SixCoefficientTransform(
                values.try_into().expect("coefficient count was checked"),
            )
        }
        ([], [value]) => EmbeddedProfilePayload::ClassificationValue(*value),
        (values, classes) => {
            return Err(profile_error(
                profile,
                &format!(
                    "unsupported field-2 shape ({} fixed64 values, {} varints)",
                    values.len(),
                    classes.len()
                ),
            ));
        }
    };

    Ok(ParsedEmbeddedProfile {
        name: encoded_name,
        payload,
    })
}

fn read_profile_varint(input: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *input
            .get(*cursor)
            .ok_or_else(|| Error::MissingCalibration("truncated profile protobuf varint".into()))?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(Error::MissingCalibration(
                "profile protobuf varint overflowed".into(),
            ));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::MissingCalibration(
        "profile protobuf varint exceeded ten bytes".into(),
    ))
}

fn skip_profile_field(
    input: &[u8],
    cursor: &mut usize,
    wire: u8,
) -> std::result::Result<(), String> {
    let length = match wire {
        0 => {
            read_profile_varint(input, cursor).map_err(|error| error.to_string())?;
            return Ok(());
        }
        1 => 8,
        2 => {
            usize::try_from(read_profile_varint(input, cursor).map_err(|error| error.to_string())?)
                .map_err(|_| "length-delimited field exceeds usize".to_owned())?
        }
        5 => 4,
        _ => return Err(format!("unsupported protobuf wire type {wire}")),
    };
    *cursor = cursor
        .checked_add(length)
        .filter(|end| *end <= input.len())
        .ok_or_else(|| "truncated protobuf field".to_owned())?;
    Ok(())
}

fn profile_error(profile: &EmbeddedProfile, detail: &str) -> Error {
    Error::MissingCalibration(format!(
        "invalid embedded profile {}: {detail}",
        profile.name
    ))
}

fn positive_integer(value: f64, name: &str) -> Result<u32> {
    let integer = non_negative_integer(value, name)?;
    if integer == 0 {
        return Err(Error::MissingCalibration(format!(
            "{name} must be positive"
        )));
    }
    Ok(integer)
}

fn non_negative_integer(value: f64, name: &str) -> Result<u32> {
    if !(0.0..=(u32::MAX as f64)).contains(&value) || value.fract() != 0.0 {
        return Err(Error::MissingCalibration(format!(
            "{name} must be a non-negative integer"
        )));
    }
    Ok(value as u32)
}

/// Build a convenient synthetic calibration for tests and downstream prototypes.
pub fn synthetic_dual_fisheye_calibration(
    lens_width: u32,
    lens_height: u32,
) -> Result<ResolvedCalibration> {
    if lens_width == 0 || lens_height == 0 {
        return Err(Error::MissingCalibration(
            "synthetic lens dimensions must be non-zero".into(),
        ));
    }
    let canvas_width = lens_width
        .checked_mul(2)
        .ok_or_else(|| Error::MissingCalibration("synthetic canvas overflowed".into()))?;
    let focal = f64::from(lens_width.min(lens_height)) / (PI * 1.1);
    let make_lens = |index: usize, orientation: Orientation| ParsedLens {
        model: LensProjectionModel::OmniRadtan,
        radius: None,
        xi: Some(0.0),
        cx: f64::from(lens_width) * (index as f64 + 0.5),
        cy: f64::from(lens_height) * 0.5,
        fx: focal,
        fy: focal,
        k1: 0.0,
        k2: 0.0,
        k3: 0.0,
        distortion_coefficients: vec![0.0; 5],
        polynomial_projection: None,
        euler_degrees: [0.0; 3],
        orientation,
        translation: [0.0; 3],
        canvas_width,
        canvas_height: lens_height,
        lens_type: 0,
    };
    let calibration = ResolvedCalibration {
        camera_model: None,
        optical_resolution: None,
        offset_version: 3,
        offset_source: OffsetSource::Current,
        offset_flags: 0,
        profile_name: Some("synthetic".into()),
        lenses: [
            make_lens(0, Orientation::IDENTITY),
            make_lens(1, Orientation::from_axis_angle([0.0, 1.0, 0.0], PI)?),
        ],
        lens_geometry: [None, None],
        canvas_width,
        canvas_height: lens_height,
        raw_offset: "synthetic".into(),
    };
    calibration.validate()?;
    Ok(calibration)
}
