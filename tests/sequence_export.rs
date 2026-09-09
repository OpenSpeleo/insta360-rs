#![cfg(feature = "media")]

#[allow(dead_code)]
mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ffmpeg::util::mathematics::rescale::Rescale;
use ffmpeg_next as ffmpeg;
use insta360_rs::media::{ExportEvent, Exporter};
use insta360_rs::{
    AudioPolicy, EquirectangularProjection, InputSet, MediaAcceleration, OpticalSetup,
    ProcessingBackend, RecordingSequence, RollingShutterCorrection, Stabilization, StitchConfig,
    VideoExportOptions,
};

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
fn tail(index: u32, total: u32) -> Vec<u8> {
    let mut data = Vec::new();
    bytes(&mut data, 1, b"SEQUENCE-EXPORT-TEST");
    bytes(&mut data, 2, b"Insta360 X5");
    bytes(
        &mut data,
        111,
        common::x5_v6_underwater_calibration(64, 64)
            .raw_offset
            .as_bytes(),
    );
    integer(&mut data, 80, 2);
    integer(&mut data, 131, 3);
    integer(&mut data, 88, if total > 1 { 2 } else { 1 });
    let mut group = Vec::new();
    integer(&mut group, 1, 20);
    integer(&mut group, 2, index.into());
    bytes(&mut group, 3, b"synthetic-export-group");
    integer(&mut group, 4, total.into());
    bytes(&mut data, 26, &group);
    let mut payload = data.clone();
    payload.extend([1, 1]);
    payload.extend((data.len() as u32).to_le_bytes());
    let mut directory = vec![0u8; 20];
    directory[10] = 1;
    directory[11] = 1;
    directory[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
    payload.extend(&directory);
    payload.extend([0, 0]);
    payload.extend((directory.len() as u32).to_le_bytes());
    payload.extend([0u8; 32]);
    payload.extend(((payload.len() + 40) as u32).to_le_bytes());
    payload.extend(3u32.to_le_bytes());
    payload.extend(b"8db42d694ccc418790edff439fe026bf");
    let mut result = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    result.extend(b"inst");
    result.extend(payload);
    result
}

fn source(directory: &Path, index: u32, total: u32, audio: bool) -> PathBuf {
    let path = directory.join(format!("source_{total}_{index}.insv"));
    let start = if total == 1 { 0 } else { index * 5 };
    let end = if total == 1 { 10 } else { start + 5 };
    let filters = format!("[0:v]trim=start_frame={start}:end_frame={end},setpts=PTS-STARTPTS[a];[1:v]trim=start_frame={start}:end_frame={end},setpts=PTS-STARTPTS[b]");
    let mut command = Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=10:duration=1",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=64x64:rate=10:duration=1",
    ]);
    if audio {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=500:sample_rate=48000:duration=1",
        ]);
    }
    command.args(["-filter_complex", &filters, "-map", "[a]", "-map", "[b]"]);
    if audio {
        command.args(["-map", "2:a", "-af", "asetpts=PTS+0.05/TB", "-c:a", "aac"]);
    }
    command
        .args([
            "-c:v",
            "mpeg4",
            "-g",
            "1",
            "-q:v",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-threads",
            "1",
            "-t",
            if total == 1 { "1" } else { "0.5" },
            "-f",
            "mp4",
        ])
        .arg(&path);
    let output = command.output().expect("ffmpeg fixture prerequisite");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&tail(index, total))
        .unwrap();
    path
}

fn exporter(sequence: RecordingSequence) -> Exporter {
    Exporter::from_sequence(
        sequence,
        StitchConfig {
            optical_setup: OpticalSetup::InvisibleDiveCaseUnderwater,
            stabilization: Stabilization::Off,
            rolling_shutter: RollingShutterCorrection::Off,
            backend: ProcessingBackend::Cpu,
            projection: Some(EquirectangularProjection {
                width: 128,
                height: 64,
            }),
            ..StitchConfig::default()
        },
    )
    .unwrap()
}

fn options(audio: AudioPolicy) -> VideoExportOptions {
    VideoExportOptions {
        audio,
        acceleration: MediaAcceleration::Software,
        ..VideoExportOptions::default()
    }
}

fn run(exporter: &Exporter, path: &Path, options: VideoExportOptions) -> (u64, Vec<ExportEvent>) {
    let job = exporter.export_video(path, options);
    let mut events = Vec::new();
    while !job.is_finished() {
        if let Some(event) = job.recv_event_timeout(Duration::from_millis(100)) {
            events.push(event);
        }
    }
    while let Some(event) = job.try_event() {
        events.push(event);
    }
    (job.wait().expect("stitched export").frames_written, events)
}

fn decoded(path: &Path) -> Vec<u8> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn chapter_boundary_matches_unsplit_frames_with_one_encoder_and_global_clipping() {
    let directory = tempfile::tempdir().unwrap();
    let full = source(directory.path(), 0, 1, false);
    let a = source(directory.path(), 0, 2, false);
    let b = source(directory.path(), 1, 2, false);
    let full = exporter(RecordingSequence::single(InputSet::discover(full).unwrap()).unwrap());
    let sequence = exporter(
        RecordingSequence::new(vec![
            InputSet::discover(a).unwrap(),
            InputSet::discover(b).unwrap(),
        ])
        .unwrap(),
    );
    let whole_path = directory.path().join("whole.mp4");
    let split_path = directory.path().join("split.mp4");
    assert_eq!(run(&full, &whole_path, options(AudioPolicy::Drop)).0, 10);
    let (frames, events) = run(&sequence, &split_path, options(AudioPolicy::Drop));
    assert_eq!(frames, 10);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExportEvent::EncoderSelected(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExportEvent::BackendSelected(_)))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(event, ExportEvent::EncoderSelected(report) if !report.hardware && report.audio_tracks == 0)));
    assert_eq!(decoded(&whole_path), decoded(&split_path));
    let clip = VideoExportOptions {
        start: Some(Duration::from_millis(300)),
        duration: Some(Duration::from_millis(400)),
        ..options(AudioPolicy::Drop)
    };
    let report = sequence.preflight_video(&clip).unwrap();
    assert_eq!(report.chapter_count, 2);
    assert_eq!(report.duration, Duration::from_millis(400));
    let clip_path = directory.path().join("clip.mp4");
    assert_eq!(run(&sequence, &clip_path, clip).0, 4);
}

fn audio_packets(path: &Path) -> Vec<(i64, i64, Vec<u8>)> {
    ffmpeg::init().unwrap();
    let mut input = ffmpeg::format::input(path).unwrap();
    let mut packets = Vec::new();
    for (stream, packet) in input.packets() {
        if stream.parameters().medium() != ffmpeg::media::Type::Audio {
            continue;
        }
        let time = packet
            .pts()
            .unwrap()
            .rescale(stream.time_base(), (1, 1_000_000));
        let end =
            (packet.pts().unwrap() + packet.duration()).rescale(stream.time_base(), (1, 1_000_000));
        packets.push((time, end, packet.data().unwrap().to_vec()));
    }
    packets
}

#[test]
fn audio_copy_preserves_packet_payload_order_and_video_offset_across_chapters() {
    let directory = tempfile::tempdir().unwrap();
    let a = source(directory.path(), 0, 2, true);
    let b = source(directory.path(), 1, 2, true);
    let mut expected = Vec::new();
    for (index, path) in [&a, &b].into_iter().enumerate() {
        expected.extend(
            audio_packets(path)
                .into_iter()
                .filter(|(pts, end, _)| *pts >= 0 && *end <= 500_000)
                .map(|(pts, end, payload)| {
                    (
                        pts + index as i64 * 500_000,
                        end + index as i64 * 500_000,
                        payload,
                    )
                }),
        );
    }
    let exporter = exporter(
        RecordingSequence::new(vec![
            InputSet::discover(a).unwrap(),
            InputSet::discover(b).unwrap(),
        ])
        .unwrap(),
    );
    assert_eq!(
        exporter
            .preflight_video(&options(AudioPolicy::Copy))
            .unwrap()
            .audio_tracks,
        1
    );
    let path = directory.path().join("audio.mp4");
    let (_, events) = run(&exporter, &path, options(AudioPolicy::Copy));
    assert!(events.iter().any(
        |event| matches!(event, ExportEvent::EncoderSelected(report) if report.audio_tracks == 1)
    ));
    let actual: Vec<_> = audio_packets(&path)
        .into_iter()
        .map(|(pts, _, payload)| (pts, payload))
        .collect();
    assert!(!actual.is_empty());
    assert!(
        actual[0].0 > 0,
        "audio retains its positive offset from the video origin"
    );
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|(pts, _, payload)| (*pts, payload.clone()))
            .collect::<Vec<_>>()
    );
    let clipped = directory.path().join("clipped-audio.mp4");
    assert_eq!(
        run(
            &exporter,
            &clipped,
            VideoExportOptions {
                start: Some(Duration::from_millis(300)),
                duration: Some(Duration::from_millis(400)),
                ..options(AudioPolicy::Copy)
            }
        )
        .0,
        4
    );
    let expected: Vec<_> = expected
        .into_iter()
        .filter(|(pts, end, _)| *pts >= 300_000 && *end <= 700_000)
        .map(|(pts, _, payload)| (pts - 300_000, payload))
        .collect();
    let actual: Vec<_> = audio_packets(&clipped)
        .into_iter()
        .map(|(pts, _, payload)| (pts, payload))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn silent_copy_preflight_validation_cancellation_and_no_clobber_are_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let source = source(directory.path(), 0, 1, false);
    let exporter =
        exporter(RecordingSequence::single(InputSet::discover(source).unwrap()).unwrap());
    let report = exporter
        .preflight_video(&options(AudioPolicy::Copy))
        .unwrap();
    assert_eq!(report.audio_tracks, 0);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("no audio")));
    let invalid = VideoExportOptions {
        quality: 0,
        ..options(AudioPolicy::Drop)
    };
    assert!(exporter.preflight_video(&invalid).is_err());
    let path = directory.path().join("cancel.mp4");
    let job = exporter.export_video(&path, options(AudioPolicy::Drop));
    job.cancel();
    assert!(job.wait().is_err());
    assert!(!path.exists());
    let path = directory.path().join("existing.mp4");
    fs::write(&path, b"original").unwrap();
    assert!(exporter
        .export_video(&path, options(AudioPolicy::Drop))
        .wait()
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".insta360-rs-part")));
}

#[cfg(feature = "gpu")]
#[test]
fn configured_real_x5_gpu_audio_smoke() {
    let Ok(source) = std::env::var("INSTA360_RS_X5_SAMPLE") else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let exporter = Exporter::new(
        InputSet::new(vec![source.into()]).unwrap(),
        StitchConfig {
            optical_setup: OpticalSetup::BareAir,
            backend: ProcessingBackend::Gpu,
            projection: Some(EquirectangularProjection {
                width: 640,
                height: 320,
            }),
            ..StitchConfig::default()
        },
    )
    .unwrap();
    let path = directory.path().join("real-gpu-audio.mp4");
    let (count, events) = run(
        &exporter,
        &path,
        VideoExportOptions {
            start: Some(Duration::from_secs(10)),
            duration: Some(Duration::from_millis(100)),
            audio: AudioPolicy::Copy,
            acceleration: MediaAcceleration::Auto,
            ..VideoExportOptions::default()
        },
    );
    assert!(count > 0);
    assert!(events.iter().any(|event| matches!(event, ExportEvent::BackendSelected(report) if report.selected == insta360_rs::EffectiveBackend::Gpu)));
    assert!(!decoded(&path).is_empty());
    assert!(!audio_packets(&path).is_empty());
}
