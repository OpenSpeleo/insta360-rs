#![cfg(feature = "media")]

//! Independent encoded fixtures for the decoded source adapters. These establish
//! layout, identity and export contracts, not physical-camera calibration.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use insta360_rs::media::Exporter;
use insta360_rs::paired::{PairedPreviewReader, PreviewAcceleration};
use insta360_rs::{
    AudioPolicy, EquirectangularProjection, FrameSelection, ImageExportOptions, InputSet,
    MediaAcceleration, PairedReader, ProcessingBackend, RecordingSequence,
    RollingShutterCorrection, Stabilization, StitchConfig, VideoExportOptions,
};

#[derive(Clone, Copy, Debug)]
enum Layout {
    Tracks,
    Legacy,
    Packed,
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
fn integer(out: &mut Vec<u8>, tag: u32, value: u64) {
    varint(out, u64::from(tag) * 8);
    varint(out, value);
}
fn bytes(out: &mut Vec<u8>, tag: u32, value: &[u8]) {
    varint(out, u64::from(tag) * 8 + 2);
    varint(out, value.len() as u64);
    out.extend(value);
}

fn offset(lens_type: u32, version: u8) -> String {
    let mut fields = vec!["2".to_owned()];
    for cx in [32.0, 96.0] {
        let mut values = if version == 2 {
            vec![
                30.0, cx, 32.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
            ]
        } else {
            let mut values = vec![1.0, 26.0, 26.0, cx, 32.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
            values.extend(vec![0.0; if version == 6 { 13 } else { 5 }]);
            values
        };
        values.extend([128.0, 64.0, f64::from(lens_type)]);
        fields.extend(values.iter().map(ToString::to_string));
    }
    fields.push(((u32::from(version) << 16) | 1024).to_string());
    fields.join("_")
}

fn trailer(camera: &str, lens_type: u32, version: u8, layout: Layout) -> Vec<u8> {
    trailer_with_fields(camera, lens_type, version, layout, &[])
}

fn trailer_with_fields(
    camera: &str,
    lens_type: u32,
    version: u8,
    layout: Layout,
    extra: &[u8],
) -> Vec<u8> {
    let mut data = Vec::new();
    bytes(&mut data, 1, b"INDEPENDENT-LAYOUT-FIXTURE");
    bytes(&mut data, 2, camera.as_bytes());
    bytes(
        &mut data,
        match version {
            2 => 53,
            3 => 54,
            _ => 111,
        },
        offset(lens_type, version).as_bytes(),
    );
    integer(&mut data, 68, 0); // Explicit bare optics, independent of filename.
    integer(&mut data, 129, 2); // Full dual-fisheye projection.
    integer(&mut data, 130, 1); // No recorded frame rotation.
    integer(
        &mut data,
        131,
        match layout {
            Layout::Tracks => 3,
            Layout::Legacy => 2,
            Layout::Packed => 1,
        },
    );
    data.extend_from_slice(extra);
    let mut payload = data.clone();
    payload.extend([1, 1]);
    payload.extend((data.len() as u32).to_le_bytes());
    let mut index = vec![0; 20];
    index[10] = 1;
    index[11] = 1;
    index[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
    payload.extend(&index);
    payload.extend([0, 0]);
    payload.extend((index.len() as u32).to_le_bytes());
    payload.extend([0; 32]);
    payload.extend(((payload.len() + 40) as u32).to_le_bytes());
    payload.extend(3u32.to_le_bytes());
    payload.extend(b"8db42d694ccc418790edff439fe026bf");
    let mut result = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    result.extend(b"inst");
    result.extend(payload);
    result
}

fn run(command: &mut Command) {
    let output = command.output().expect("ffmpeg fixture prerequisite");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[allow(clippy::too_many_arguments)]
fn fixture(
    directory: &Path,
    camera: &str,
    lens_type: u32,
    version: u8,
    layout: Layout,
    frames: usize,
    origin: &str,
    audio: bool,
) -> InputSet {
    let count = if matches!(layout, Layout::Legacy) {
        2
    } else {
        1
    };
    let mut paths = Vec::new();
    for lens in 0..count {
        let path = directory.join(format!(
            "VID_fixture_{}_001.insv",
            if lens == 0 { "00" } else { "10" }
        ));
        let mut command = Command::new("ffmpeg");
        command
            .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i"])
            .arg(format!(
                "color=c={}:size=64x64:rate=12",
                if lens == 0 { "red" } else { "blue" }
            ));
        if !matches!(layout, Layout::Legacy) {
            command.args(["-f", "lavfi", "-i", "color=c=blue:size=64x64:rate=12"]);
        }
        if audio {
            command.args(["-f", "lavfi", "-i"]).arg(format!(
                "sine=frequency={}:sample_rate=48000",
                440 + 220 * lens
            ));
        }
        match layout {
            Layout::Tracks => {
                command.args(["-map", "0:v", "-map", "1:v"]);
            }
            Layout::Legacy => {
                command.args(["-map", "0:v"]);
            }
            Layout::Packed => {
                command.args([
                    "-filter_complex",
                    "[0:v][1:v]hstack=inputs=2[v]",
                    "-map",
                    "[v]",
                ]);
            }
        }
        if audio {
            command.args([
                "-map",
                if matches!(layout, Layout::Legacy) {
                    "1:a"
                } else {
                    "2:a"
                },
                "-c:a",
                "aac",
            ]);
        }
        command
            .args([
                "-t",
                &(frames as f64 / 12.0).to_string(),
                "-c:v",
                "mpeg4",
                "-bf",
                "2",
                "-g",
                if lens == 0 { "12" } else { "4" },
                "-q:v",
                "2",
                "-video_track_timescale",
                if lens == 0 { "12288" } else { "90000" },
                "-output_ts_offset",
                origin,
                "-f",
                "mp4",
            ])
            .arg(&path);
        run(&mut command);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&trailer(camera, lens_type, version, layout))
            .unwrap();
        paths.push(path);
    }
    paths.reverse(); // Public InputSet must restore physical A/B identity.
    InputSet::new(paths).unwrap()
}

fn config() -> StitchConfig {
    StitchConfig {
        stabilization: Stabilization::Off,
        rolling_shutter: RollingShutterCorrection::Off,
        backend: ProcessingBackend::Cpu,
        projection: Some(EquirectangularProjection {
            width: 128,
            height: 64,
        }),
        ..StitchConfig::default()
    }
}

fn assert_lens_colors(pair: &insta360_rs::FramePair) {
    assert_eq!((pair.a.width(), pair.a.height()), (64, 64));
    assert_eq!((pair.b.width(), pair.b.height()), (64, 64));
    // Limited-range YUV420 red and blue have independent known luma values.
    assert!(
        pair.a.data(0)[0].abs_diff(81) <= 2,
        "A must be the red _00_/left lens"
    );
    assert!(
        pair.b.data(0)[0].abs_diff(41) <= 2,
        "B must be the blue _10_/right lens"
    );
}

#[test]
fn legacy_and_packed_pairs_keep_native_identity_nonzero_origins_and_repeated_seeks() {
    for layout in [Layout::Tracks, Layout::Legacy, Layout::Packed] {
        let directory = tempfile::tempdir().unwrap();
        let inputs = fixture(
            directory.path(),
            "Insta360 ONE X2",
            41,
            3,
            layout,
            60,
            "0.25",
            false,
        );
        let sequence = RecordingSequence::single(inputs).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut serial = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let mut identities = Vec::new();
        while let Some(pair) = serial.next_pair(&cancel).unwrap() {
            assert_lens_colors(&pair);
            assert_eq!(
                pair.timestamp_micros,
                (identities.len() as f64 * 1_000_000.0 / 12.0).round() as i64
            );
            assert!(pair.source_timestamp_micros >= 250_000);
            identities.push(pair.identity());
        }
        assert_eq!(identities.len(), 60);
        let mut preview =
            PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
        for cycle in 0..3 {
            for index in [0, 59, 13, 1, 48, 31, 2, 58] {
                let time = Duration::from_micros(index * 1_000_000 / 12);
                let pair = preview.frame_at(time, cancel.clone()).unwrap();
                assert_eq!(
                    pair.identity(),
                    identities[index as usize],
                    "{layout:?} cycle {cycle}"
                );
                assert_lens_colors(&pair);
                let saved = PairedReader::open_at_pair(&sequence, pair.identity())
                    .unwrap()
                    .next_pair(&cancel)
                    .unwrap()
                    .unwrap();
                assert_eq!(saved.identity(), pair.identity());
                assert_lens_colors(&saved);
            }
            cancel.store(true, Ordering::Relaxed);
            assert!(matches!(
                preview.frame_at(Duration::ZERO, cancel.clone()),
                Err(insta360_rs::Error::Cancelled)
            ));
            cancel.store(false, Ordering::Relaxed);
        }
    }
}

#[test]
fn all_registered_bare_camera_families_export_images_and_video_with_motion_off() {
    for (camera, lens, version, layout) in [
        ("Insta360 ONE", 13, 2, Layout::Packed),
        ("Insta360 ONE R", 33, 3, Layout::Legacy),
        ("Insta360 ONE RS", 33, 3, Layout::Tracks),
        ("Insta360 ONE RS 1-Inch 360 Edition", 62, 3, Layout::Packed),
        ("Insta360 X4 Air", 131, 6, Layout::Tracks),
        ("B2", 142, 6, Layout::Packed),
        ("Insta360 ONE X", 19, 2, Layout::Legacy),
        ("Insta360 ONE X2", 41, 3, Layout::Packed),
        ("Insta360 X3", 70, 3, Layout::Tracks),
        ("Insta360 X4", 71, 3, Layout::Legacy),
        ("Insta360 X5", 113, 6, Layout::Packed),
        ("Insta360 X6", 193, 6, Layout::Tracks),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let inputs = fixture(
            directory.path(),
            camera,
            lens,
            version,
            layout,
            6,
            "0",
            false,
        );
        let exporter = Exporter::new(inputs, config()).unwrap();
        let options = VideoExportOptions {
            audio: AudioPolicy::Drop,
            acceleration: MediaAcceleration::Software,
            ..VideoExportOptions::default()
        };
        assert_eq!(
            exporter.preflight_video(&options).unwrap().frame_rate,
            12.0,
            "{camera}"
        );
        let image = exporter
            .export_frames(
                directory.path().join("frames"),
                FrameSelection::Indices(vec![0, 5]),
                ImageExportOptions::default(),
            )
            .wait()
            .unwrap();
        assert_eq!(image.frames_written, 2);
        for path in &image.outputs {
            let rgb = image::open(path).unwrap().into_rgb8();
            assert_eq!(rgb.dimensions(), (128, 64));
            assert!(
                rgb.pixels().any(|pixel| pixel[0] > 200 && pixel[2] < 40),
                "{camera}: red camera A survives stitching"
            );
            assert!(
                rgb.pixels().any(|pixel| pixel[2] > 200 && pixel[0] < 40),
                "{camera}: blue camera B survives stitching"
            );
        }
        let video = exporter
            .export_video(directory.path().join("result.mp4"), options)
            .wait()
            .unwrap();
        assert_eq!(video.frames_written, 6, "{camera}");
    }
}

#[test]
fn legacy_audio_stream_collisions_keep_both_original_packet_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Legacy,
        24,
        "0",
        true,
    );
    let sequence = RecordingSequence::single(inputs.clone()).unwrap();
    let mut expected: BTreeMap<usize, Vec<Vec<u8>>> = BTreeMap::new();
    let mut offset = 0;
    for path in inputs.paths() {
        let mut input = ffmpeg_next::format::input(path).unwrap();
        let count = input.nb_streams() as usize;
        for (stream, packet) in input.packets() {
            if stream.parameters().medium() == ffmpeg_next::media::Type::Audio {
                expected
                    .entry(offset + stream.index())
                    .or_default()
                    .push(packet.data().unwrap().to_vec());
            }
        }
        offset += count;
    }
    assert_eq!(expected.len(), 2);
    let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
    reader.enable_audio();
    let mut actual: BTreeMap<usize, Vec<Vec<u8>>> = BTreeMap::new();
    loop {
        let next = reader.next_pair(&AtomicBool::new(false)).unwrap();
        for (chapter, packet) in reader.take_audio_packets() {
            assert_eq!(chapter, 0);
            actual
                .entry(packet.stream())
                .or_default()
                .push(packet.data().unwrap().to_vec());
        }
        if next.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let exporter = Exporter::from_sequence(sequence, config()).unwrap();
    let options = VideoExportOptions {
        acceleration: MediaAcceleration::Software,
        ..VideoExportOptions::default()
    };
    assert_eq!(exporter.preflight_video(&options).unwrap().audio_tracks, 2);
    let output = directory.path().join("audio.mp4");
    assert_eq!(
        exporter
            .export_video(&output, options)
            .wait()
            .unwrap()
            .frames_written,
        24
    );
    let result = ffmpeg_next::format::input(&output).unwrap();
    assert_eq!(
        result
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg_next::media::Type::Audio)
            .count(),
        2
    );
    let mut expected_tracks = Vec::new();
    for path in inputs.paths() {
        let mut input = ffmpeg_next::format::input(path).unwrap();
        let mut complete = Vec::new();
        for (stream, packet) in input.packets() {
            if stream.parameters().medium() != ffmpeg_next::media::Type::Audio {
                continue;
            }
            let pts = packet.pts().unwrap();
            let base = stream.time_base();
            let end_ticks = i128::from(pts + packet.duration()) * i128::from(base.numerator());
            if pts >= 0 && end_ticks <= 2 * i128::from(base.denominator()) {
                complete.push(packet.data().unwrap().to_vec());
            }
        }
        expected_tracks.push(complete);
    }
    let mut encoded = ffmpeg_next::format::input(&output).unwrap();
    let mut actual_tracks: BTreeMap<usize, Vec<Vec<u8>>> = BTreeMap::new();
    for (stream, packet) in encoded.packets() {
        if stream.parameters().medium() == ffmpeg_next::media::Type::Audio {
            actual_tracks
                .entry(stream.index())
                .or_default()
                .push(packet.data().unwrap().to_vec());
        }
    }
    assert_eq!(
        actual_tracks.into_values().collect::<Vec<_>>(),
        expected_tracks
    );
}

#[test]
fn ambiguous_packed_geometry_and_legacy_metadata_mismatch_fail_before_output() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Packed,
        12,
        "0",
        false,
    );
    let sequence = RecordingSequence::single(inputs.clone()).unwrap();
    for mutation in 0..7 {
        let mut invalid = sequence.clone();
        let metadata = &mut invalid.chapters[0].inspection.metadata;
        match mutation {
            0 => metadata.file_category = None,
            1 => metadata.file_rotation = None,
            2 => metadata.file_rotation = Some(insta360_rs::container::FileRotation::Degrees90),
            3 => metadata.stream_type = None,
            4 => metadata.offsets.clear(),
            5 => {
                metadata.offsets[0].value =
                    metadata.offsets[0].value.replacen("_32_32_", "_96_32_", 1)
            }
            _ => invalid.chapters[0].inspection.video_tracks[0].width = 126,
        }
        assert!(
            PairedReader::open(&invalid, Duration::ZERO).is_err(),
            "mutation {mutation}"
        );
        assert!(PairedPreviewReader::new(&invalid, PreviewAcceleration::Software).is_err());
    }
    let renamed = directory.path().join("unknown.insv");
    fs::copy(&inputs.paths()[0], &renamed).unwrap();
    let unknown = RecordingSequence::single(InputSet::new(vec![renamed]).unwrap()).unwrap();
    assert!(PairedReader::open(&unknown, Duration::ZERO).is_err());

    let other = tempfile::tempdir().unwrap();
    let pair = fixture(
        other.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Legacy,
        12,
        "0",
        false,
    );
    let secondary = &pair.paths()[1];
    // Append a new, independently well-formed trailer with a different camera.
    OpenOptions::new()
        .append(true)
        .open(secondary)
        .unwrap()
        .write_all(&trailer("Insta360 X3", 70, 3, Layout::Legacy))
        .unwrap();
    assert!(RecordingSequence::single(pair).is_err());
}

#[test]
fn expanded_cameras_do_not_inherit_x5_motion_or_unknown_single_lens_layouts() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Legacy,
        12,
        "0",
        false,
    );
    let mut unsupported_motion = config();
    unsupported_motion.stabilization = Stabilization::DirectionLock;
    let exporter = Exporter::new(inputs.clone(), unsupported_motion).unwrap();
    assert!(exporter
        .preflight_video(&VideoExportOptions::default())
        .is_err());
    let image_dir = directory.path().join("bad-motion");
    assert!(exporter
        .export_frames(
            &image_dir,
            FrameSelection::Indices(vec![0]),
            ImageExportOptions::default()
        )
        .wait()
        .is_err());
    assert!(!image_dir.exists());
    let single =
        RecordingSequence::single(InputSet::new(vec![inputs.paths()[0].clone()]).unwrap()).unwrap();
    assert!(PairedReader::open(&single, Duration::ZERO).is_err());
}

#[test]
fn long_legacy_sources_keep_advancing_with_bounded_queues_and_recover_after_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Legacy,
        4096,
        "0",
        false,
    );
    let sequence = RecordingSequence::single(inputs).unwrap();
    let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
    let cancel = AtomicBool::new(false);
    let mut count = 0;
    while let Some(pair) = reader.next_pair(&cancel).unwrap() {
        assert_lens_colors(&pair);
        count += 1;
        if count % 127 == 0 {
            cancel.store(true, Ordering::Relaxed);
            assert!(matches!(
                reader.next_pair(&cancel),
                Err(insta360_rs::Error::Cancelled)
            ));
            cancel.store(false, Ordering::Relaxed);
        }
    }
    assert_eq!(count, 4096);
}

fn high_depth_source(directory: &Path, source: &Path, transfer: &str) -> InputSet {
    let output = directory.join(format!("VID_{transfer}_00_002.insv"));
    let transfer_id = match transfer {
        "smpte2084" => 16,
        "arib-std-b67" => 18,
        _ => 1,
    };
    let x265 = format!(
        "pools=1:frame-threads=1:log-level=error:colorprim=1:transfer={transfer_id}:colormatrix=1"
    );
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-c:v",
            "libx265",
            "-threads",
            "1",
            "-x265-params",
            &x265,
            "-pix_fmt",
            "yuv420p10le",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            transfer,
            "-color_range",
            "tv",
            "-f",
            "mp4",
        ])
        .arg(&output);
    run(&mut command);
    OpenOptions::new()
        .append(true)
        .open(&output)
        .unwrap()
        .write_all(&trailer("Insta360 ONE X2", 41, 3, Layout::Packed))
        .unwrap();
    InputSet::new(vec![output]).unwrap()
}

#[test]
fn packed_ten_bit_sdr_retains_native_samples_and_exports_explicit_eight_bit_output() {
    let directory = tempfile::tempdir().unwrap();
    let original = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Packed,
        6,
        "0",
        false,
    );
    let input = high_depth_source(directory.path(), &original.paths()[0], "bt709");
    let sequence = RecordingSequence::single(input.clone()).unwrap();
    let pair = PairedReader::open(&sequence, Duration::ZERO)
        .unwrap()
        .next_pair(&AtomicBool::new(false))
        .unwrap()
        .unwrap();
    let reference = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&input.paths()[0])
        .args([
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p10le",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    assert!(reference.status.success());
    for (frame, offset) in [(&pair.a, 0), (&pair.b, 64 * 2)] {
        assert_eq!(frame.format(), ffmpeg_next::format::Pixel::YUV420P10LE);
        let sample = u16::from_le_bytes([frame.data(0)[0], frame.data(0)[1]]);
        let expected = u16::from_le_bytes([reference.stdout[offset], reference.stdout[offset + 1]]);
        assert_eq!(
            sample, expected,
            "owning packed halves preserve original 10-bit values"
        );
    }
    let exporter = Exporter::new(input, config()).unwrap();
    let images = exporter
        .export_frames(
            directory.path().join("sdr"),
            FrameSelection::Indices(vec![0]),
            ImageExportOptions::default(),
        )
        .wait()
        .unwrap();
    let image = image::open(&images.outputs[0]).unwrap();
    assert_eq!(image.color(), image::ColorType::Rgb8);
    let video = directory.path().join("sdr.mp4");
    exporter
        .export_video(
            &video,
            VideoExportOptions {
                audio: AudioPolicy::Drop,
                acceleration: MediaAcceleration::Software,
                ..VideoExportOptions::default()
            },
        )
        .wait()
        .unwrap();
    let mut input = ffmpeg_next::format::input(&video).unwrap();
    let stream = input
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .unwrap();
    let index = stream.index();
    let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())
        .unwrap()
        .decoder()
        .video()
        .unwrap();
    for (stream, packet) in input.packets() {
        if stream.index() != index {
            continue;
        }
        decoder.send_packet(&packet).unwrap();
        let mut frame = ffmpeg_next::frame::Video::empty();
        if decoder.receive_frame(&mut frame).is_ok() {
            assert_eq!(frame.format(), ffmpeg_next::format::Pixel::YUV420P);
            return;
        }
    }
    decoder.send_eof().unwrap();
    let mut frame = ffmpeg_next::frame::Video::empty();
    decoder.receive_frame(&mut frame).unwrap();
    assert_eq!(frame.format(), ffmpeg_next::format::Pixel::YUV420P);
}

#[test]
fn pq_hlg_and_dolby_reject_stitched_export_even_when_color_preserve_is_requested() {
    let directory = tempfile::tempdir().unwrap();
    let original = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Packed,
        6,
        "0",
        false,
    );
    for transfer in ["smpte2084", "arib-std-b67", "bt709"] {
        let input = high_depth_source(directory.path(), &original.paths()[0], transfer);
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=color_transfer",
                "-of",
                "default=nw=1",
            ])
            .arg(&input.paths()[0])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&probe.stdout).contains(transfer),
            "{transfer}: {}",
            String::from_utf8_lossy(&probe.stdout)
        );
        if transfer == "bt709" {
            let mut mode = Vec::new();
            integer(&mut mode, 8, 3); // RecordedColorMode Dolby, independent of transfer tags.
            let mut extra = Vec::new();
            bytes(&mut extra, 212, &mode);
            OpenOptions::new()
                .append(true)
                .open(&input.paths()[0])
                .unwrap()
                .write_all(&trailer_with_fields(
                    "Insta360 ONE X2",
                    41,
                    3,
                    Layout::Packed,
                    &extra,
                ))
                .unwrap();
        }
        // Native paired access preserves HDR samples for callers; only stitched
        // RGB8/YUV4208 export requires the missing tone mapper.
        let sequence = RecordingSequence::single(input.clone()).unwrap();
        if transfer == "bt709" {
            assert_eq!(
                sequence.chapters[0].inspection.metadata.recorded_color_mode,
                Some(insta360_rs::container::RecordedColorMode::Dolby)
            );
        }
        assert!(PairedReader::open(&sequence, Duration::ZERO)
            .unwrap()
            .next_pair(&AtomicBool::new(false))
            .unwrap()
            .is_some());
        for preserve in [false, true] {
            let mut config = config();
            if preserve {
                config.color_conversion = insta360_rs::ColorConversion::Preserve;
            }
            let exporter = Exporter::new(input.clone(), config).unwrap();
            let error = exporter
                .preflight_video(&VideoExportOptions::default())
                .expect_err(transfer);
            assert!(
                error.to_string().contains("tone mapper"),
                "{transfer}: {error}"
            );
            let images = directory
                .path()
                .join(format!("{transfer}-{preserve}-images"));
            assert!(exporter
                .export_frames(
                    &images,
                    FrameSelection::Indices(vec![0]),
                    ImageExportOptions::default()
                )
                .wait()
                .is_err());
            assert!(!images.exists());
            let video = directory.path().join(format!("{transfer}-{preserve}.mp4"));
            assert!(exporter
                .export_video(&video, VideoExportOptions::default())
                .wait()
                .is_err());
            assert!(!video.exists());
        }
    }
}

#[test]
fn legacy_and_packed_chapters_keep_recording_order_preview_boundaries_and_global_video_cuts() {
    for layout in [Layout::Legacy, Layout::Packed] {
        let directory = tempfile::tempdir().unwrap();
        let mut chapters = Vec::new();
        for index in [2, 0] {
            let chapter_dir = directory.path().join(format!("part-{index}"));
            fs::create_dir(&chapter_dir).unwrap();
            let inputs = fixture(
                &chapter_dir,
                "Insta360 ONE X2",
                41,
                3,
                layout,
                12,
                "0",
                true,
            );
            let mut group = Vec::new();
            integer(&mut group, 1, 20);
            integer(&mut group, 2, index);
            bytes(&mut group, 3, b"independent-layout-sequence");
            integer(&mut group, 4, 4); // Submedia indexes may include previews.
            let mut extra = Vec::new();
            bytes(&mut extra, 26, &group);
            integer(&mut extra, 88, 2);
            for path in inputs.paths() {
                OpenOptions::new()
                    .append(true)
                    .open(path)
                    .unwrap()
                    .write_all(&trailer_with_fields(
                        "Insta360 ONE X2",
                        41,
                        3,
                        layout,
                        &extra,
                    ))
                    .unwrap();
            }
            chapters.push(inputs);
        }
        let sequence = RecordingSequence::new(chapters).unwrap();
        assert_eq!(sequence.chapters[0].group_index, Some(0));
        assert_eq!(sequence.chapters[1].group_index, Some(2));
        assert_eq!(sequence.duration, Duration::from_secs(2));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut preview =
            PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
        for _ in 0..5 {
            for (time, chapter) in [(990, 1), (0, 0), (1000, 1), (500, 0)] {
                let pair = preview
                    .frame_at(Duration::from_millis(time), cancel.clone())
                    .unwrap();
                assert_eq!(pair.chapter_index, chapter);
                assert_lens_colors(&pair);
                if time == 990 {
                    assert_eq!(pair.timestamp_micros, 1_000_000);
                }
                assert_eq!(
                    PairedReader::open_at_pair(&sequence, pair.identity())
                        .unwrap()
                        .next_pair(&cancel)
                        .unwrap()
                        .unwrap()
                        .identity(),
                    pair.identity()
                );
            }
        }
        let exporter = Exporter::from_sequence(sequence, config()).unwrap();
        let output = directory.path().join("clipped.mp4");
        let options = VideoExportOptions {
            start: Some(Duration::from_millis(950)),
            duration: Some(Duration::from_millis(150)),
            acceleration: MediaAcceleration::Software,
            ..VideoExportOptions::default()
        };
        assert_eq!(
            exporter
                .export_video(output, options)
                .wait()
                .unwrap()
                .frames_written,
            2
        );
    }
}

fn restoration_exports_match_standalone_color_stage(
    mode: insta360_rs::UnderwaterColorMode,
    backend: ProcessingBackend,
) {
    let directory = tempfile::tempdir().unwrap();
    let input_path = directory.path().join("VID_restoration_00_001.insv");
    run(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x305a80:size=64x64:rate=12",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x5090a0:size=64x64:rate=12",
            "-filter_complex",
            "[0:v][1:v]hstack=inputs=2[v]",
            "-map",
            "[v]",
            "-frames:v",
            "6",
            "-c:v",
            "mpeg4",
            "-q:v",
            "2",
            "-f",
            "mp4",
        ])
        .arg(&input_path));
    OpenOptions::new()
        .append(true)
        .open(&input_path)
        .unwrap()
        .write_all(&trailer("Insta360 ONE X2", 41, 3, Layout::Packed))
        .unwrap();
    let inputs = InputSet::new(vec![input_path]).unwrap();
    let mut settings = config();
    settings.backend = backend;
    let uncolored = Exporter::new(inputs.clone(), settings.clone())
        .unwrap()
        .export_frames(
            directory.path().join("original"),
            FrameSelection::Indices(vec![0, 5]),
            ImageExportOptions::default(),
        )
        .wait()
        .unwrap();
    settings.underwater_color.mode = mode;
    let exporter = Exporter::new(inputs, settings.clone()).unwrap();
    let restored = exporter
        .export_frames(
            directory.path().join("restored"),
            FrameSelection::Indices(vec![0, 5]),
            ImageExportOptions::default(),
        )
        .wait()
        .unwrap();
    let mut standalone = insta360_rs::underwater::UnderwaterColorSession::prepare(
        settings.underwater_color,
        128,
        64,
        12,
        1,
        &insta360_rs::assets::BundledAssetProvider,
    )
    .unwrap();
    let mut first_frame = Vec::new();
    for (index, (original, output)) in uncolored.outputs.iter().zip(&restored.outputs).enumerate() {
        let before = image::open(original).unwrap().to_rgb8().into_raw();
        let mut expected = before.clone();
        standalone.reset();
        standalone
            .process_rgb8(&mut expected, if index == 0 { 0.0 } else { 5.0 / 12.0 })
            .unwrap();
        let actual = image::open(output).unwrap().to_rgb8().into_raw();
        assert_eq!(
            actual, expected,
            "each selected image begins with fresh restoration state"
        );
        assert_ne!(
            actual, before,
            "enabled restoration must change the fixture colors"
        );
        if index == 0 {
            first_frame = actual;
        }
    }
    assert_eq!(
        restored.optics, uncolored.optics,
        "color does not change resolved optics"
    );
    let output = directory.path().join("restored.mp4");
    let options = VideoExportOptions {
        audio: AudioPolicy::Drop,
        acceleration: MediaAcceleration::Software,
        ..VideoExportOptions::default()
    };
    exporter.preflight_video(&options).unwrap();
    let video = exporter.export_video(&output, options).wait().unwrap();
    assert_eq!(video.frames_written, 6);
    assert_eq!(video.optics, restored.optics);
    let reference = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&output)
        .args(["-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .unwrap();
    assert!(reference.status.success());
    assert_eq!(reference.stdout.len(), first_frame.len());
    let error = reference
        .stdout
        .iter()
        .zip(&first_frame)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / first_frame.len() as f64;
    assert!(
        error < 5.0,
        "restored video must agree with restored still within HEVC/chroma error, MAE={error}"
    );
}

#[test]
fn legacy_restoration_is_applied_to_selected_images_and_stitched_video() {
    restoration_exports_match_standalone_color_stage(
        insta360_rs::UnderwaterColorMode::Legacy,
        ProcessingBackend::Cpu,
    );
}

#[cfg(feature = "underwater-ai")]
#[test]
fn ai_restoration_is_applied_to_selected_images_and_stitched_video() {
    restoration_exports_match_standalone_color_stage(
        insta360_rs::UnderwaterColorMode::Ai,
        ProcessingBackend::Cpu,
    );
}

#[cfg(feature = "gpu")]
#[test]
fn gpu_video_uses_rgb_restoration_when_underwater_color_is_enabled() {
    if insta360_rs::gpu::available_adapters().is_empty() {
        assert!(
            std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_none(),
            "required GPU adapter unavailable"
        );
        return;
    }
    restoration_exports_match_standalone_color_stage(
        insta360_rs::UnderwaterColorMode::Legacy,
        ProcessingBackend::Gpu,
    );
}

#[cfg(not(feature = "underwater-ai"))]
#[test]
fn unavailable_ai_fails_preflight_and_both_exports_before_creating_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 ONE X2",
        41,
        3,
        Layout::Packed,
        6,
        "0",
        false,
    );
    let mut settings = config();
    settings.underwater_color.mode = insta360_rs::UnderwaterColorMode::Ai;
    let exporter = Exporter::new(inputs, settings).unwrap();
    assert!(exporter
        .preflight_video(&VideoExportOptions::default())
        .unwrap_err()
        .to_string()
        .contains("underwater-ai"));
    let image_dir = directory.path().join("ai-images");
    assert!(exporter
        .export_frames(
            &image_dir,
            FrameSelection::Indices(vec![0]),
            ImageExportOptions::default()
        )
        .wait()
        .is_err());
    assert!(!image_dir.exists());
    let video = directory.path().join("ai.mp4");
    assert!(exporter
        .export_video(&video, VideoExportOptions::default())
        .wait()
        .is_err());
    assert!(!video.exists());
}

#[test]
fn reusable_exact_frame_rendering_matches_still_export_for_every_layout() {
    use insta360_rs::media::{inspect_frame_dimensions, RecordingFrameRenderer};
    let cancel = AtomicBool::new(false);
    for layout in [Layout::Tracks, Layout::Legacy, Layout::Packed] {
        let directory = tempfile::tempdir().unwrap();
        let inputs = fixture(
            directory.path(),
            "Insta360 X5",
            113,
            6,
            layout,
            3,
            "0.25",
            false,
        );
        let sequence = RecordingSequence::single(inputs.clone()).unwrap();
        let dimensions = inspect_frame_dimensions(&sequence).unwrap();
        assert_eq!((dimensions[0].width, dimensions[0].height), (64, 64));
        let settings = config();
        let projection = settings.projection.unwrap();
        let info = RecordingFrameRenderer::preflight(&sequence, &settings, None, &cancel).unwrap();
        assert_eq!(info[0].projection, projection);
        let mut renderer = RecordingFrameRenderer::new(sequence.clone(), settings.clone()).unwrap();
        assert_eq!(renderer.prepare_all(None, &cancel).unwrap(), info);
        let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let pair = reader.next_pair(&cancel).unwrap().unwrap();
        let identity = pair.identity();
        let originals = [pair.a.data(0).to_vec(), pair.b.data(0).to_vec()];
        let output = renderer.render(&pair, projection, &cancel).unwrap();
        assert_eq!(output.info, info[0]);
        assert_eq!(output.backend.selected, insta360_rs::EffectiveBackend::Cpu);
        assert_eq!(pair.identity(), identity);
        assert_eq!(pair.a.data(0), originals[0]);
        assert_eq!(pair.b.data(0), originals[1]);
        let exported = Exporter::new(inputs, settings)
            .unwrap()
            .export_frames(
                directory.path().join("still"),
                FrameSelection::Indices(vec![0]),
                ImageExportOptions::default(),
            )
            .wait()
            .unwrap();
        assert_eq!(
            output.frame.as_rgb8(),
            image::open(&exported.outputs[0])
                .unwrap()
                .to_rgb8()
                .as_raw()
        );
        assert!(matches!(
            renderer.render(&pair, projection, &AtomicBool::new(true)),
            Err(insta360_rs::Error::Cancelled)
        ));
        assert_eq!(
            renderer
                .render(&pair, projection, &cancel)
                .unwrap()
                .frame
                .as_rgb8(),
            output.frame.as_rgb8()
        );
    }
}

#[test]
fn instance_preflight_retains_preparation_and_recovers_after_failed_rechecks() {
    use insta360_rs::media::RecordingFrameRenderer;
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 X5",
        113,
        6,
        Layout::Tracks,
        2,
        "0",
        false,
    );
    let path = inputs.paths()[0].clone();
    let sequence = RecordingSequence::single(inputs).unwrap();
    let cancel = AtomicBool::new(false);
    let pair = PairedReader::open(&sequence, Duration::ZERO)
        .unwrap()
        .next_pair(&cancel)
        .unwrap()
        .unwrap();
    let mut settings = config();
    settings.projection = None;
    settings.underwater_color.mode = if cfg!(feature = "underwater-ai") {
        insta360_rs::UnderwaterColorMode::Ai
    } else {
        insta360_rs::UnderwaterColorMode::Legacy
    };
    let mut renderer = RecordingFrameRenderer::new(sequence, settings).unwrap();
    let reports = renderer.prepare_all(None, &cancel).unwrap();
    let projection = EquirectangularProjection {
        width: 128,
        height: 64,
    };
    assert_eq!(reports[0].projection, projection, "native lens default");
    let expected = renderer.render_strict(&pair, projection, &cancel).unwrap();
    assert!(matches!(
        renderer.prepare_all(None, &AtomicBool::new(true)),
        Err(insta360_rs::Error::Cancelled)
    ));
    assert!(renderer
        .prepare_all(
            Some(EquirectangularProjection {
                width: 16384,
                height: 8192,
            }),
            &cancel,
        )
        .is_err());
    // A metadata reload would now fail. Rendering the retained exact pair must
    // still work, and even a failed decoder recheck must leave preparation usable.
    let moved = directory.path().join("temporarily-moved.insv");
    std::fs::rename(&path, &moved).unwrap();
    assert!(renderer.prepare_all(None, &cancel).is_err());
    let reused = renderer.render_strict(&pair, projection, &cancel).unwrap();
    assert_eq!(reused.frame.as_rgb8(), expected.frame.as_rgb8());
    assert_eq!(reused.info, reports[0]);
    std::fs::rename(moved, path).unwrap();
    let override_projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    assert_eq!(
        renderer
            .prepare_all(Some(override_projection), &cancel)
            .unwrap()[0]
            .projection,
        override_projection
    );
    assert_eq!(renderer.prepare_all(None, &cancel).unwrap(), reports);
    assert_eq!(
        renderer
            .render_strict(&pair, projection, &cancel)
            .unwrap()
            .frame
            .as_rgb8(),
        expected.frame.as_rgb8()
    );
}

#[test]
fn native_color_needs_no_calibration_and_applies_ilog_before_independent_restoration() {
    use insta360_rs::media::NativeColorProcessor;
    use insta360_rs::{ColorConversion, UnderwaterColorMode, UnderwaterColorOptions};
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 X5",
        113,
        6,
        Layout::Tracks,
        3,
        "0.25",
        false,
    );
    let mut sequence = RecordingSequence::single(inputs).unwrap();
    sequence.chapters[0].inspection.metadata.offsets.clear();
    sequence.chapters[0].inspection.metadata.recorded_color_mode =
        Some(insta360_rs::container::RecordedColorMode::ILog);
    let cancel = AtomicBool::new(false);
    let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
    let pair = reader.next_pair(&cancel).unwrap().unwrap();
    let mut original = NativeColorProcessor::new(
        sequence.clone(),
        ColorConversion::Preserve,
        UnderwaterColorOptions::default(),
    )
    .unwrap();
    assert!(!original.requires_processing(0).unwrap());
    let original = original.process(&pair, None, &cancel).unwrap();
    let lut = insta360_rs::color::CubeLut::load_bundled("studio-i-log-x5-rec709").unwrap();
    #[cfg(not(feature = "underwater-ai"))]
    let modes = [UnderwaterColorMode::Off, UnderwaterColorMode::Legacy];
    #[cfg(feature = "underwater-ai")]
    let modes = [
        UnderwaterColorMode::Off,
        UnderwaterColorMode::Legacy,
        UnderwaterColorMode::Ai,
    ];
    for mode in modes {
        let options = UnderwaterColorOptions {
            mode,
            ..UnderwaterColorOptions::default()
        };
        let mut processor =
            NativeColorProcessor::new(sequence.clone(), ColorConversion::Auto, options).unwrap();
        assert!(
            processor.requires_processing(0).unwrap(),
            "Auto resolves I-Log even when restoration is Off"
        );
        assert_eq!(
            processor.preflight(Some(128), &cancel).unwrap()[0].width,
            64,
            "native processing does not upscale"
        );
        let actual = processor.process(&pair, None, &cancel).unwrap();
        let repeated = processor.process(&pair, None, &cancel).unwrap();
        let mut reference = insta360_rs::underwater::UnderwaterColorSession::prepare(
            options,
            64,
            64,
            12,
            1,
            &insta360_rs::assets::BundledAssetProvider,
        )
        .unwrap();
        for index in 0..2 {
            let mut expected = original[index].as_rgb8().to_vec();
            lut.apply_rgb8(&mut expected).unwrap();
            reference.reset();
            reference
                .process_rgb8(&mut expected, pair.timestamp_micros as f64 / 1_000_000.0)
                .unwrap();
            assert_eq!(actual[index].as_rgb8(), expected);
            assert_eq!(actual[index].as_rgb8(), repeated[index].as_rgb8());
        }
        assert!(matches!(
            processor.process(&pair, None, &AtomicBool::new(true)),
            Err(insta360_rs::Error::Cancelled)
        ));
        if mode == UnderwaterColorMode::Legacy {
            assert!(processor.preflight(Some(32), &cancel).is_err());
        } else {
            let scaled = processor.process(&pair, Some(32), &cancel).unwrap();
            assert_eq!((scaled[0].width(), scaled[0].height()), (32, 32));
        }
    }
}

#[test]
fn frame_preflight_checks_dimensions_color_resources_and_exact_pair_contract() {
    use insta360_rs::media::RecordingFrameRenderer;
    let directory = tempfile::tempdir().unwrap();
    let inputs = fixture(
        directory.path(),
        "Insta360 X5",
        113,
        6,
        Layout::Tracks,
        2,
        "0",
        false,
    );
    let sequence = RecordingSequence::single(inputs).unwrap();
    let mut settings = config();
    settings.underwater_color.mode = insta360_rs::UnderwaterColorMode::Legacy;
    let cancel = AtomicBool::new(false);
    for projection in [
        EquirectangularProjection {
            width: 64,
            height: 32,
        },
        EquirectangularProjection {
            width: 16384,
            height: 8192,
        },
    ] {
        assert!(
            RecordingFrameRenderer::preflight(&sequence, &settings, Some(projection), &cancel)
                .is_err()
        );
    }
    let projection = settings.projection.unwrap();
    let mut renderer = RecordingFrameRenderer::new(sequence.clone(), settings).unwrap();
    let mut pair = PairedReader::open(&sequence, Duration::ZERO)
        .unwrap()
        .next_pair(&cancel)
        .unwrap()
        .unwrap();
    pair.b_pts += 1;
    assert!(renderer.render(&pair, projection, &cancel).is_err());
    pair.b_pts -= 1;
    assert!(renderer.render(&pair, projection, &cancel).is_ok());
    pair.a.set_pts(Some(pair.a_pts + 1));
    assert!(renderer.render(&pair, projection, &cancel).is_err());
    pair.a.set_pts(Some(pair.a_pts));
    let timestamp = pair.timestamp_micros;
    pair.timestamp_micros = 10_000_000;
    assert!(renderer.render(&pair, projection, &cancel).is_err());
    pair.timestamp_micros = timestamp;
    assert!(renderer.render(&pair, projection, &cancel).is_ok());
    pair.chapter_index = 2;
    assert!(renderer.render(&pair, projection, &cancel).is_err());
}
