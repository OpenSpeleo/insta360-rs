//! Explicit underwater color restoration, separate from optical correction.
//!
//! Restoration operates on a decoded RGB panorama and never changes projected
//! coordinates, source selection, or seam ownership. Housing detection does not
//! enable this module: callers must select a mode explicitly.

#[cfg(feature = "underwater-ai")]
mod ai;
mod ilut;
mod legacy;
#[cfg(feature = "underwater-ai")]
mod model;
#[cfg(feature = "underwater-ai")]
mod style;

use crate::assets::{AssetPolicy, AssetProvider, BundledAssetProvider};
use crate::{Error, Result, UnderwaterColorMode, UnderwaterColorOptions};

/// Returns the actual linked independent MNN library's version, not a build marker.
/// Builds without `underwater-ai` return [`Error::MissingCapability`].
pub fn mnn_runtime_version() -> Result<&'static str> {
    #[cfg(feature = "underwater-ai")]
    {
        model::runtime_version()
    }
    #[cfg(not(feature = "underwater-ai"))]
    {
        Err(Error::MissingCapability(
            "underwater AI was not compiled into this build".into(),
        ))
    }
}

/// Reusable restoration state for one decoded RGB8 image or continuous video job.
///
/// Prepare once per output size. Frames use packed RGB channel order; processing
/// does not resize or move pixels. Call [`Self::reset`] at recording boundaries
/// and before unrelated selected images. Non-increasing timestamps also reset
/// temporal state. The supplied frame rate is validated but correction smoothing
/// follows processed frames, as in the reference CPU implementation.
pub struct UnderwaterColorSession {
    engine: Engine,
    frame_bytes: usize,
    previous_pts: Option<f64>,
}

enum Engine {
    Off,
    Legacy(Box<legacy::LegacySession>),
    #[cfg(feature = "underwater-ai")]
    Ai(Box<ai::AiSession>),
}

impl UnderwaterColorSession {
    /// Checks settings, verifies complete required resources, and prepares
    /// job-owned processing buffers and, for AI, independent MNN CPU sessions.
    pub fn prepare(
        options: UnderwaterColorOptions,
        width: u32,
        height: u32,
        fps_numerator: u32,
        fps_denominator: u32,
        provider: &dyn AssetProvider,
    ) -> Result<Self> {
        options.validate_capabilities()?;
        let area = (width as usize)
            .checked_mul(height as usize)
            .filter(|area| *area > 0 && *area <= 64 * 1024 * 1024)
            .ok_or_else(|| {
                Error::InvalidMedia(
                    "underwater color requires positive dimensions and at most 64 megapixels"
                        .into(),
                )
            })?;
        if fps_numerator == 0 || fps_denominator == 0 {
            return Err(Error::InvalidMedia(
                "underwater color requires a positive rational frame rate".into(),
            ));
        }
        let engine = match options.mode {
            UnderwaterColorMode::Off => Engine::Off,
            UnderwaterColorMode::Legacy => {
                let manifest = BundledAssetProvider::manifest()?;
                let asset = manifest
                    .load_verified(provider, "underwater-legacy-ilut", AssetPolicy::Required)?
                    .expect("required asset");
                Engine::Legacy(Box::new(legacy::LegacySession::new(
                    width,
                    height,
                    fps_numerator,
                    fps_denominator,
                    options.effective_strength(),
                    options.effective_balance(),
                    ilut::IntegerLut::parse(&asset.bytes)?,
                )?))
            }
            UnderwaterColorMode::Ai => {
                #[cfg(feature = "underwater-ai")]
                {
                    Engine::Ai(Box::new(ai::AiSession::new(
                        width,
                        height,
                        options.effective_strength(),
                        options.effective_style(),
                        provider,
                    )?))
                }
                #[cfg(not(feature = "underwater-ai"))]
                {
                    unreachable!("capability validated before preparing resources")
                }
            }
        };
        Ok(Self {
            engine,
            frame_bytes: area * 3,
            previous_pts: None,
        })
    }

    /// Restores a packed RGB8 frame in place. Invalid lengths or non-finite
    /// timestamps are rejected before pixels or temporal state are changed.
    pub fn process_rgb8(&mut self, pixels: &mut [u8], pts_seconds: f64) -> Result<()> {
        if pixels.len() != self.frame_bytes || !pts_seconds.is_finite() {
            return Err(Error::InvalidMedia(
                "underwater color frame length or timestamp is invalid".into(),
            ));
        }
        if self
            .previous_pts
            .is_some_and(|previous| pts_seconds <= previous)
        {
            self.reset();
        }
        match &mut self.engine {
            Engine::Off => {}
            Engine::Legacy(session) => session.process_rgb8(pixels, pts_seconds)?,
            #[cfg(feature = "underwater-ai")]
            Engine::Ai(session) => session.process_rgb8(pixels)?,
        }
        self.previous_pts = Some(pts_seconds);
        Ok(())
    }

    /// Clears temporal history while retaining prepared models and allocations.
    pub fn reset(&mut self) {
        self.previous_pts = None;
        match &mut self.engine {
            Engine::Off => {}
            Engine::Legacy(session) => session.reset(),
            #[cfg(feature = "underwater-ai")]
            Engine::Ai(session) => session.reset(),
        }
    }
}

impl UnderwaterColorOptions {
    /// Validates options and checks that the selected inference engine was built.
    /// Resource integrity is checked when a processing session is prepared.
    pub fn validate_capabilities(&self) -> Result<()> {
        self.validate()?;
        if self.mode == UnderwaterColorMode::Ai && !cfg!(feature = "underwater-ai") {
            return Err(Error::MissingCapability("underwater AI restoration requires the underwater-ai feature and its pinned MNN CPU build".into()));
        }
        Ok(())
    }

    /// Checks ranges and rejects controls that do not apply to the selected mode.
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [("strength", self.strength), ("balance", self.balance)] {
            if value.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
                return Err(Error::InvalidMedia(format!(
                    "underwater color {name} must be finite and between 0 and 1"
                )));
            }
        }
        match self.mode {
            UnderwaterColorMode::Off
                if self.strength.is_some() || self.balance.is_some() || self.style.is_some() =>
            {
                Err(Error::InvalidMedia(
                    "underwater color controls require Legacy or Ai mode".into(),
                ))
            }
            UnderwaterColorMode::Legacy if self.style.is_some() => Err(Error::InvalidMedia(
                "underwater color style requires Ai mode".into(),
            )),
            UnderwaterColorMode::Ai if self.balance.is_some() => Err(Error::InvalidMedia(
                "underwater color balance requires Legacy mode".into(),
            )),
            UnderwaterColorMode::Ai if self.style.is_some_and(|style| style > 3) => Err(
                Error::InvalidMedia("underwater AI style must be between 0 and 3".into()),
            ),
            _ => Ok(()),
        }
    }

    /// Returns the chosen strength, or the selected mode's reference default.
    pub fn effective_strength(&self) -> f32 {
        self.strength.unwrap_or(match self.mode {
            UnderwaterColorMode::Off => 0.0,
            UnderwaterColorMode::Legacy => 0.8,
            UnderwaterColorMode::Ai => 1.0,
        })
    }

    /// Returns the legacy correction balance, defaulting to 0.5.
    pub fn effective_balance(&self) -> f32 {
        self.balance.unwrap_or(0.5)
    }

    /// Returns the AI style index, defaulting to style zero.
    pub fn effective_style(&self) -> u32 {
        self.style.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MissingAssets;
    impl AssetProvider for MissingAssets {
        fn load(&self, _: &str) -> crate::assets::AssetResult<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    #[test]
    fn public_session_off_is_identity_and_rejects_invalid_frames_before_mutation() {
        let options = UnderwaterColorOptions::default();
        let mut session =
            UnderwaterColorSession::prepare(options, 2, 1, 30, 1, &MissingAssets).unwrap();
        let original = [0, 127, 255, 5, 9, 11];
        for frame in 0..1000 {
            let mut pixels = original;
            session
                .process_rgb8(&mut pixels, f64::from(frame) / 30.0)
                .unwrap();
            assert_eq!(pixels, original);
        }
        for pts in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut pixels = original;
            assert!(session.process_rgb8(&mut pixels, pts).is_err());
            assert_eq!(pixels, original);
        }
        assert!(session.process_rgb8(&mut [7; 5], 0.0).is_err());
        session.reset();
        session.process_rgb8(&mut original.clone(), 0.0).unwrap();
        for (width, height, num, den) in [
            (0, 1, 30, 1),
            (1, 0, 30, 1),
            (u32::MAX, u32::MAX, 30, 1),
            (1, 1, 0, 1),
            (1, 1, 30, 0),
        ] {
            assert!(UnderwaterColorSession::prepare(
                options,
                width,
                height,
                num,
                den,
                &MissingAssets
            )
            .is_err());
        }
    }

    #[test]
    fn public_legacy_requires_verified_assets_and_zero_strength_preserves_every_byte() {
        let options = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Legacy,
            strength: Some(0.0),
            ..Default::default()
        };
        assert!(UnderwaterColorSession::prepare(options, 64, 64, 30, 1, &MissingAssets).is_err());
        assert!(
            UnderwaterColorSession::prepare(options, 63, 64, 30, 1, &BundledAssetProvider).is_err()
        );
        let mut session =
            UnderwaterColorSession::prepare(options, 64, 64, 30, 1, &BundledAssetProvider).unwrap();
        let original: Vec<_> = (0..64 * 64 * 3).map(|index| (index % 256) as u8).collect();
        for pts in [0.0, 1.0, 0.0, 10.0] {
            let mut pixels = original.clone();
            session.process_rgb8(&mut pixels, pts).unwrap();
            assert_eq!(pixels, original);
            session.reset();
        }
    }

    #[cfg(not(feature = "underwater-ai"))]
    #[test]
    fn ai_capability_fails_before_requesting_resources_without_native_feature() {
        assert!(matches!(
            mnn_runtime_version(),
            Err(Error::MissingCapability(_))
        ));
        let options = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Ai,
            ..Default::default()
        };
        options.validate().unwrap();
        assert!(matches!(
            options.validate_capabilities(),
            Err(Error::MissingCapability(_))
        ));
        assert!(matches!(
            UnderwaterColorSession::prepare(options, 64, 64, 30, 1, &MissingAssets),
            Err(Error::MissingCapability(_))
        ));
    }

    #[test]
    fn defaults_are_off_and_mode_specific_controls_are_strict() {
        let off = UnderwaterColorOptions::default();
        assert_eq!(off.mode, UnderwaterColorMode::Off);
        assert_eq!(off.effective_strength(), 0.0);
        off.validate().unwrap();
        let legacy = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Legacy,
            ..off
        };
        assert_eq!(legacy.effective_strength(), 0.8);
        assert_eq!(legacy.effective_balance(), 0.5);
        let ai = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Ai,
            ..off
        };
        assert_eq!(ai.effective_strength(), 1.0);
        assert_eq!(ai.effective_style(), 0);
        for options in [
            UnderwaterColorOptions {
                strength: Some(0.0),
                ..off
            },
            UnderwaterColorOptions {
                balance: Some(0.0),
                ..off
            },
            UnderwaterColorOptions {
                style: Some(0),
                ..off
            },
            UnderwaterColorOptions {
                style: Some(0),
                ..legacy
            },
            UnderwaterColorOptions {
                balance: Some(0.5),
                ..ai
            },
            UnderwaterColorOptions {
                style: Some(4),
                ..ai
            },
        ] {
            assert!(options.validate().is_err());
        }
        for value in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -f32::EPSILON,
            1.0 + f32::EPSILON,
        ] {
            assert!(UnderwaterColorOptions {
                strength: Some(value),
                ..legacy
            }
            .validate()
            .is_err());
            assert!(UnderwaterColorOptions {
                balance: Some(value),
                ..legacy
            }
            .validate()
            .is_err());
        }
        for strength in [0.0, 0.5, 1.0] {
            UnderwaterColorOptions {
                strength: Some(strength),
                balance: Some(strength),
                ..legacy
            }
            .validate()
            .unwrap();
            for style in 0..=3 {
                UnderwaterColorOptions {
                    strength: Some(strength),
                    style: Some(style),
                    ..ai
                }
                .validate()
                .unwrap();
            }
        }
    }
}
