//! Opt-in source ownership diagnostics using the production calibration and masks.
//!
//! These images explain pixel selection; agreement with them is not independent
//! evidence that a physical housing contour is correct.

use super::{
    equirectangular_direction, frame_buffer_len, project_and_sample, resolved_render_geometry,
    LensFrame, MaskCache, PanoramaFrame, DEFAULT_FEATHER_FRACTION,
};
use crate::{EquirectangularProjection, ResolvedCalibration, Result};

/// Unblended views and source support, before motion, LUTs or seam optimization.
pub struct StitchDiagnostics {
    /// Grayscale source support: black is excluded, white is fully supported.
    pub source_masks: [LensFrame; 2],
    /// Each independently projected lens; excluded output is black.
    pub projected: [PanoramaFrame; 2],
    /// Red/green are normalized first/second-lens detail ownership; blue is uncovered.
    pub ownership: PanoramaFrame,
    /// Red/green are the unnormalized first/second-lens detail weights.
    pub detail_weights: PanoramaFrame,
    /// Red/green are the unnormalized first/second-lens illumination weights.
    pub illumination_weights: PanoramaFrame,
}

/// Computes diagnostic images from exactly the same source geometry as stitching.
/// This allocation-heavy operation is intended for explicit developer inspection.
pub fn inspect(
    lenses: &[LensFrame; 2],
    calibration: &ResolvedCalibration,
    projection: EquirectangularProjection,
) -> Result<StitchDiagnostics> {
    projection.validate()?;
    calibration.validate_for_stitching()?;
    let masks = MaskCache::default().prepare(
        lenses
            .each_ref()
            .map(|frame| (frame.width(), frame.height())),
        calibration,
    )?;
    let geometry = resolved_render_geometry(calibration)?;
    let source_mask = |index: usize| -> Result<LensFrame> {
        let frame = &lenses[index];
        let len = frame_buffer_len(frame.width(), frame.height())?;
        let rgb = (0..len / 3)
            .flat_map(|pixel| {
                let value = masks[index].as_ref().map_or(255, |mask| {
                    (mask.weights[pixel] * 255.0).round().clamp(0.0, 255.0) as u8
                });
                [value; 3]
            })
            .collect();
        LensFrame::new(frame.width(), frame.height(), rgb)
    };
    let source_masks = [source_mask(0)?, source_mask(1)?];
    let len = frame_buffer_len(projection.width, projection.height)?;
    let mut projected = [vec![0_u8; len], vec![0_u8; len]];
    let mut ownership = vec![0_u8; len];
    let mut detail_weights = vec![0_u8; len];
    let mut illumination_weights = vec![0_u8; len];
    let byte = |value: f64| (value * 255.0).round().clamp(0.0, 255.0) as u8;
    for row in 0..projection.height as usize {
        for column in 0..projection.width as usize {
            let direction = equirectangular_direction(
                column,
                row,
                projection.width as usize,
                projection.height as usize,
            );
            let at = (row * projection.width as usize + column) * 3;
            let mut detail = [0.0; 2];
            for lens in 0..2 {
                if let Some(sample) = project_and_sample(
                    &lenses[lens],
                    &calibration.lenses[lens],
                    lens,
                    direction,
                    DEFAULT_FEATHER_FRACTION,
                    masks[lens].as_ref(),
                    geometry[lens],
                    None,
                ) {
                    for channel in 0..3 {
                        projected[lens][at + channel] =
                            sample.color[channel].round().clamp(0.0, 255.0) as u8;
                    }
                    detail[lens] = sample.detail_weight;
                    detail_weights[at + lens] = byte(sample.detail_weight);
                    illumination_weights[at + lens] = byte(sample.illumination_weight);
                }
            }
            let total = detail[0] + detail[1];
            if total > 0.0 {
                ownership[at] = byte(detail[0] / total);
                ownership[at + 1] = byte(detail[1] / total);
            } else {
                ownership[at + 2] = 255;
            }
        }
    }
    let [first, second] = projected;
    let panorama = |rgb| PanoramaFrame::new(projection.width, projection.height, rgb);
    Ok(StitchDiagnostics {
        source_masks,
        projected: [panorama(first)?, panorama(second)?],
        ownership: panorama(ownership)?,
        detail_weights: panorama(detail_weights)?,
        illumination_weights: panorama(illumination_weights)?,
    })
}
