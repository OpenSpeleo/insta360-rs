//! Studio video-model belt preparation and conversion to the common ray field.
//!
//! The model sees a cylindrical 400-degree belt. Renderers consume only the
//! resulting spherical displacements, so CPU and GPU share this conversion.
use super::{flow, mask::PreparedMasks, StitchSource};
use crate::{
    seam_ai::{SeamFlow, SeamModel},
    Error, FrameMotion, Orientation, ResolvedCalibration, Result,
};
use rayon::prelude::*;
use std::{
    f64::consts::{PI, TAU},
    sync::atomic::{AtomicBool, Ordering},
};

const BELT_WIDTH: usize = 60;
const BELT_HEIGHT: usize = 1080;
const MODEL_WIDTH: usize = 64;
const MODEL_HEIGHT: usize = 544;
const FLOW_WIDTH: usize = 16;
const FLOW_HEIGHT: usize = 136;
const MIN_AZIMUTH: f64 = -110.0;
const AZIMUTH_SPAN: f64 = 400.0;
const FIELD_WIDTH: usize = 512;
const FIELD_HEIGHT: usize = 64;

fn focal() -> f64 {
    (BELT_HEIGHT - 1) as f64 / AZIMUTH_SPAN * 180.0 / PI
}

fn belt_angles(x: f64, y: f64) -> [f64; 2] {
    [
        (MIN_AZIMUTH + y * AZIMUTH_SPAN / (BELT_HEIGHT - 1) as f64).to_radians(),
        PI * 0.5 + ((x - (BELT_WIDTH - 1) as f64 * 0.5) / focal()).atan(),
    ]
}

fn model_position(phi: f64, theta: f64) -> [f64; 2] {
    let source_x = (BELT_WIDTH - 1) as f64 * 0.5 + (theta - PI * 0.5).tan() * focal();
    let source_y = (phi.to_degrees() - MIN_AZIMUTH) / AZIMUTH_SPAN * (BELT_HEIGHT - 1) as f64;
    [
        (source_x + 0.5) * MODEL_WIDTH as f64 / BELT_WIDTH as f64 - 0.5,
        (source_y + 0.5) * MODEL_HEIGHT as f64 / BELT_HEIGHT as f64 - 0.5,
    ]
}

fn model_angles(x: f64, y: f64) -> [f64; 2] {
    belt_angles(
        (x + 0.5) * BELT_WIDTH as f64 / MODEL_WIDTH as f64 - 0.5,
        (y + 0.5) * BELT_HEIGHT as f64 / MODEL_HEIGHT as f64 - 0.5,
    )
}

fn sample(values: &[f32], width: usize, height: usize, x: f64, y: f64) -> Option<f32> {
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x > (width - 1) as f64
        || y > (height - 1) as f64
    {
        return None;
    }
    let (xx, yy) = (x.floor() as usize, y.floor() as usize);
    let (right, down) = ((xx + 1).min(width - 1), (yy + 1).min(height - 1));
    let (dx, dy) = ((x - xx as f64) as f32, (y - yy as f64) as f32);
    Some(
        (values[yy * width + xx] * (1.0 - dx) + values[yy * width + right] * dx) * (1.0 - dy)
            + (values[down * width + xx] * (1.0 - dx) + values[down * width + right] * dx) * dy,
    )
}

/// OpenCV's resize convention: destination pixel center maps through the size
/// ratio, followed by border replication. Quantize the intermediate CV_8UC1.
fn resize_byte_plane(source: &[f32]) -> Vec<f32> {
    (0..MODEL_WIDTH * MODEL_HEIGHT)
        .into_par_iter()
        .map(|i| {
            let x = (((i % MODEL_WIDTH) as f64 + 0.5) * BELT_WIDTH as f64 / MODEL_WIDTH as f64
                - 0.5)
                .clamp(0.0, (BELT_WIDTH - 1) as f64);
            let y = (((i / MODEL_WIDTH) as f64 + 0.5) * BELT_HEIGHT as f64 / MODEL_HEIGHT as f64
                - 0.5)
                .clamp(0.0, (BELT_HEIGHT - 1) as f64);
            sample(source, BELT_WIDTH, BELT_HEIGHT, x, y)
                .expect("resize samples are clamped to the nonempty source plane")
                .round()
                .clamp(0.0, 255.0)
        })
        .collect()
}

#[derive(Debug)]
struct Inputs {
    gray: [Vec<f32>; 2],
    masks: [Vec<f32>; 2],
}

#[allow(clippy::too_many_arguments)]
fn inputs(
    sources: &[StitchSource<'_>; 2],
    calibration: &ResolvedCalibration,
    motion: &FrameMotion,
    masks: &PreparedMasks,
    basis: Orientation,
    cancel: &AtomicBool,
) -> Result<Inputs> {
    let geometry = super::resolved_render_geometry(calibration)?;
    let planes: [(Vec<f32>, Vec<f32>); 2] = std::array::from_fn(|lens| {
        let mut gray = vec![0.0; BELT_WIDTH * BELT_HEIGHT];
        let mut coverage = vec![0.0; BELT_WIDTH * BELT_HEIGHT];
        gray.par_iter_mut()
            .zip(coverage.par_iter_mut())
            .enumerate()
            .for_each(|(i, (gray, coverage))| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let [phi, theta] = belt_angles((i % BELT_WIDTH) as f64, (i / BELT_WIDTH) as f64);
                let direction = basis.inverse().rotate_vector([
                    theta.sin() * phi.cos(),
                    theta.sin() * phi.sin(),
                    theta.cos(),
                ]);
                if let Some(sample) = super::project_and_sample(
                    &sources[lens],
                    &calibration.lenses[lens],
                    lens,
                    direction,
                    0.08,
                    masks[lens].as_ref(),
                    geometry[lens],
                    motion.readout()[lens].as_ref(),
                ) {
                    *gray = (0.299 * sample.color[0]
                        + 0.587 * sample.color[1]
                        + 0.114 * sample.color[2])
                        .round()
                        .clamp(0.0, 255.0) as f32;
                    if sample.illumination_weight > 0.01 {
                        *coverage = 255.0;
                    }
                }
            });
        (
            resize_byte_plane(&gray),
            resize_byte_plane(&coverage)
                .into_par_iter()
                .map(|v| v / 255.0)
                .collect(),
        )
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let [(first, mask_first), (second, mask_second)] = planes;
    Ok(Inputs {
        gray: [first, second],
        masks: [mask_first, mask_second],
    })
}

fn flow_at(values: &[f32], x: f64, y: f64) -> Option<[f64; 2]> {
    let (x, y) = ((x + 0.5) * 0.25 - 0.5, (y + 0.5) * 0.25 - 0.5);
    Some([
        f64::from(sample(values, FLOW_WIDTH, FLOW_HEIGHT, x, y)?) * 4.0,
        f64::from(sample(
            &values[FLOW_WIDTH * FLOW_HEIGHT..],
            FLOW_WIDTH,
            FLOW_HEIGHT,
            x,
            y,
        )?) * 4.0,
    ])
}

fn candidate(inputs: &Inputs, result: &SeamFlow, phi: f64, theta: f64) -> Option<[f64; 3]> {
    let [x, y] = model_position(phi, theta);
    let [dx, dy] = flow_at(&result.flow_f, x, y)?;
    // A model proposal is not itself confidence: require source support,
    // round-trip agreement, local texture and an improved photometric match.
    if dx.abs().max(dy.abs()) > 16.0 {
        return None;
    }
    let [bx, by] = flow_at(&result.flow_b, x + dx, y + dy)?;
    if (dx + bx).hypot(dy + by) > 3.0 {
        return None;
    }
    let mut original = 0.0;
    let mut warped = 0.0;
    let mut mean = 0.0;
    let mut squared = 0.0;
    for oy in [-2.0, 0.0, 2.0] {
        for ox in [-2.0, 0.0, 2.0] {
            let coverage = |lens: usize, xx, yy| {
                sample(
                    inputs.masks[lens].as_slice(),
                    MODEL_WIDTH,
                    MODEL_HEIGHT,
                    xx,
                    yy,
                )
            };
            if coverage(0, x + ox, y + oy)? < 0.99
                || coverage(1, x + ox, y + oy)? < 0.99
                || coverage(1, x + dx + ox, y + dy + oy)? < 0.99
            {
                return None;
            }
            let a = f64::from(sample(
                &inputs.gray[0],
                MODEL_WIDTH,
                MODEL_HEIGHT,
                x + ox,
                y + oy,
            )?);
            let b = f64::from(sample(
                &inputs.gray[1],
                MODEL_WIDTH,
                MODEL_HEIGHT,
                x + ox,
                y + oy,
            )?);
            let shifted = f64::from(sample(
                &inputs.gray[1],
                MODEL_WIDTH,
                MODEL_HEIGHT,
                x + dx + ox,
                y + dy + oy,
            )?);
            original += (a - b).abs();
            warped += (a - shifted).abs();
            mean += a;
            squared += a * a;
        }
    }
    let variance = squared / 9.0 - (mean / 9.0).powi(2);
    if variance < 9.0 || warped > (original * 0.95).max(18.0) {
        return None;
    }
    let [new_phi, new_theta] = model_angles(x + dx, y + dy);
    // The repeated 40 degrees provide two estimates around the cut. Fade the
    // outer 20 degrees and average duplicates instead of switching at a row.
    let edge_distance =
        (phi.to_degrees() - MIN_AZIMUTH).min(MIN_AZIMUTH + AZIMUTH_SPAN - phi.to_degrees());
    let weight = (edge_distance / 20.0).clamp(0.0, 1.0);
    Some([new_phi - phi, new_theta - theta, weight])
}

fn convert(inputs: &Inputs, result: &SeamFlow, half_height: f64) -> flow::Field {
    let vectors = (0..FIELD_WIDTH * FIELD_HEIGHT)
        .into_par_iter()
        .map(|i| {
            let phi = ((i % FIELD_WIDTH) as f64 + 0.5) / FIELD_WIDTH as f64 * TAU;
            let theta = PI * 0.5
                + (((i / FIELD_WIDTH) as f64 + 0.5) / FIELD_HEIGHT as f64 * 2.0 - 1.0)
                    * half_height;
            let wrapped = (phi.to_degrees() - MIN_AZIMUTH).rem_euclid(360.0) + MIN_AZIMUTH;
            let mut sum = [0.0; 3];
            for angle in [wrapped, wrapped + 360.0] {
                if angle > MIN_AZIMUTH + AZIMUTH_SPAN {
                    continue;
                }
                if let Some([dx, dy, weight]) = candidate(inputs, result, angle.to_radians(), theta)
                {
                    sum[0] += dx * weight;
                    sum[1] += dy * weight;
                    sum[2] += weight;
                }
            }
            if sum[2] < 0.25 {
                return flow::Vector::default();
            }
            flow::Vector {
                dx: (sum[0] / sum[2] * FIELD_WIDTH as f64 / TAU) as f32,
                dy: (sum[1] / sum[2] * FIELD_HEIGHT as f64 / (2.0 * half_height)) as f32,
                confidence: 1.0,
            }
        })
        .collect();
    flow::Field {
        width: FIELD_WIDTH,
        height: FIELD_HEIGHT,
        vectors,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    model: &mut SeamModel,
    sources: &[StitchSource<'_>; 2],
    calibration: &ResolvedCalibration,
    motion: &FrameMotion,
    masks: &PreparedMasks,
    basis: Orientation,
    half_height: f64,
    cancel: &AtomicBool,
) -> Result<flow::Field> {
    let inputs = inputs(sources, calibration, motion, masks, basis, cancel)?;
    let first = inputs.gray[0].repeat(3);
    let second = inputs.gray[1].repeat(3);
    let result = model.infer(&first, &second, &inputs.masks[0], &inputs.masks[1])?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let field = convert(&inputs, &result, half_height);
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_model_translation_produces_a_consistent_spherical_correction() {
        let first = crate::seam_ai::tests::input(0, 0);
        let second = crate::seam_ai::tests::input(-4, 0);
        let mask = vec![1.0; MODEL_WIDTH * MODEL_HEIGHT];
        let mut model = SeamModel::new().unwrap();
        let result = model.infer(&first, &second, &mask, &mask).unwrap();
        let inputs = Inputs {
            gray: [first[..mask.len()].to_vec(), second[..mask.len()].to_vec()],
            masks: [mask.clone(), mask],
        };
        let field = convert(&inputs, &result, 10_f64.to_radians());
        let mut accepted: Vec<_> = field
            .vectors
            .iter()
            .filter(|v| v.confidence >= 0.5)
            .map(|v| v.dy)
            .collect();
        assert!(
            accepted.len() > field.vectors.len() / 10,
            "actual inference must yield meaningful accepted coverage: {}/{}",
            accepted.len(),
            field.vectors.len()
        );
        accepted.sort_by(f32::total_cmp);
        // Second image moves left: lens1 must be sampled at smaller cylinder
        // x and smaller theta. This checks infer→confidence→field, not a
        // manufactured flow. Independent tensor values are checked separately.
        assert!(accepted[accepted.len() / 2] < -1.0);
    }

    #[test]
    fn cylindrical_coordinates_and_resize_centers_have_independent_expectations() {
        let [phi, theta] = belt_angles(29.5, 0.0);
        assert!((phi.to_degrees() + 110.0).abs() < 1e-12);
        assert!((theta.to_degrees() - 90.0).abs() < 1e-12);
        let [phi, theta] = belt_angles(29.5, 1079.0);
        assert!((phi.to_degrees() - 290.0).abs() < 1e-12);
        assert!((theta.to_degrees() - 90.0).abs() < 1e-12);
        // A unit X/radial ratio is a 45-degree departure from the seam plane.
        assert!((belt_angles(29.5 + focal(), 539.5)[1].to_degrees() - 135.0).abs() < 1e-12);
        assert_eq!(model_position(90_f64.to_radians(), PI * 0.5), [31.5, 271.5]);
        assert!((model_angles(31.5, 271.5)[0].to_degrees() - 90.0).abs() < 1e-12);
        let row_ramp: Vec<_> = (0..BELT_WIDTH * BELT_HEIGHT)
            .map(|i| (i % BELT_WIDTH) as f32)
            .collect();
        let resized = resize_byte_plane(&row_ramp);
        assert_eq!(resized[0], 0.0);
        assert_eq!(resized[63], 59.0);
        assert_eq!(resized[32], 30.0);
    }

    #[test]
    fn quarter_grid_flow_maps_to_input_pixels_and_preserves_xy_order() {
        let mut values = vec![1.0; FLOW_WIDTH * FLOW_HEIGHT];
        values.extend(vec![-2.0; FLOW_WIDTH * FLOW_HEIGHT]);
        assert_eq!(flow_at(&values, 31.5, 271.5), Some([4.0, -8.0]));
        assert!(flow_at(&values, -1.0, 271.5).is_none());
        let start = model_angles(31.5, 271.5);
        let end = model_angles(35.5, 263.5);
        assert!(end[1] > start[1]);
        assert!(
            (end[0] - start[0] - (-8.0 * 1080.0 / 544.0 * 400.0 / 1079.0_f64).to_radians()).abs()
                < 1e-12
        );
    }

    #[test]
    fn identity_flow_and_occluded_source_never_create_displacement() {
        let plane: Vec<_> = (0..MODEL_WIDTH * MODEL_HEIGHT)
            .map(|i| ((i % MODEL_WIDTH) * 4) as f32)
            .collect();
        let mut inputs = Inputs {
            gray: [plane.clone(), plane],
            masks: [
                vec![1.0; MODEL_WIDTH * MODEL_HEIGHT],
                vec![1.0; MODEL_WIDTH * MODEL_HEIGHT],
            ],
        };
        let result = SeamFlow {
            flow_f: vec![0.0; 2 * FLOW_WIDTH * FLOW_HEIGHT],
            flow_b: vec![0.0; 2 * FLOW_WIDTH * FLOW_HEIGHT],
        };
        let field = convert(&inputs, &result, 10_f64.to_radians());
        assert!(field.vectors.iter().any(|v| v.confidence > 0.5));
        assert!(field
            .vectors
            .iter()
            .all(|v| v.dx.abs() < 1e-8 && v.dy.abs() < 1e-8));
        inputs.masks[1].fill(0.0);
        assert!(convert(&inputs, &result, 10_f64.to_radians())
            .vectors
            .iter()
            .all(|v| v.confidence == 0.0));
    }

    #[test]
    fn known_translation_improves_match_and_inconsistent_backward_flow_is_rejected() {
        let first: Vec<_> = (0..MODEL_WIDTH * MODEL_HEIGHT)
            .map(|i| ((i % MODEL_WIDTH) * 4) as f32)
            .collect();
        let second: Vec<_> = (0..MODEL_WIDTH * MODEL_HEIGHT)
            .map(|i| (i % MODEL_WIDTH).saturating_sub(4) as f32 * 4.0)
            .collect();
        let inputs = Inputs {
            gray: [first, second],
            masks: [
                vec![1.0; MODEL_WIDTH * MODEL_HEIGHT],
                vec![1.0; MODEL_WIDTH * MODEL_HEIGHT],
            ],
        };
        let mut result = SeamFlow {
            flow_f: vec![0.0; 2 * FLOW_WIDTH * FLOW_HEIGHT],
            flow_b: vec![0.0; 2 * FLOW_WIDTH * FLOW_HEIGHT],
        };
        result.flow_f[..FLOW_WIDTH * FLOW_HEIGHT].fill(1.0);
        result.flow_b[..FLOW_WIDTH * FLOW_HEIGHT].fill(-1.0);
        let value = candidate(&inputs, &result, 90_f64.to_radians(), PI * 0.5).unwrap();
        let expected = ((4.0 * 60.0 / 64.0) / focal()).atan();
        assert!(value[0].abs() < 1e-12);
        assert!((value[1] - expected).abs() < 1e-12);
        result.flow_b.fill(0.0);
        assert!(candidate(&inputs, &result, 90_f64.to_radians(), PI * 0.5).is_none());
    }
}
