//! Independent MNN execution of the verified Studio neural restoration graph.
use super::resize::rgb8 as resize_rgb8;
use super::{
    ilut::IntegerLut,
    model::Model,
    style::{Database, Style},
};
use crate::{
    assets::{AssetPolicy, AssetProvider, BundledAssetProvider},
    Result,
};

const EDGE: usize = 17;
const ENTRIES: usize = EDGE * EDGE * EDGE;
const UPDATE_INTERVAL: u64 = 10;

#[derive(Clone, Copy)]
pub(super) struct AiSchedule {
    pub(super) update: bool,
    pub(super) feature: bool,
    pub(super) weights: [f32; 2],
}

#[derive(Debug)]
pub(super) struct AiSession {
    feature_model: Model,
    preset_model: Model,
    database: Database,
    style: Style,
    width: usize,
    height: usize,
    strength: f32,
    low: Vec<u8>,
    resized: Vec<u8>,
    features: Vec<f32>,
    image: Vec<f32>,
    identity: Vec<f32>,
    identity_luminance: Vec<f32>,
    output: Vec<f32>,
    smoothed: Vec<f32>,
    style_vector: [f32; 256],
    lut: IntegerLut,
    frame: u64,
    preview_clock: PreviewClock,
}
impl AiSession {
    pub(super) fn new(
        width: u32,
        height: u32,
        strength: f32,
        style_index: u32,
        provider: &dyn AssetProvider,
    ) -> Result<Self> {
        // The pinned MNN pool spin/yields between small convolution tasks.
        // Independent 1/2/4-thread processes show large scheduling stalls with
        // multiple workers under contention. Keep inference on its owner thread;
        // pixel work runs on the GPU or the separate bounded CPU pixel kernels.
        Self::new_with_threads(width, height, strength, style_index, provider, 1)
    }

    fn new_with_threads(
        width: u32,
        height: u32,
        strength: f32,
        style_index: u32,
        provider: &dyn AssetProvider,
        cpu_threads: usize,
    ) -> Result<Self> {
        let manifest = BundledAssetProvider::manifest()?;
        // Validate the whole resource group, including style previews, so a
        // partial external provider cannot silently select a different style.
        let mut resources = std::collections::BTreeMap::new();
        for id in &manifest
            .group("underwater-ai-studio-5-9-10")
            .expect("bundled group")
            .members
        {
            resources.insert(
                id.as_str(),
                manifest
                    .load_verified(provider, id, AssetPolicy::Required)?
                    .expect("required asset")
                    .bytes,
            );
        }
        let mut load = |id| resources.remove(id).expect("complete verified group");
        let mut original = load("underwater-model197-part0");
        original.extend_from_slice(&load("underwater-model197-part1"));
        let preset_model = Model::open_with_threads(&original, 197, cpu_threads)?;
        drop(original);
        let feature_model =
            Model::open_with_threads(&load("underwater-model198"), 198, cpu_threads)?;
        let database = Database::parse(&load("underwater-style-database"))?;
        let style = Style::parse(&load("underwater-style-manifest"), style_index)?;
        let mut identity = vec![0.0; ENTRIES * 3];
        for x in 0..EDGE {
            for y in 0..EDGE {
                for z in 0..EDGE {
                    let index = (x * EDGE + y) * EDGE + z;
                    for (channel, coordinate) in [x, y, z].into_iter().enumerate() {
                        identity[channel * ENTRIES + index] =
                            (coordinate * 16).min(255) as f32 / 255.0;
                    }
                }
            }
        }
        let identity_luminance = (0..ENTRIES)
            .map(|index| rgb_to_lab(std::array::from_fn(|c| identity[c * ENTRIES + index]))[0])
            .collect();
        Ok(Self {
            feature_model,
            preset_model,
            database,
            style,
            width: width as usize,
            height: height as usize,
            strength,
            low: vec![0; 224 * 224 * 3],
            resized: vec![0; 256 * 256 * 3],
            features: vec![0.0; 224 * 224 * 3],
            image: vec![0.0; 256 * 256 * 3],
            identity,
            identity_luminance,
            output: vec![0.0; ENTRIES * 3],
            smoothed: vec![0.0; ENTRIES * 3],
            style_vector: [0.0; 256],
            lut: IntegerLut::from_grid(16, 17, vec![[0; 3]; ENTRIES])?,
            frame: 0,
            preview_clock: PreviewClock::default(),
        })
    }

    /// Input dimensions do not affect either fixed-shape inference graph.
    #[cfg(any(feature = "media", test))]
    pub(super) fn resize(&mut self, width: u32, height: u32) {
        self.width = width as usize;
        self.height = height as usize;
        self.reset();
    }

    pub(super) fn reset(&mut self) {
        self.frame = 0;
        self.preview_clock = PreviewClock::default();
    }

    pub(super) fn process_rgb8(&mut self, pixels: &mut [u8]) -> Result<()> {
        let schedule = self.schedule(None);
        self.process_scheduled(pixels, schedule.update, schedule.feature, schedule.weights)
    }

    pub(super) fn process_rgb8_continuous(
        &mut self,
        pixels: &mut [u8],
        elapsed_frames: f64,
    ) -> Result<()> {
        let schedule = self.schedule(Some(elapsed_frames));
        self.process_scheduled(pixels, schedule.update, schedule.feature, schedule.weights)
    }

    pub(super) fn schedule(&mut self, elapsed_frames: Option<f64>) -> AiSchedule {
        if let Some(elapsed) = elapsed_frames {
            let (update, feature, weights) = self.preview_clock.advance(elapsed);
            AiSchedule {
                update,
                feature,
                weights,
            }
        } else {
            AiSchedule {
                update: self.frame.is_multiple_of(UPDATE_INTERVAL),
                feature: self.frame.is_multiple_of(6 * UPDATE_INTERVAL),
                weights: [0.8, 0.2],
            }
        }
    }

    #[cfg(all(feature = "gpu", any(feature = "media", test)))]
    pub(super) fn gpu_dimensions(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    #[cfg(all(feature = "gpu", any(feature = "media", test)))]
    pub(super) fn gpu_enabled(&self) -> bool {
        self.strength != 0.0
    }

    #[cfg(all(feature = "gpu", any(feature = "media", test)))]
    pub(super) fn gpu_lut(&self) -> &[[u8; 3]] {
        self.lut.entries()
    }

    #[cfg(all(feature = "gpu", any(feature = "media", test)))]
    pub(super) fn gpu_update(&mut self, rgb224: &[u8], schedule: AiSchedule) -> Result<()> {
        if !schedule.update || rgb224.len() != self.low.len() {
            return Err(crate::Error::InvalidMedia(
                "invalid GPU underwater analysis image".into(),
            ));
        }
        self.low.copy_from_slice(rgb224);
        self.prepare_current_low(schedule.feature, schedule.weights)
    }

    #[cfg(all(feature = "gpu", any(feature = "media", test)))]
    pub(super) fn gpu_commit(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    fn process_scheduled(
        &mut self,
        pixels: &mut [u8],
        update: bool,
        feature: bool,
        weights: [f32; 2],
    ) -> Result<()> {
        if self.strength == 0.0 {
            return Ok(());
        }
        if update {
            self.prepare_lut(pixels, feature, weights)?;
        }
        self.lut.apply_rgb8(pixels);
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    /// The learned-grid preparation is separate from full-frame application.
    /// Both consume the same pre-restoration RGB8 image and timing decision.
    fn prepare_lut(&mut self, pixels: &[u8], feature: bool, weights: [f32; 2]) -> Result<()> {
        resize_rgb8(pixels, self.width, self.height, &mut self.low, 224, 224);
        self.prepare_current_low(feature, weights)
    }

    fn prepare_current_low(&mut self, feature: bool, weights: [f32; 2]) -> Result<()> {
        if feature {
            self.update_style()?;
        }
        self.infer_lut()?;
        self.update_lut_grid(weights);
        Ok(())
    }

    fn update_style(&mut self) -> Result<()> {
        normalize_image(&self.low, &mut self.features, true);
        let mut feature = [0.0; 576];
        self.feature_model
            .run(&self.features, &[], &[], &mut feature)?;
        let norm = feature.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut feature {
                *value /= norm;
            }
        }
        self.style_vector = self.database.select(&feature, &self.style)?;
        Ok(())
    }

    fn infer_lut(&mut self) -> Result<()> {
        resize_rgb8(&self.low, 224, 224, &mut self.resized, 256, 256);
        normalize_image(&self.resized, &mut self.image, false);
        self.preset_model.run(
            &self.identity,
            &self.image,
            &self.style_vector,
            &mut self.output,
        )
    }

    fn update_lut_grid(&mut self, weights: [f32; 2]) {
        if self.frame == 0 {
            self.smoothed.copy_from_slice(&self.output);
        } else {
            for (old, new) in self.smoothed.iter_mut().zip(&self.output) {
                *old = weights[0] * *old + weights[1] * *new;
            }
        }
        let strength = self.strength;
        let smoothed = &self.smoothed;
        let identity = &self.identity;
        let identity_luminance = &self.identity_luminance;
        self.lut.update_grid(|index| {
            let src = std::array::from_fn(|c| identity[c * ENTRIES + index]);
            let restored = std::array::from_fn(|c| smoothed[c * ENTRIES + index]);
            apply_strength_with_luminance(src, identity_luminance[index], restored, 0.5, strength)
                .map(|v| (v * 255.0).round_ties_even().clamp(0.0, 255.0) as u8)
        });
    }
}

/// Uses source time, not callback count. Skipped intervals trigger at most one
/// inference per displayed frame and preserve exponential smoothing duration.
#[derive(Debug, Default)]
struct PreviewClock {
    elapsed: f64,
    lut_at: Option<f64>,
    feature_at: Option<f64>,
}
impl PreviewClock {
    fn advance(&mut self, frames: f64) -> (bool, bool, [f32; 2]) {
        self.elapsed += frames;
        let interval = UPDATE_INTERVAL as f64;
        let update = self
            .lut_at
            .is_none_or(|last| self.elapsed - last >= interval - 1e-6);
        let feature = update
            && self
                .feature_at
                .is_none_or(|last| self.elapsed - last >= 6.0 * interval - 1e-6);
        let previous_weight = self
            .lut_at
            .map_or(0.0, |last| 0.8_f64.powf((self.elapsed - last) / interval));
        if update {
            self.lut_at = Some(self.elapsed);
        }
        if feature {
            self.feature_at = Some(self.elapsed);
        }
        (
            update,
            feature,
            [previous_weight as f32, (1.0 - previous_weight) as f32],
        )
    }
}

fn normalize_image(rgb: &[u8], output: &mut [f32], imagenet: bool) {
    let area = rgb.len() / 3;
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    for (index, pixel) in rgb.chunks_exact(3).enumerate() {
        for c in 0..3 {
            output[c * area + index] = if imagenet {
                // OpenCV convertTo receives double scale/offset derived from
                // float normalization constants, then produces float tensors.
                (f64::from(pixel[c]) / (255.0 * f64::from(STD[c]))
                    - f64::from(MEAN[c]) / f64::from(STD[c])) as f32
            } else {
                f32::from(pixel[c]) / 255.0
            };
        }
    }
}

fn flab(value: f32) -> f32 {
    if value > 0.008_856_452 {
        value.powf(1.0 / 3.0)
    } else {
        value.mul_add(7.787037, 0.13793103)
    }
}
fn iflab(value: f32) -> f32 {
    if value > 0.20689655 {
        value * value * value
    } else {
        value.mul_add(0.12841855, -0.017712904)
    }
}
fn rgb_to_lab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let x = r.mul_add(0.433953, g.mul_add(0.376219, 0.189828 * b));
    let y = r.mul_add(0.212671, g.mul_add(0.715160, 0.072169 * b));
    let z = r.mul_add(0.017758, g.mul_add(0.109477, 0.872765 * b));
    let fy = flab(y);
    [
        116.0_f32.mul_add(fy, -16.0),
        500.0_f32.mul_add(flab(x), -500.0 * fy),
        (-200.0_f32).mul_add(flab(z), 200.0 * fy),
    ]
}
fn lab_to_rgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let fy = l.mul_add(0.00862069, 16.0 * 0.00862069);
    let x = iflab(a.mul_add(0.002, fy));
    let y = iflab(fy);
    let z = iflab(b.mul_add(-0.005, fy));
    [
        x.mul_add(3.0799327, y.mul_add(-1.537_15, -0.542782 * z)),
        x.mul_add(-0.921235, y.mul_add(1.875992, 0.0452442 * z)),
        x.mul_add(0.0528909, y.mul_add(-0.204043, 1.1511515 * z)),
    ]
}
#[cfg(test)]
fn apply_strength(src: [f32; 3], restored: [f32; 3], luminance: f32, total: f32) -> [f32; 3] {
    let src_lab = rgb_to_lab(src);
    let mut lab = rgb_to_lab(restored);
    lab[0] = (lab[0] - src_lab[0]).mul_add(luminance, src_lab[0]);
    let rgb = lab_to_rgb(lab);
    std::array::from_fn(|c| (rgb[c] - src[c]).mul_add(total, src[c]))
}

fn apply_strength_with_luminance(
    src: [f32; 3],
    src_luminance: f32,
    restored: [f32; 3],
    luminance: f32,
    total: f32,
) -> [f32; 3] {
    let mut lab = rgb_to_lab(restored);
    lab[0] = (lab[0] - src_luminance).mul_add(luminance, src_luminance);
    let rgb = lab_to_rgb(lab);
    std::array::from_fn(|c| (rgb[c] - src[c]).mul_add(total, src[c]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_thread_numerical_equivalence(
        one: &AiSession,
        parallel: &AiSession,
        first: &[u8],
        second: &[u8],
        context: &str,
    ) {
        assert_eq!(
            one.frame, parallel.frame,
            "{context}: processed frame count"
        );
        assert_eq!(
            (
                one.preview_clock.elapsed,
                one.preview_clock.lut_at,
                one.preview_clock.feature_at,
            ),
            (
                parallel.preview_clock.elapsed,
                parallel.preview_clock.lut_at,
                parallel.preview_clock.feature_at,
            ),
            "{context}: source-time inference cadence",
        );
        assert!(
            one.style_vector == parallel.style_vector,
            "{context}: selected style vector",
        );
        // MNN can choose different FP32 convolution algorithms for different
        // thread budgets. Retain the independent complete-tensor tolerance.
        for (label, first, second) in [
            ("model output", &one.output, &parallel.output),
            ("smoothed output", &one.smoothed, &parallel.smoothed),
        ] {
            assert_eq!(first.len(), second.len());
            for (index, (&first, &second)) in first.iter().zip(second).enumerate() {
                assert!(
                    (first - second).abs() <= 0.0005 + first.abs() * 0.0005,
                    "{context}: {label} element {index}: {first} versus {second}",
                );
            }
        }
        // Small FP32 differences may straddle a half-byte rounding threshold.
        // The bound covers every LUT channel, not only colors in this image.
        // It is separate from exact CPU/GPU application of the same LUT.
        assert_eq!(one.lut.entries().len(), parallel.lut.entries().len());
        let max_lut_delta = one
            .lut
            .entries()
            .iter()
            .flatten()
            .zip(parallel.lut.entries().iter().flatten())
            .map(|(&first, &second)| first.abs_diff(second))
            .max()
            .unwrap_or(0);
        assert!(
            max_lut_delta <= 1,
            "{context}: LUT channel delta {max_lut_delta}"
        );
        assert_eq!(first.len(), second.len());
        let max_pixel_delta = first
            .iter()
            .zip(second)
            .map(|(&first, &second)| first.abs_diff(second))
            .max()
            .unwrap_or(0);
        assert!(
            max_pixel_delta <= 1,
            "{context}: RGB channel delta {max_pixel_delta}"
        );
    }

    #[test]
    #[ignore = "requires a fresh MNN process; run the isolated shipping check"]
    fn aquavision_thread_budget_preserves_numerical_and_temporal_contracts() {
        let input = |frame: u32| -> Vec<u8> {
            (0..64 * 64)
                .flat_map(|i| {
                    [
                        20 + ((i + frame * 17) % 31) as u8,
                        80 + ((i + frame * 13) % 61) as u8,
                        100 + ((i + frame * 47) % 101) as u8,
                    ]
                })
                .collect()
        };
        for style in 0..4 {
            // Construct the maximum budget first; MNN's shared pool cannot grow.
            let mut parallel =
                AiSession::new_with_threads(64, 64, 1.0, style, &BundledAssetProvider, 4).unwrap();
            let mut one =
                AiSession::new_with_threads(64, 64, 1.0, style, &BundledAssetProvider, 1).unwrap();
            for model in [&parallel.feature_model, &parallel.preset_model] {
                assert_eq!(model.actual_threads().unwrap(), 4);
            }
            for model in [&one.feature_model, &one.preset_model] {
                assert_eq!(model.actual_threads().unwrap(), 1);
            }
            for frame in 0..=61 {
                let mut first = input(frame);
                let mut second = first.clone();
                one.process_rgb8(&mut first).unwrap();
                parallel.process_rgb8(&mut second).unwrap();
                assert_thread_numerical_equivalence(
                    &one,
                    &parallel,
                    &first,
                    &second,
                    &format!("style {style} frame {frame}"),
                );
            }
            one.reset();
            parallel.reset();
            for (index, elapsed) in [1.0, 1.0, 9.0, 2.0, 48.0, 1.0, 25.0]
                .into_iter()
                .enumerate()
            {
                let mut first = input(index as u32);
                let mut second = first.clone();
                one.process_rgb8_continuous(&mut first, elapsed).unwrap();
                parallel
                    .process_rgb8_continuous(&mut second, elapsed)
                    .unwrap();
                assert_thread_numerical_equivalence(
                    &one,
                    &parallel,
                    &first,
                    &second,
                    &format!("style {style} preview {index}"),
                );
            }
        }
    }

    #[test]
    fn cached_identity_luminance_preserves_strength_float_and_byte_results() {
        for x in 0..EDGE {
            for y in 0..EDGE {
                for z in 0..EDGE {
                    let source = [x, y, z].map(|v| (v * 16).min(255) as f32 / 255.0);
                    let cached = rgb_to_lab(source)[0];
                    for restored in [[-0.1, 0.8, 1.2], [0.9, 0.02, 0.3], source] {
                        for strength in [0.0, 0.5, 1.0] {
                            let original = apply_strength(source, restored, 0.5, strength);
                            let actual = apply_strength_with_luminance(
                                source, cached, restored, 0.5, strength,
                            );
                            assert_eq!(actual.map(f32::to_bits), original.map(f32::to_bits));
                            let byte =
                                |v: f32| (v * 255.0).round_ties_even().clamp(0.0, 255.0) as u8;
                            assert_eq!(actual.map(byte), original.map(byte));
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "phase diagnostic; use one thread budget per fresh process with --nocapture"]
    fn benchmark_aquavision_phases() {
        use std::{hint::black_box, time::Instant};
        // Run one budget per fresh process: MNN shares a fixed-size worker pool
        // by CPU mask, so looping from two to four would silently retain two.
        let threads: usize = std::env::var("INSTA360_BENCH_CPU_THREADS")
            .expect("set INSTA360_BENCH_CPU_THREADS to 1, 2 or 4; run each in a fresh process")
            .parse()
            .expect("INSTA360_BENCH_CPU_THREADS must be an integer");
        assert!(matches!(threads, 1 | 2 | 4));
        for (width, height) in [(1280, 640), (2560, 1280)] {
            // Construct the requested CPU budget once; labels never depend
            // on the production default. Initialization is outside timing.
            let mut session =
                AiSession::new_with_threads(width, height, 1.0, 0, &BundledAssetProvider, threads)
                    .unwrap();
            let feature_threads = session.feature_model.actual_threads().unwrap();
            let preset_threads = session.preset_model.actual_threads().unwrap();
            assert_eq!(
                feature_threads, threads,
                "feature model thread budget was clamped"
            );
            assert_eq!(
                preset_threads, threads,
                "preset model thread budget was clamped"
            );
            let input: Vec<u8> = (0..width * height)
                .flat_map(|i| {
                    [
                        20 + (i % 31) as u8,
                        80 + (i % 61) as u8,
                        100 + (i % 101) as u8,
                    ]
                })
                .collect();
            let mut output = input.clone();
            let mut samples: [Vec<f64>; 5] = std::array::from_fn(|_| Vec::new());
            for round in 0..13 {
                output.copy_from_slice(&input);
                let start = Instant::now();
                resize_rgb8(
                    &input,
                    width as usize,
                    height as usize,
                    &mut session.low,
                    224,
                    224,
                );
                let resize_ms = start.elapsed().as_secs_f64() * 1000.0;
                let start = Instant::now();
                session.update_style().unwrap();
                let feature_ms = start.elapsed().as_secs_f64() * 1000.0;
                let start = Instant::now();
                session.infer_lut().unwrap();
                let preset_ms = start.elapsed().as_secs_f64() * 1000.0;
                let start = Instant::now();
                session.update_lut_grid([0.8, 0.2]);
                let grid_ms = start.elapsed().as_secs_f64() * 1000.0;
                let start = Instant::now();
                session.lut.apply_rgb8(&mut output);
                black_box(&output);
                let apply_ms = start.elapsed().as_secs_f64() * 1000.0;
                session.frame += 1;
                if round >= 3 {
                    for (samples, value) in samples
                        .iter_mut()
                        .zip([resize_ms, feature_ms, preset_ms, grid_ms, apply_ms])
                    {
                        samples.push(value);
                    }
                }
            }
            let phases = [
                "resize_224",
                "feature_normalize_infer_match",
                "preset_resize_normalize_infer",
                "smooth_and_lut_grid",
                "apply_lut",
            ];
            for (phase, mut samples) in phases.into_iter().zip(samples) {
                samples.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    serde_json::json!({"benchmark":"aquavision", "cpu_threads":threads,"requested_cpu_threads":threads,"effective_feature_threads":feature_threads,"effective_preset_threads":preset_threads,"width":width,"height":height,"phase":phase,"median_ms":samples[samples.len()/2],"samples_ms":samples})
                );
            }
        }
    }

    #[test]
    fn resizing_ai_retains_models_and_tensors_and_matches_fresh_sessions() {
        use crate::underwater::{Engine, UnderwaterColorSession};
        use crate::{UnderwaterColorMode, UnderwaterColorOptions};
        let options = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Ai,
            ..Default::default()
        };
        let mut session =
            UnderwaterColorSession::prepare(options, 96, 80, 30, 1, &BundledAssetProvider).unwrap();
        let identities = |session: &UnderwaterColorSession| {
            let Engine::Ai(ai) = &session.engine else {
                panic!("AI session");
            };
            [
                session.engine_identity(),
                ai.feature_model.allocation_identity(),
                ai.preset_model.allocation_identity(),
                ai.low.as_ptr() as usize,
                ai.resized.as_ptr() as usize,
                ai.features.as_ptr() as usize,
                ai.image.as_ptr() as usize,
                ai.identity.as_ptr() as usize,
                ai.identity_luminance.as_ptr() as usize,
                ai.output.as_ptr() as usize,
                ai.smoothed.as_ptr() as usize,
                ai.lut.allocation_identity(),
            ]
        };
        let original = identities(&session);
        for (index, (width, height)) in [(96, 80), (64, 65), (96, 80)].into_iter().enumerate() {
            session.resize(width, height).unwrap();
            assert_eq!(identities(&session), original);
            let pixels: Vec<u8> = (0..width * height)
                .flat_map(|i| {
                    [
                        20 + (i % 31) as u8,
                        80 + (i % 61) as u8,
                        100 + (i % 101) as u8,
                    ]
                })
                .collect();
            let mut fresh = UnderwaterColorSession::prepare(
                options,
                width,
                height,
                30,
                1,
                &BundledAssetProvider,
            )
            .unwrap();
            let pts = 1.0 + index as f64 * 2.0;
            let mut actual = pixels.clone();
            let mut expected = pixels.clone();
            session.process_rgb8_continuous(&mut actual, pts).unwrap();
            fresh.process_rgb8_continuous(&mut expected, pts).unwrap();
            assert!(
                actual == expected,
                "AI first output differs at transition {index}"
            );
            assert_eq!(identities(&session), original);
            // Leave real continuous history before the next dimension change.
            let mut changed = pixels;
            for pixel in changed.chunks_exact_mut(3) {
                pixel[0] = 200 - pixel[0];
            }
            session
                .process_rgb8_continuous(&mut changed, pts + 0.4)
                .unwrap();
            let previous_pts = session.previous_pts;
            assert!(session.resize(0, height).is_err());
            assert_eq!(session.previous_pts, previous_pts);
            assert_eq!(identities(&session), original);
        }
    }

    #[test]
    fn verified_models_execute_all_styles_and_reuse_temporal_buffers() {
        // Complete output snapshots, explicitly self-derived: the independent
        // model-adapter reference is tested separately in model.rs.
        let reference = include_bytes!("../../tests/fixtures/underwater-sequence-reference-v1.bin");
        assert_eq!(&reference[..8], b"MNNRGB1\0");
        let mut references = reference[8..].chunks_exact(64 * 64 * 3);
        let mut compare = |actual: &[u8]| {
            let expected = references.next().expect("complete RGB reference");
            let error: u64 = actual
                .iter()
                .zip(expected)
                .map(|(actual, expected)| {
                    let difference = actual.abs_diff(*expected);
                    assert!(
                        difference <= 2,
                        "temporal reference channel changed by {difference}"
                    );
                    u64::from(difference)
                })
                .sum();
            assert!(
                (error as f64 / actual.len() as f64) < 0.05,
                "temporal reference mean error changed"
            );
        };
        let original: Vec<_> = (0..64 * 64)
            .flat_map(|i| {
                [
                    20 + (i % 31) as u8,
                    80 + (i % 61) as u8,
                    100 + (i % 101) as u8,
                ]
            })
            .collect();
        let mut outputs = Vec::new();
        for style in 0..4 {
            let mut session = AiSession::new(64, 64, 1.0, style, &BundledAssetProvider).unwrap();
            let allocations = [
                session.low.as_ptr() as usize,
                session.resized.as_ptr() as usize,
                session.features.as_ptr() as usize,
                session.image.as_ptr() as usize,
                session.output.as_ptr() as usize,
                session.smoothed.as_ptr() as usize,
            ];
            let mut first = original.clone();
            session.process_rgb8(&mut first).unwrap();
            compare(&first);
            assert_ne!(first, original, "style {style} did not restore the image");
            assert!(session.output.iter().all(|value| value.is_finite()));
            let initial_output = session.output.clone();
            for frame in 1..=61 {
                let mut pixels = original.clone();
                if frame >= 10 {
                    for pixel in pixels.chunks_exact_mut(3) {
                        pixel[0] = 200 - pixel[0];
                        pixel[1] /= 2;
                    }
                }
                session.process_rgb8(&mut pixels).unwrap();
                if [9, 10, 59, 60, 61].contains(&frame) {
                    compare(&pixels);
                }
                if frame < 10 {
                    assert_eq!(
                        pixels, first,
                        "LUT must remain unchanged between analysis frames"
                    );
                }
            }
            assert_ne!(
                session.output, initial_output,
                "analysis must rerun on later frames"
            );
            assert_eq!(
                allocations,
                [
                    session.low.as_ptr() as usize,
                    session.resized.as_ptr() as usize,
                    session.features.as_ptr() as usize,
                    session.image.as_ptr() as usize,
                    session.output.as_ptr() as usize,
                    session.smoothed.as_ptr() as usize
                ]
            );
            session.reset();
            let mut reset = original.clone();
            session.process_rgb8(&mut reset).unwrap();
            assert_eq!(reset, first);
            outputs.push(first);
        }
        assert!(
            outputs.windows(2).all(|pair| pair[0] != pair[1]),
            "styles must select distinct trained vectors"
        );
        assert!(references.next().is_none());
    }
    #[test]
    fn public_ai_identity_missing_assets_and_invalid_frames_are_checked() {
        use crate::{
            underwater::UnderwaterColorSession, UnderwaterColorMode, UnderwaterColorOptions,
        };
        let options = UnderwaterColorOptions {
            mode: UnderwaterColorMode::Ai,
            strength: Some(0.0),
            ..Default::default()
        };
        let mut session =
            UnderwaterColorSession::prepare(options, 64, 64, 30, 1, &BundledAssetProvider).unwrap();
        let mut pixels = vec![123; 64 * 64 * 3];
        let original = pixels.clone();
        for pts in [0.0, 1.0, 0.0] {
            session.process_rgb8(&mut pixels, pts).unwrap();
            assert_eq!(pixels, original);
        }
        assert!(session.process_rgb8(&mut pixels, f64::NAN).is_err());
        assert_eq!(pixels, original);
        let provider = crate::assets::InMemoryAssetProvider::default();
        assert!(UnderwaterColorSession::prepare(options, 64, 64, 30, 1, &provider).is_err());
    }
    #[test]
    fn normalization_is_planar_rgb_with_verified_imagenet_constants() {
        let mut output = [0.0; 6];
        normalize_image(&[255, 0, 128, 0, 255, 64], &mut output, true);
        for (actual, expected) in output.into_iter().zip([
            2.2489083,
            -2.117904,
            -2.0357144,
            2.4285715,
            0.42649257,
            -0.68897593,
        ]) {
            assert!((actual - expected).abs() < 0.00001);
        }
    }
    #[test]
    fn strength_zero_is_exact_identity_and_lab_gray_is_preserved() {
        for value in [0.0, 0.02, 0.5, 1.0] {
            let source = [value; 3];
            assert_eq!(apply_strength(source, [0.7, 0.3, 0.1], 0.5, 0.0), source);
            let actual = apply_strength(source, source, 0.5, 1.0);
            for v in actual {
                assert!((v - value).abs() < 0.00001);
            }
        }
    }
    #[test]
    fn bilinear_resize_preserves_constants_and_pixel_centers() {
        let mut output = vec![0; 7 * 9 * 3];
        resize_rgb8(&[12, 34, 56], 1, 1, &mut output, 7, 9);
        assert!(output.chunks_exact(3).all(|v| v == [12, 34, 56]));
        let mut middle = [0; 3];
        resize_rgb8(
            &[0, 0, 0, 100, 100, 100, 200, 200, 200, 100, 100, 100],
            2,
            2,
            &mut middle,
            1,
            1,
        );
        assert_eq!(middle, [100; 3]);
    }
    #[test]
    fn preview_inference_cadence_follows_source_time_and_bounds_skipped_work() {
        let mut clock = PreviewClock::default();
        assert_eq!(clock.advance(1.0), (true, true, [0.0, 1.0]));
        for _ in 0..9 {
            assert!(!clock.advance(1.0).0);
        }
        assert_eq!(clock.advance(1.0), (true, false, [0.8, 0.2]));
        let (update, feature, weights) = clock.advance(50.0);
        assert!(update && feature);
        assert!((weights[0] - 0.8_f32.powi(5)).abs() < 1e-6);
        assert!(!clock.advance(1.0).0);
        let mut equivalent = PreviewClock::default();
        equivalent.advance(1.0);
        for _ in 0..19 {
            equivalent.advance(0.5);
        }
        assert_eq!(equivalent.advance(0.5), (true, false, [0.8, 0.2]));
    }
}
