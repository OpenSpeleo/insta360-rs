//! Exact neural preprocessing shared by CPU and GPU analysis.

pub(crate) fn weights(position: usize, input: usize, output: usize) -> (usize, usize, i32, i32) {
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
}

/// Pixel-center bilinear RGB8 resize with OpenCV's 11-bit interpolation weights.
#[cfg(any(feature = "underwater-ai", test))]
pub(crate) fn rgb8(
    source: &[u8],
    width: usize,
    height: usize,
    dest: &mut [u8],
    out_width: usize,
    out_height: usize,
) {
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
