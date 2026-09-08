//! Forward-generated sensor observations, independent of renderer projection.

use std::f64::consts::{PI, TAU};

use insta360_rs::calibration::synthetic_dual_fisheye_calibration;
use insta360_rs::motion::{FrameMotion, ReadoutDirection, ReadoutPoseTable};
use insta360_rs::{
    EquirectangularProjection, LensFrame, Orientation, PanoramaFrame, ResolvedCalibration,
};

#[derive(Clone, Copy)]
pub struct Scan {
    pub direction: ReadoutDirection,
    pub crop: [f64; 2],
    pub angle: f64,
    pub phase: f64,
}

pub struct Chart {
    pub lenses: [LensFrame; 2],
    pub calibration: ResolvedCalibration,
    pub projection: EquirectangularProjection,
    pub motion: FrameMotion,
    pub expected: Vec<u8>,
}

pub fn scans(columns: bool) -> [Scan; 2] {
    if columns {
        [
            Scan {
                direction: ReadoutDirection::LeftToRight,
                crop: [0.13, 0.78],
                angle: 0.55,
                phase: 0.01,
            },
            Scan {
                direction: ReadoutDirection::RightToLeft,
                crop: [0.22, 0.95],
                angle: -0.42,
                phase: -0.03,
            },
        ]
    } else {
        [
            Scan {
                direction: ReadoutDirection::TopToBottom,
                crop: [0.0, 1.0],
                angle: 0.45,
                phase: 0.0,
            },
            Scan {
                direction: ReadoutDirection::BottomToTop,
                crop: [0.0, 1.0],
                angle: -0.35,
                phase: 0.03,
            },
        ]
    }
}

/// Generates each sensor pixel by tracing its known equidistant ray outward at
/// that pixel's capture time. This is a forward observation model: no renderer
/// projection, iterative solver, or pose-table lookup contributes expected data.
pub fn chart(scans: [Scan; 2], reference_degrees: [f64; 3], target_degrees: [f64; 3]) -> Chart {
    let size = 256_u32;
    let focal = f64::from(size) / (PI * 1.1);
    let axis = [
        0.3 / 0.94_f64.sqrt(),
        -0.6 / 0.94_f64.sqrt(),
        0.7 / 0.94_f64.sqrt(),
    ];
    let lenses = std::array::from_fn(|lens| {
        let scan = scans[lens];
        let mut rgb = Vec::with_capacity(size as usize * size as usize * 3);
        for y in 0..size {
            for x in 0..size {
                let dx = f64::from(x) - f64::from(size) * 0.5;
                let dy = f64::from(size) * 0.5 - f64::from(y);
                let radial = dx.hypot(dy);
                let theta = radial / focal;
                let scale = if radial > 0.0 {
                    theta.sin() / radial
                } else {
                    0.0
                };
                let local = [dx * scale, dy * scale, theta.cos()];
                // Synthetic back lens has a known 180-degree mounting about Y.
                let body = if lens == 0 {
                    local
                } else {
                    [-local[0], local[1], -local[2]]
                };
                let coordinate = match scan.direction {
                    ReadoutDirection::TopToBottom | ReadoutDirection::BottomToTop => y,
                    ReadoutDirection::LeftToRight | ReadoutDirection::RightToLeft => x,
                };
                let mut time = scan.crop[0]
                    + (f64::from(coordinate) + 0.5) / f64::from(size)
                        * (scan.crop[1] - scan.crop[0]);
                if matches!(
                    scan.direction,
                    ReadoutDirection::BottomToTop | ReadoutDirection::RightToLeft
                ) {
                    time = 1.0 - time;
                }
                let angle = scan.phase + scan.angle * (time - 0.5);
                let world = rotate_euler(rotate_axis(body, axis, angle), reference_degrees);
                rgb.extend(encode(world));
            }
        }
        LensFrame::new(size, size, rgb).expect("forward-generated sensor image")
    });
    let tables = scans.map(|scan| {
        Some(
            ReadoutPoseTable::new(
                scan.direction,
                scan.crop,
                (0..=64)
                    .map(|i| {
                        Orientation::from_axis_angle(
                            axis,
                            -(scan.phase + scan.angle * (i as f64 / 64.0 - 0.5)),
                        )
                        .expect("analytical rotation")
                    })
                    .collect(),
            )
            .expect("bounded readout table"),
        )
    });
    let reference = Orientation::from_euler_degrees(
        reference_degrees[0],
        reference_degrees[1],
        reference_degrees[2],
    )
    .expect("reference pose");
    let target =
        Orientation::from_euler_degrees(target_degrees[0], target_degrees[1], target_degrees[2])
            .expect("target pose");
    let motion = FrameMotion::new(target.inverse() * reference, tables).expect("frame motion");
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let mut expected =
        Vec::with_capacity(projection.width as usize * projection.height as usize * 3);
    for row in 0..projection.height {
        let polar = (f64::from(row) + 0.5) * PI / f64::from(projection.height);
        for column in 0..projection.width {
            let longitude = (f64::from(column) + 0.5) * TAU / f64::from(projection.width);
            let ray = [
                polar.sin() * longitude.cos(),
                -polar.sin() * longitude.sin(),
                polar.cos(),
            ];
            expected.extend(encode(rotate_euler(ray, target_degrees)));
        }
    }
    Chart {
        lenses,
        calibration: synthetic_dual_fisheye_calibration(size, size).expect("synthetic intrinsics"),
        projection,
        motion,
        expected,
    }
}

/// Error against independent world-direction colors, in 8-bit code values.
pub fn error(panorama: &PanoramaFrame, expected: &[u8]) -> (f64, u8) {
    assert_eq!(panorama.as_rgb8().len(), expected.len());
    let differences = panorama
        .as_rgb8()
        .iter()
        .zip(expected)
        .map(|(actual, expected)| actual.abs_diff(*expected));
    let (sum, maximum) = differences.fold((0_u64, 0_u8), |(sum, maximum), value| {
        (sum + u64::from(value), maximum.max(value))
    });
    (sum as f64 / expected.len() as f64, maximum)
}

fn encode(ray: [f64; 3]) -> [u8; 3] {
    ray.map(|component| (128.0 + 100.0 * component).round() as u8)
}

// Rodrigues rotation used only in the independent forward model.
fn rotate_axis(ray: [f64; 3], axis: [f64; 3], angle: f64) -> [f64; 3] {
    let (sin, cos) = angle.sin_cos();
    let cross = [
        axis[1] * ray[2] - axis[2] * ray[1],
        axis[2] * ray[0] - axis[0] * ray[2],
        axis[0] * ray[1] - axis[1] * ray[0],
    ];
    let dot = axis.iter().zip(ray).map(|(a, b)| a * b).sum::<f64>();
    std::array::from_fn(|i| ray[i] * cos + cross[i] * sin + axis[i] * dot * (1.0 - cos))
}

fn rotate_euler(ray: [f64; 3], degrees: [f64; 3]) -> [f64; 3] {
    let x = rotate_axis(ray, [1.0, 0.0, 0.0], degrees[0].to_radians());
    let y = rotate_axis(x, [0.0, 1.0, 0.0], degrees[1].to_radians());
    rotate_axis(y, [0.0, 0.0, 1.0], degrees[2].to_radians())
}
