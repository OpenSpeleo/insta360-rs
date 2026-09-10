//! Regenerates the explicitly self-derived complete temporal RGB regression fixture.
//! This is not an independent reference for restoration quality; see tests/reference.

#[cfg(feature = "underwater-ai")]
fn main() -> insta360_rs::Result<()> {
    use insta360_rs::assets::BundledAssetProvider;
    use insta360_rs::underwater::{mnn_runtime_version, UnderwaterColorSession};
    use insta360_rs::{UnderwaterColorMode, UnderwaterColorOptions};
    assert_eq!(mnn_runtime_version()?, "3.6.1");
    let path = std::env::args_os().nth(1).expect("output fixture path");
    let mut output = b"MNNRGB1\0".to_vec();
    for style in 0..4 {
        let mut session = UnderwaterColorSession::prepare(
            UnderwaterColorOptions {
                mode: UnderwaterColorMode::Ai,
                strength: Some(1.0),
                style: Some(style),
                ..Default::default()
            },
            64,
            64,
            30,
            1,
            &BundledAssetProvider,
        )?;
        for frame in 0..=61 {
            let mut pixels: Vec<_> = (0..64 * 64)
                .flat_map(|index| {
                    [
                        20 + (index % 31) as u8,
                        80 + (index % 61) as u8,
                        100 + (index % 101) as u8,
                    ]
                })
                .collect();
            if frame >= 10 {
                for pixel in pixels.chunks_exact_mut(3) {
                    pixel[0] = 200 - pixel[0];
                    pixel[1] /= 2;
                }
            }
            session.process_rgb8(&mut pixels, f64::from(frame) / 30.0)?;
            if [0, 9, 10, 59, 60, 61].contains(&frame) {
                output.extend(pixels);
            }
        }
    }
    std::fs::write(path, output).expect("write regression fixture");
    Ok(())
}

#[cfg(not(feature = "underwater-ai"))]
fn main() {
    eprintln!("Enable underwater-ai to regenerate the temporal color reference");
}
