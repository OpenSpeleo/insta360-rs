use insta360_rs::{LensFrame, ResolvedCalibration};

/// Forward-generated spherical texture through an independently inverted
/// equidistant camera. Only the second image contains an angular offset.
pub fn chart(shift_degrees: f64) -> ([LensFrame; 2], ResolvedCalibration) {
    let size = 256;
    let calibration =
        insta360_rs::calibration::synthetic_dual_fisheye_calibration(size, size).unwrap();
    let frames = std::array::from_fn(|index| {
        let lens = &calibration.lenses[index];
        let mut rgb = Vec::with_capacity(size as usize * size as usize * 3);
        for y in 0..size {
            for x in 0..size {
                let nx = (f64::from(x) - f64::from(size) * 0.5) / lens.fx;
                let ny = -(f64::from(y) - f64::from(size) * 0.5) / lens.fy;
                let theta = nx.hypot(ny);
                let local = if theta < 1e-9 {
                    [0.0, 0.0, 1.0]
                } else {
                    [
                        theta.sin() * nx / theta,
                        theta.sin() * ny / theta,
                        theta.cos(),
                    ]
                };
                let body = lens.orientation.inverse().rotate_vector(local);
                let phi = body[1].atan2(body[0])
                    - if index == 1 {
                        shift_degrees.to_radians()
                    } else {
                        0.0
                    };
                let theta = body[0].hypot(body[1]).atan2(body[2]);
                let value = 0.5
                    + 0.14 * (17.0 * phi).sin()
                    + 0.13 * (43.0 * phi + 19.0 * theta).cos()
                    + 0.1 * (14.0 * theta).sin();
                rgb.extend([(value * 255.0).round().clamp(0.0, 255.0) as u8; 3]);
            }
        }
        LensFrame::new(size, size, rgb).unwrap()
    });
    (frames, calibration)
}
