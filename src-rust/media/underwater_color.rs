//! Job-owned restoration state, shared by image and video export.

use super::*;
use crate::underwater::UnderwaterColorSession;
use crate::{UnderwaterColorMode, UnderwaterColorOptions};

pub(super) struct UnderwaterProcessor {
    options: UnderwaterColorOptions,
    frame_rate: ffmpeg::Rational,
    prepared: Option<(u32, u32, UnderwaterColorSession)>,
}

/// A borrowed restoration request for one unpublished panorama. GPU rendering
/// consumes the same temporal session as CPU rendering; no parallel history exists.
pub(super) struct FrameRestoration<'a> {
    processor: &'a mut UnderwaterProcessor,
    timestamp_micros: i64,
    continuous: bool,
}

impl FrameRestoration<'_> {
    pub(super) fn enabled(&self) -> bool {
        self.processor.enabled()
    }

    #[cfg(all(feature = "gpu", feature = "underwater-ai"))]
    pub(super) fn gpu_frame(
        &mut self,
        projection: EquirectangularProjection,
    ) -> Result<Option<crate::underwater::GpuUnderwaterFrame<'_>>> {
        self.processor
            .prepare(projection.width, projection.height)?;
        let Some((_, _, session)) = &mut self.processor.prepared else {
            return Ok(None);
        };
        session.prepare_gpu_frame(self.timestamp_micros as f64 / 1_000_000.0, self.continuous)
    }

    pub(super) fn process(self, frame: PanoramaFrame) -> Result<PanoramaFrame> {
        if self.continuous {
            self.processor
                .process_continuous(frame, self.timestamp_micros)
        } else {
            self.processor.process(frame, self.timestamp_micros)
        }
    }
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

    pub(super) fn frame(
        &mut self,
        timestamp_micros: i64,
        continuous: bool,
    ) -> FrameRestoration<'_> {
        FrameRestoration {
            processor: self,
            timestamp_micros,
            continuous,
        }
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
        if !self.enabled() {
            return Ok(());
        }
        if let Some((current_width, current_height, session)) = &mut self.prepared {
            if (*current_width, *current_height) != (width, height) {
                session.resize(width, height)?;
                *current_width = width;
                *current_height = height;
            }
        } else {
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
    pub(super) fn process_continuous(
        &mut self,
        frame: PanoramaFrame,
        timestamp_micros: i64,
    ) -> Result<PanoramaFrame> {
        if !self.enabled() {
            return Ok(frame);
        }
        let (width, height) = (frame.width(), frame.height());
        let mut rgb = frame.into_rgb8();
        self.process_rgb8_continuous(&mut rgb, width, height, timestamp_micros)?;
        PanoramaFrame::new(width, height, rgb)
    }

    pub(super) fn process_rgb8_continuous(
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
            .process_rgb8_continuous(rgb, timestamp_micros as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underwater_resize_reuses_prepared_engine_and_keeps_same_size_history() {
        let options = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Legacy,
            ..Default::default()
        };
        let mut processor = UnderwaterProcessor::new(options, Some(30.0)).unwrap();
        processor.prepare(128, 96).unwrap();
        let identity = processor.prepared.as_ref().unwrap().2.engine_identity();
        for (width, height) in [(128, 96), (64, 80), (128, 96)] {
            let mut fresh = UnderwaterProcessor::new(options, Some(30.0)).unwrap();
            for (frame, color) in [[35, 85, 110], [80, 110, 140]].into_iter().enumerate() {
                let mut actual = color.repeat(width as usize * height as usize);
                let mut expected = actual.clone();
                let pts = 1_000_000 + frame as i64 * 50_000;
                processor
                    .process_rgb8_continuous(&mut actual, width, height, pts)
                    .unwrap();
                fresh
                    .process_rgb8_continuous(&mut expected, width, height, pts)
                    .unwrap();
                assert!(
                    actual == expected,
                    "media resize/history differs at {width}x{height}, frame {frame}"
                );
                assert_eq!(
                    processor.prepared.as_ref().unwrap().2.engine_identity(),
                    identity
                );
            }
        }
    }
}
