//! Job-owned restoration state, shared by image and video export.

use super::*;
use crate::underwater::UnderwaterColorSession;
use crate::{UnderwaterColorMode, UnderwaterColorOptions};

pub(super) struct UnderwaterProcessor {
    options: UnderwaterColorOptions,
    frame_rate: ffmpeg::Rational,
    prepared: Option<(u32, u32, UnderwaterColorSession)>,
}

impl UnderwaterProcessor {
    pub(super) fn new(options: UnderwaterColorOptions, frame_rate: Option<f64>) -> Result<Self> {
        options.validate_capabilities()?;
        let frame_rate = if options.mode == UnderwaterColorMode::Off {
            (1, 1).into()
        } else {
            let rate = frame_rate
                .filter(|rate| rate.is_finite() && *rate > 0.0)
                .ok_or_else(|| {
                    Error::MissingCapability(
                        "underwater color requires a known positive source frame rate".into(),
                    )
                })?;
            let rate = ffmpeg::Rational::from(rate);
            if rate.numerator() <= 0 || rate.denominator() <= 0 {
                return Err(Error::InvalidMedia(
                    "underwater color frame rate is outside the supported range".into(),
                ));
            }
            rate
        };
        Ok(Self {
            options,
            frame_rate,
            prepared: None,
        })
    }

    pub(super) fn enabled(&self) -> bool {
        self.options.mode != UnderwaterColorMode::Off
    }

    pub(super) fn reset(&mut self) {
        if let Some((_, _, session)) = &mut self.prepared {
            session.reset();
        }
    }

    /// Verifies resources and dimensions before allocating output or writing files.
    pub(super) fn prepare(&mut self, width: u32, height: u32) -> Result<()> {
        if self.enabled()
            && self
                .prepared
                .as_ref()
                .is_none_or(|(w, h, _)| (*w, *h) != (width, height))
        {
            let session = UnderwaterColorSession::prepare(
                self.options,
                width,
                height,
                self.frame_rate.numerator() as u32,
                self.frame_rate.denominator() as u32,
                &crate::assets::BundledAssetProvider,
            )?;
            self.prepared = Some((width, height, session));
        }
        Ok(())
    }

    pub(super) fn process(
        &mut self,
        frame: PanoramaFrame,
        timestamp_micros: i64,
    ) -> Result<PanoramaFrame> {
        if !self.enabled() {
            return Ok(frame);
        }
        let (width, height) = (frame.width(), frame.height());
        let mut rgb = frame.into_rgb8();
        self.process_rgb8(&mut rgb, width, height, timestamp_micros)?;
        PanoramaFrame::new(width, height, rgb)
    }

    pub(super) fn process_rgb8(
        &mut self,
        rgb: &mut [u8],
        width: u32,
        height: u32,
        timestamp_micros: i64,
    ) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        self.prepare(width, height)?;
        self.prepared
            .as_mut()
            .expect("prepared restoration session")
            .2
            .process_rgb8(rgb, timestamp_micros as f64 / 1_000_000.0)
    }
}
