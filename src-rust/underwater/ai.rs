//! Independent MNN execution of the verified Studio neural restoration graph.
use super::{
    ilut::IntegerLut,
    model::Model,
    style::{Database, Style},
};
use crate::{
    assets::{AssetPolicy, AssetProvider, BundledAssetProvider},
    Result,
};
use rayon::prelude::*;

const EDGE: usize = 17;
const ENTRIES: usize = EDGE * EDGE * EDGE;
const UPDATE_INTERVAL: u64 = 10;

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
    output: Vec<f32>,
    smoothed: Vec<f32>,
    style_vector: [f32; 256],
    lut: IntegerLut,
    frame: u64,
}
impl AiSession {
    pub(super) fn new(
        width: u32,
        height: u32,
        strength: f32,
        style_index: u32,
        provider: &dyn AssetProvider,
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
        let preset_model = Model::open(&original, 197)?;
        drop(original);
        let feature_model = Model::open(&load("underwater-model198"), 198)?;
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
            output: vec![0.0; ENTRIES * 3],
            smoothed: vec![0.0; ENTRIES * 3],
            style_vector: [0.0; 256],
            lut: IntegerLut::from_grid(16, 17, vec![[0; 3]; ENTRIES])?,
            frame: 0,
        })
    }

    pub(super) fn reset(&mut self) {
        self.frame = 0;
    }

    pub(super) fn process_rgb8(&mut self, pixels: &mut [u8]) -> Result<()> {
        if self.strength == 0.0 {
            return Ok(());
        }
        if self.frame.is_multiple_of(UPDATE_INTERVAL) {
            resize_rgb8(pixels, self.width, self.height, &mut self.low, 224, 224);
            if self.frame.is_multiple_of(6 * UPDATE_INTERVAL) {
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
            }
            resize_rgb8(&self.low, 224, 224, &mut self.resized, 256, 256);
            normalize_image(&self.resized, &mut self.image, false);
            self.preset_model.run(
                &self.identity,
                &self.image,
                &self.style_vector,
                &mut self.output,
            )?;
            if self.frame == 0 {
                self.smoothed.copy_from_slice(&self.output);
            } else {
                for (old, new) in self.smoothed.iter_mut().zip(&self.output) {
                    *old = 0.8 * *old + 0.2 * *new;
                }
            }
            let strength = self.strength;
            let smoothed = &self.smoothed;
            let identity = &self.identity;
            self.lut.update_grid(|index| {
                let src = std::array::from_fn(|c| identity[c * ENTRIES + index]);
                let restored = std::array::from_fn(|c| smoothed[c * ENTRIES + index]);
                apply_strength(src, restored, 0.5, strength)
                    .map(|v| (v * 255.0).round_ties_even().clamp(0.0, 255.0) as u8)
            });
        }
        pixels.par_chunks_exact_mut(3).for_each(|pixel| {
            pixel.copy_from_slice(&self.lut.sample([pixel[0], pixel[1], pixel[2]]));
        });
        self.frame = self.frame.wrapping_add(1);
        Ok(())
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

/// Pixel-center bilinear RGB8 resize with OpenCV's 11-bit interpolation weights.
fn resize_rgb8(
    source: &[u8],
    width: usize,
    height: usize,
    dest: &mut [u8],
    out_width: usize,
    out_height: usize,
) {
    let weights = |position: usize, input: usize, output: usize| {
        let location = ((position as f64 + 0.5) * input as f64 / output as f64 - 0.5).max(0.0);
        let base = location.floor() as usize;
        let fraction = if base >= input - 1 {
            0.0
        } else {
            (location - base as f64) as f32
        };
        (
            base.min(input - 1),
            (base + 1).min(input - 1),
            ((1.0 - fraction) * 2048.0).round_ties_even() as i32,
            (fraction * 2048.0).round_ties_even() as i32,
        )
    };
    for y in 0..out_height {
        let (y0, y1, b0, b1) = weights(y, height, out_height);
        for x in 0..out_width {
            let (x0, x1, a0, a1) = weights(x, width, out_width);
            for c in 0..3 {
                let top = i32::from(source[(y0 * width + x0) * 3 + c]) * a0
                    + i32::from(source[(y0 * width + x1) * 3 + c]) * a1;
                let bottom = i32::from(source[(y1 * width + x0) * 3 + c]) * a0
                    + i32::from(source[(y1 * width + x1) * 3 + c]) * a1;
                dest[(y * out_width + x) * 3 + c] =
                    ((top * b0 + bottom * b1 + (1 << 21)) >> 22).clamp(0, 255) as u8;
            }
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
fn apply_strength(src: [f32; 3], restored: [f32; 3], luminance: f32, total: f32) -> [f32; 3] {
    let src_lab = rgb_to_lab(src);
    let mut lab = rgb_to_lab(restored);
    lab[0] = (lab[0] - src_lab[0]).mul_add(luminance, src_lab[0]);
    let rgb = lab_to_rgb(lab);
    std::array::from_fn(|c| (rgb[c] - src[c]).mul_add(total, src[c]))
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
