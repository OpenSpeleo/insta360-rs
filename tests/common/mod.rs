use insta360_rs::container::EmbeddedOffset;
use insta360_rs::{CalibrationResolver, LensFrame, OpticalSetup, ResolvedCalibration};

const X5_DIVING_WATER_LENS_TYPE: u32 = 117;
const OFFSET_FLAGS: u32 = 0x400;

pub fn x5_v6_underwater_calibration(width: u32, height: u32) -> ResolvedCalibration {
    let canvas_width = width * 2;
    let focal = f64::from(width) * 0.86;
    let lens = |center_x: f64| {
        let mut values = vec![
            2.0,
            focal,
            focal,
            center_x,
            f64::from(height) * 0.5,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        ];
        values.extend([0.0; 13]);
        values.extend([
            f64::from(canvas_width),
            f64::from(height),
            f64::from(X5_DIVING_WATER_LENS_TYPE),
        ]);
        values
    };
    let mut fields = vec!["2".to_owned()];
    for value in lens(f64::from(width) * 0.5)
        .into_iter()
        .chain(lens(f64::from(width) * 1.5))
    {
        fields.push(value.to_string());
    }
    fields.push(((6_u32 << 16) | OFFSET_FLAGS).to_string());
    let offset = EmbeddedOffset {
        version: 6,
        original: false,
        value: fields.join("_"),
    };
    CalibrationResolver::default()
        .resolve_embedded_offset(&offset, &OpticalSetup::InvisibleDiveCaseUnderwater)
        .expect("synthetic X5 V6 underwater calibration")
}

pub fn x5_underwater_lenses_with_exterior(exterior: u8) -> [LensFrame; 2] {
    let mut first = vec![100; 128 * 128 * 3];
    for row in 0..128_usize {
        for column in 0..128_usize {
            // The fixture's xi=2, focal=110.08, and maximum mask angle=94°
            // give an outer radius below 57 pixels. Radius 60 is safely
            // outside the mask, including every bilinear neighbor.
            if (column as f64 - 64.0).hypot(row as f64 - 64.0) > 60.0 {
                first[(row * 128 + column) * 3..][..3].fill(exterior);
            }
        }
    }
    [
        LensFrame::new(128, 128, first).expect("masked lens"),
        LensFrame::new(128, 128, vec![100; 128 * 128 * 3]).expect("constant lens"),
    ]
}

pub fn x5_underwater_lenses_with_lower_housing(housing: u8) -> [LensFrame; 2] {
    let mut first = vec![100; 512 * 512 * 3];
    // Source-down is the native mask's zero-degree azimuth. In this xi=2,
    // focal=440.32 fixture, its 90.5-degree boundary has radius <222 pixels.
    // This patch lies beyond that boundary, but inside the 94-degree side
    // boundary (radius >227), exposing an accidental exchange of mask axes.
    for row in 480..482_usize {
        for column in 248..265_usize {
            first[(row * 512 + column) * 3..][..3].fill(housing);
        }
    }
    [
        LensFrame::new(512, 512, first).expect("lower housing lens"),
        LensFrame::new(512, 512, vec![100; 512 * 512 * 3]).expect("constant lens"),
    ]
}
