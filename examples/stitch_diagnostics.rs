//! Exact-source diagnostic capture. All paths/settings are explicit environment inputs.
//! This produces local investigation artifacts, not reference-quality assertions.

#[cfg(feature = "media")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use insta360_rs::calibration::OffsetSource;
    use insta360_rs::media::{NativeColorProcessor, RecordingFrameRenderer};
    use insta360_rs::stitch::diagnostics;
    use insta360_rs::{
        CalibrationResolver, ColorConversion, EquirectangularProjection, InputSet, PairedReader,
        ProcessingBackend, RecordingSequence, RollingShutterCorrection, SeamMode, Stabilization,
        StitchConfig, UnderwaterColorOptions,
    };
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    fn save(
        path: &Path,
        width: u32,
        height: u32,
        rgb: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        image::codecs::png::PngEncoder::new(file).write_image(
            rgb,
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )?;
        Ok(())
    }
    use image::ImageEncoder;

    let input = PathBuf::from(std::env::var("INSTA360_STITCH_INPUT")?);
    let output = PathBuf::from(std::env::var("INSTA360_STITCH_OUTPUT")?);
    let seconds: f64 = std::env::var("INSTA360_STITCH_TIME")
        .unwrap_or_else(|_| "231".into())
        .parse()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("invalid diagnostic timestamp".into());
    }
    let width: u32 = std::env::var("INSTA360_STITCH_WIDTH")
        .unwrap_or_else(|_| "1920".into())
        .parse()?;
    let projection = EquirectangularProjection {
        width,
        height: width / 2,
    }
    .validate()?;
    let modes = std::env::var("INSTA360_STITCH_MODES").unwrap_or_else(|_| "off".into());
    let samples: usize = std::env::var("INSTA360_STITCH_SAMPLES")
        .unwrap_or_else(|_| "1".into())
        .parse()?;
    let warmup: usize = std::env::var("INSTA360_STITCH_WARMUP")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    if !(1..=1000).contains(&samples) || warmup > 100 {
        return Err("diagnostic samples must be 1..1000 and warmup <=100".into());
    }
    let requested_backends =
        std::env::var("INSTA360_STITCH_BACKENDS").unwrap_or_else(|_| "cpu,gpu".into());
    if requested_backends
        .split(',')
        .any(|backend| !["cpu", "gpu"].contains(&backend))
    {
        return Err("diagnostic backends must be cpu and/or gpu".into());
    }
    let stabilization = match std::env::var("INSTA360_STITCH_STABILIZATION")
        .unwrap_or_else(|_| "off".into())
        .as_str()
    {
        "off" => Stabilization::Off,
        "auto" => Stabilization::default(),
        _ => return Err("diagnostic stabilization must be off or auto".into()),
    };
    let cancel = AtomicBool::new(false);
    let sequence = RecordingSequence::single(InputSet::new(vec![input.clone()])?)?;
    let mut reader = PairedReader::open(&sequence, Duration::from_secs_f64(seconds))?;
    let pair = reader
        .next_pair(&cancel)?
        .ok_or("no frame at diagnostic timestamp")?;
    let config = StitchConfig {
        color_conversion: ColorConversion::Preserve,
        stabilization,
        rolling_shutter: RollingShutterCorrection::Off,
        ..Default::default()
    };
    let metadata = &sequence.chapters[pair.chapter_index].inspection.metadata;
    let calibration = CalibrationResolver::new(config.calibration_policy).resolve_metadata(
        metadata,
        &config.optical_selection(),
        OffsetSource::Current,
    )?;
    let mut native = NativeColorProcessor::new(
        sequence.clone(),
        ColorConversion::Preserve,
        UnderwaterColorOptions::default(),
    )?;
    let lenses = native.process(&pair, None, &cancel)?;
    let diagnostics = diagnostics::inspect(&lenses, &calibration, projection)?;
    std::fs::create_dir_all(&output)?;
    for (index, source) in lenses.iter().enumerate() {
        save(
            &output.join(format!("source-{index}.png")),
            source.width(),
            source.height(),
            source.as_rgb8(),
        )?;
        let mask = &diagnostics.source_masks[index];
        save(
            &output.join(format!("mask-{index}.png")),
            mask.width(),
            mask.height(),
            mask.as_rgb8(),
        )?;
        let projected = &diagnostics.projected[index];
        save(
            &output.join(format!("projected-{index}.png")),
            projected.width(),
            projected.height(),
            projected.as_rgb8(),
        )?;
    }
    for (name, frame) in [
        ("ownership", diagnostics.ownership),
        ("detail-weights", diagnostics.detail_weights),
        ("illumination-weights", diagnostics.illumination_weights),
    ] {
        save(
            &output.join(format!("{name}.png")),
            frame.width(),
            frame.height(),
            frame.as_rgb8(),
        )?;
    }
    let mut results = Vec::new();
    for mode in modes.split(',') {
        let seam_mode = match mode {
            "off" => SeamMode::Fixed,
            "dynamic" => SeamMode::Dynamic,
            "opticalFlow" => SeamMode::OpticalFlow,
            "ai" => SeamMode::Ai,
            _ => return Err(format!("unknown diagnostic mode {mode}").into()),
        };
        for (label, backend) in [
            ("cpu", ProcessingBackend::Cpu),
            ("gpu", ProcessingBackend::Gpu),
        ] {
            if !requested_backends.split(',').any(|value| value == label) {
                continue;
            }
            let start = Instant::now();
            let run = (|| -> insta360_rs::Result<_> {
                let mut renderer = RecordingFrameRenderer::new(
                    sequence.clone(),
                    StitchConfig {
                        seam_mode,
                        backend,
                        ..config.clone()
                    },
                )?;
                let frame = renderer.render(&pair, projection, &cancel)?;
                let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
                let mut durations = Vec::with_capacity(samples);
                let mut identical = true;
                for iteration in 0..warmup + samples {
                    let started = Instant::now();
                    let repeated = renderer.render(&pair, projection, &cancel)?;
                    if iteration >= warmup {
                        durations.push(started.elapsed().as_secs_f64() * 1000.0);
                    }
                    identical &= frame.frame == repeated.frame;
                }
                Ok((frame, cold_ms, durations, identical))
            })();
            match run {
                Ok((frame, cold_ms, durations, identical)) => {
                    save(
                        &output.join(format!("{mode}-{label}.png")),
                        frame.frame.width(),
                        frame.frame.height(),
                        frame.frame.as_rgb8(),
                    )?;
                    results.push(serde_json::json!({"mode": mode, "backend": frame.backend, "info": frame.info,
                        "confidenceCoverage": frame.stitch_plan.as_ref().map(|plan| plan.confidence_coverage()),
                        "coldMs": cold_ms, "warmMs": durations.iter().sum::<f64>() / durations.len() as f64, "warmSamplesMs": durations, "repeatIdentical": identical}));
                }
                Err(error) => results.push(
                    serde_json::json!({"mode": mode, "backend": label, "error": error.to_string()}),
                ),
            }
        }
    }
    let mut source = std::fs::File::open(&input)?;
    let length = source.metadata()?.len();
    let edge_len = length.min(1_048_576) as usize;
    let mut edge = vec![0; edge_len * 2];
    source.read_exact(&mut edge[..edge_len])?;
    source.seek(SeekFrom::Start(length - edge_len as u64))?;
    source.read_exact(&mut edge[edge_len..])?;
    let report = serde_json::json!({
        "source": input, "bytes": length, "edgeSampleSha256": insta360_rs::assets::sha256(&edge).to_hex(),
        "edgeSampleDefinition": "first and last min(1 MiB, file length) concatenated",
        "requestedSeconds": seconds, "timestampMicros": pair.timestamp_micros,
        "sourceTimestampMicros": pair.source_timestamp_micros,
        "camera": metadata.camera_name, "firmware": metadata.firmware,
        "samples": samples, "warmup": warmup, "debugAssertions": cfg!(debug_assertions),
        "diagnosticOrientation": "Source masks, per-lens projections and ownership are unstabilized; rendered mode images use config.stabilization",
        "crop": metadata.crop_window, "calibration": calibration, "config": config, "results": results,
        "qualification": "Diagnostic output; not independent Studio or physical-camera quality evidence"
    });
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("report.json"))?;
    serde_json::to_writer_pretty(file, &report)?;
    println!("{}", output.join("report.json").display());
    Ok(())
}

#[cfg(not(feature = "media"))]
fn main() {
    eprintln!("stitch_diagnostics requires the media feature");
}
