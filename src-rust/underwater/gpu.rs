//! One transactional GPU frame over the same CPU neural session and timing.
use super::{
    ai::{AiSchedule, AiSession},
    Engine, UnderwaterColorSession,
};
use crate::{gpu::GpuUnderwaterProcessor, Error, Result};

pub(crate) struct GpuUnderwaterFrame<'a> {
    owner: &'a mut UnderwaterColorSession,
    schedule: AiSchedule,
    pts_seconds: f64,
    continuous: bool,
    updated: bool,
    committed: bool,
}

impl UnderwaterColorSession {
    /// Call after preparing the output dimensions. A failed GPU frame resets
    /// history, so the renderer can retry its unpublished image on the CPU.
    pub(crate) fn prepare_gpu_frame(
        &mut self,
        pts_seconds: f64,
        continuous: bool,
    ) -> Result<Option<GpuUnderwaterFrame<'_>>> {
        if !matches!(&self.engine, Engine::Ai(ai) if ai.gpu_enabled()) {
            return Ok(None);
        }
        let elapsed = self.prepare_timing(pts_seconds, continuous)?;
        let Engine::Ai(ai) = &mut self.engine else {
            unreachable!("AI checked above")
        };
        let schedule = ai.schedule(continuous.then_some(elapsed));
        Ok(Some(GpuUnderwaterFrame {
            owner: self,
            schedule,
            pts_seconds,
            continuous,
            updated: false,
            committed: false,
        }))
    }
}

impl GpuUnderwaterFrame<'_> {
    fn ai(&self) -> &AiSession {
        let Engine::Ai(ai) = &self.owner.engine else {
            unreachable!("GPU AI frame")
        };
        ai
    }
    fn ai_mut(&mut self) -> &mut AiSession {
        let Engine::Ai(ai) = &mut self.owner.engine else {
            unreachable!("GPU AI frame")
        };
        ai
    }
}

impl GpuUnderwaterProcessor for GpuUnderwaterFrame<'_> {
    fn dimensions(&self) -> (u32, u32) {
        self.ai().gpu_dimensions()
    }
    fn needs_analysis(&self) -> bool {
        self.schedule.update
    }
    fn update(&mut self, rgb224: &[u8]) -> Result<()> {
        if self.updated {
            return Err(Error::InvalidMedia(
                "GPU underwater frame was already updated".into(),
            ));
        }
        let schedule = self.schedule;
        self.ai_mut().gpu_update(rgb224, schedule)?;
        self.updated = true;
        Ok(())
    }
    fn lut(&self) -> &[[u8; 3]] {
        self.ai().gpu_lut()
    }
    fn commit(&mut self) {
        debug_assert!(!self.committed, "GPU frame committed twice");
        debug_assert!(!self.schedule.update || self.updated);
        self.ai_mut().gpu_commit();
        self.owner.previous_pts = Some(self.pts_seconds);
        self.owner.continuous = Some(self.continuous);
        self.committed = true;
    }
}

impl Drop for GpuUnderwaterFrame<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.owner.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assets::BundledAssetProvider, UnderwaterColorMode, UnderwaterColorOptions};

    fn session() -> UnderwaterColorSession {
        UnderwaterColorSession::prepare(
            UnderwaterColorOptions {
                mode: UnderwaterColorMode::Ai,
                ..Default::default()
            },
            80,
            64,
            60,
            1,
            &BundledAssetProvider,
        )
        .unwrap()
    }

    fn pixels(seed: u32) -> Vec<u8> {
        (0..80 * 64)
            .flat_map(|i| {
                [
                    20 + ((i + seed * 13) % 31) as u8,
                    80 + ((i + seed * 47) % 61) as u8,
                    100 + ((i + seed * 17) % 101) as u8,
                ]
            })
            .collect()
    }

    fn update(frame: &mut GpuUnderwaterFrame<'_>, pixels: &[u8]) {
        if frame.needs_analysis() {
            let mut small = vec![0; 224 * 224 * 3];
            super::super::resize::rgb8(pixels, 80, 64, &mut small, 224, 224);
            frame.update(&small).unwrap();
        }
    }

    #[test]
    fn gpu_transaction_matches_cpu_cadence_dropped_frames_seeks_and_policy_changes() {
        let mut gpu = session();
        let mut cpu = session();
        let cases = [
            (0.0, true, true),
            (1.0 / 60.0, true, false),
            (10.0 / 60.0, true, true),
            (12.0 / 60.0, true, false),
            (1.0, true, true),
            (61.0 / 60.0, true, false),
            (0.5, true, true),
            (0.5, true, true),
            (1.0, false, true),
            (2.0, false, false),
        ];
        for (index, (pts, continuous, inference)) in cases.into_iter().enumerate() {
            let mut actual = pixels(index as u32);
            let mut expected = actual.clone();
            if continuous {
                cpu.process_rgb8_continuous(&mut expected, pts).unwrap();
            } else {
                cpu.process_rgb8(&mut expected, pts).unwrap();
            }
            let mut frame = gpu.prepare_gpu_frame(pts, continuous).unwrap().unwrap();
            assert_eq!(frame.needs_analysis(), inference, "frame{index}");
            update(&mut frame, &actual);
            let lut =
                super::super::ilut::IntegerLut::from_grid(16, 17, frame.lut().to_vec()).unwrap();
            lut.apply_rgb8(&mut actual);
            frame.commit();
            drop(frame);
            assert!(
                actual == expected,
                "transaction output differs at frame{index}"
            );
            assert_eq!(gpu.previous_pts, cpu.previous_pts);
            assert_eq!(gpu.continuous, cpu.continuous);
        }
    }

    #[test]
    fn abandoned_gpu_model_update_resets_history_without_reopening_models() {
        let mut gpu = session();
        let allocation = gpu.engine_identity();
        let source = pixels(3);
        {
            let mut frame = gpu.prepare_gpu_frame(0.0, true).unwrap().unwrap();
            update(&mut frame, &source);
            frame.commit();
        }
        {
            let mut frame = gpu.prepare_gpu_frame(1.0, true).unwrap().unwrap();
            update(&mut frame, &pixels(4));
            // Simulate failure after inference and before final GPU readback.
        }
        assert_eq!(gpu.engine_identity(), allocation);
        assert!(gpu.previous_pts.is_none());
        let mut fresh = session();
        let mut expected = source.clone();
        let mut actual = source;
        fresh.process_rgb8_continuous(&mut expected, 1.0).unwrap();
        gpu.process_rgb8_continuous(&mut actual, 1.0).unwrap();
        assert!(
            actual == expected,
            "CPU retry inherited an unpublished GPU LUT"
        );
    }
}
