#![cfg(feature = "media")]

use insta360_rs::{Environment, Housing};

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
    AudioPolicy, EquirectangularProjection, InputSet, MediaAcceleration, ProcessingBackend,
    RecordingSequence, RollingShutterCorrection, Stabilization, StitchConfig, VideoExportOptions,
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
    tail_with_stream_type(index, total, 3)
}

fn tail_with_stream_type(index: u32, total: u32, stream_type: u64) -> Vec<u8> {
    tail_with_motion(index, total, stream_type, false)
}

fn tail_with_motion(index: u32, total: u32, stream_type: u64, motion: bool) -> Vec<u8> {
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
    if stream_type != 0 {
        integer(&mut data, 80, 2);
    }
    integer(&mut data, 131, stream_type);
    integer(&mut data, 88, if total == 1 { 1 } else { 2 });
    let mut group = Vec::new();
    integer(&mut group, 1, 20);
    integer(&mut group, 2, index.into());
    bytes(&mut group, 3, b"synthetic-export-group");
    integer(&mut group, 4, total.into());
    bytes(&mut data, 26, &group);
    let mut motion_records = Vec::new();
    if motion {
        integer(&mut data, 24, 1_100_000 + u64::from(index) * 500_000);
        integer(&mut data, 62, 1); // compact raw IMU samples
        integer(&mut data, 64, 1); // native video PTS + first-frame camera timestamp
        integer(&mut data, 29, 0); // explicitly no gyro adjustment
        let mut range = Vec::new();
        integer(&mut range, 1, 32);
        integer(&mut range, 2, 2000);
        bytes(&mut data, 65, &range);
        let mut gyro = Vec::new();
        for sample in 0_i64..=1200 {
            gyro.extend_from_slice(&(1_000_000 + sample * 1000).to_le_bytes());
            for value in [1024, 0, 0, 1000, 0, 0] {
                gyro.extend_from_slice(&((32768 + value) as u16).to_le_bytes());
            }
        }
        let mut exposure = Vec::new();
        for sample in 0_i64..=12 {
            exposure.extend_from_slice(&(1_000_000 + sample * 100_000).to_le_bytes());
            exposure.extend_from_slice(&0.01_f64.to_le_bytes());
        }
        motion_records.push((3, 0, gyro));
        motion_records.push((4, 0, exposure));
    }
    let mut records = vec![(1, 1, data)];
    records.extend(motion_records);
    let mut payload = Vec::new();
    let mut directory = vec![0u8; 10];
    for (id, format, record) in records {
        directory.extend([id, format]);
        directory.extend((record.len() as u32).to_le_bytes());
        directory.extend((payload.len() as u32).to_le_bytes());
        payload.extend(&record);
        payload.extend([format, id]);
        payload.extend((record.len() as u32).to_le_bytes());
    }
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
    source_with_group(directory, index, total, audio, (index, total))
}

fn source_with_group(
    directory: &Path,
    index: u32,
    total: u32,
    audio: bool,
    group: (u32, u32),
) -> PathBuf {
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
            "-movie_timescale",
            "1000000",
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
        .write_all(&tail(group.0, group.1))
        .unwrap();
    path
}

fn exporter(sequence: RecordingSequence) -> Exporter {
    Exporter::from_sequence(
        sequence,
        StitchConfig {
            housing: Housing::InvisibleDiveCase,
            environment: Environment::Underwater,
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

#[test]
fn exact_stills_replay_chapter_motion_on_late_backward_and_repeated_requests() {
    use insta360_rs::media::RecordingFrameRenderer;
    use std::io::{Seek, SeekFrom};
    use std::sync::atomic::AtomicBool;
    let directory = tempfile::tempdir().unwrap();
    let motion_source = |index, total| {
        let path = source(directory.path(), index, total, false);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let offset = file.metadata().unwrap().len() - tail(index, total).len() as u64;
        file.set_len(offset).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&tail_with_motion(index, total, 3, true))
            .unwrap();
        InputSet::discover(path).unwrap()
    };
    let full = RecordingSequence::single(motion_source(0, 1)).unwrap();
    let split = RecordingSequence::new(vec![motion_source(0, 2), motion_source(1, 2)]).unwrap();
    let settings = StitchConfig {
        housing: Housing::InvisibleDiveCase,
        environment: Environment::Underwater,
        stabilization: Stabilization::DirectionLock,
        rolling_shutter: RollingShutterCorrection::Off,
        backend: ProcessingBackend::Cpu,
        underwater_color: insta360_rs::UnderwaterColorOptions {
            mode: insta360_rs::UnderwaterColorMode::Legacy,
            ..Default::default()
        },
        ..StitchConfig::default()
    };
    let projection = EquirectangularProjection {
        width: 128,
        height: 64,
    };
    let cancel = AtomicBool::new(false);
    let collect = |sequence: &RecordingSequence| {
        let mut reader = insta360_rs::PairedReader::open(sequence, Duration::ZERO).unwrap();
        let mut pairs = Vec::new();
        while let Some(pair) = reader.next_pair(&cancel).unwrap() {
            pairs.push(pair);
        }
        pairs
    };
    let full_pairs = collect(&full);
    let split_pairs = collect(&split);
    assert_eq!(full_pairs.len(), 10);
    assert_eq!(split_pairs.len(), 10);
    let mut reference = RecordingFrameRenderer::new(full, settings.clone()).unwrap();
    let expected: Vec<_> = full_pairs
        .iter()
        .map(|pair| {
            reference
                .render_strict(pair, projection, &cancel)
                .unwrap()
                .frame
        })
        .collect();
    let mut renderer = RecordingFrameRenderer::new(split, settings).unwrap();
    for index in [7, 2, 9, 0, 5, 4, 7, 7] {
        let actual = renderer
            .render_strict(&split_pairs[index], projection, &cancel)
            .unwrap();
        assert_eq!(
            actual.frame.as_rgb8(),
            expected[index].as_rgb8(),
            "frame {index}"
        );
    }
    assert_ne!(
        expected[0].as_rgb8(),
        expected[7].as_rgb8(),
        "fixture must vary over time"
    );
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

#[test]
fn sparse_member_indices_and_submedia_totals_export_available_chapters_continuously() {
    let directory = tempfile::tempdir().unwrap();
    let full = source(directory.path(), 0, 1, false);
    let full = exporter(RecordingSequence::single(InputSet::discover(full).unwrap()).unwrap());
    let whole_path = directory.path().join("whole.mp4");
    assert_eq!(run(&full, &whole_path, options(AudioPolicy::Drop)).0, 10);
    let expected = decoded(&whole_path);

    for (origin, total) in [(0, 0), (37, 4)] {
        let source_directory = directory.path().join(format!("total-{total}"));
        fs::create_dir(&source_directory).unwrap();
        let first = source_with_group(&source_directory, 0, 2, false, (origin, total));
        let second = source_with_group(&source_directory, 1, 2, false, (origin + 2, total));
        let sequence = RecordingSequence::new(vec![
            InputSet::discover(second).unwrap(),
            InputSet::discover(&first).unwrap(),
        ])
        .unwrap();
        assert!(!sequence.complete);
        assert!(sequence.warnings.is_empty());
        let export = exporter(sequence);
        let preflight = export.preflight_video(&options(AudioPolicy::Drop)).unwrap();
        assert_eq!(preflight.chapter_count, 2);
        assert_eq!(preflight.duration, Duration::from_secs(1));
        assert!(preflight.warnings.is_empty());
        let output = source_directory.join("continuous.mp4");
        let (frames, events) = run(&export, &output, options(AudioPolicy::Drop));
        assert_eq!(frames, 10);
        assert_eq!(decoded(&output), expected);
        for is_encoder in [true, false] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| if is_encoder {
                        matches!(event, ExportEvent::EncoderSelected(_))
                    } else {
                        matches!(event, ExportEvent::BackendSelected(_))
                    })
                    .count(),
                1
            );
        }
        if total == 0 {
            let available =
                RecordingSequence::new(vec![InputSet::discover(first).unwrap()]).unwrap();
            assert!(!available.complete);
            assert!(available.warnings.is_empty());
            let output = source_directory.join("available.mp4");
            assert_eq!(
                run(&exporter(available), &output, options(AudioPolicy::Drop)).0,
                5
            );
        }
    }
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

fn assert_audio_matches(actual: &[(i64, Vec<u8>)], expected: &[(i64, Vec<u8>)]) {
    assert_eq!(actual.len(), expected.len(), "copied audio packet count");
    for (index, ((actual_pts, actual_payload), (expected_pts, expected_payload))) in
        actual.iter().zip(expected).enumerate()
    {
        assert_eq!(actual_pts, expected_pts, "audio packet {index} timestamp");
        assert_eq!(
            actual_payload, expected_payload,
            "audio packet {index} payload"
        );
    }
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
    assert_ne!(
        expected[0].0 % 1000,
        0,
        "fixture must exercise a sub-millisecond audio offset"
    );
    assert_audio_matches(
        &actual,
        &expected
            .iter()
            .map(|(pts, _, payload)| (*pts, payload.clone()))
            .collect::<Vec<_>>(),
    );
    let clipped = directory.path().join("clipped-audio.mp4");
    assert_eq!(
        run(
            &exporter,
            &clipped,
            VideoExportOptions {
                start: Some(Duration::from_millis(200)),
                duration: Some(Duration::from_millis(400)),
                ..options(AudioPolicy::Copy)
            }
        )
        .0,
        4
    );
    let expected: Vec<_> = expected
        .into_iter()
        .filter(|(pts, end, _)| *pts >= 200_000 && *end <= 600_000)
        .map(|(pts, _, payload)| (pts - 200_000, payload))
        .collect();
    let actual: Vec<_> = audio_packets(&clipped)
        .into_iter()
        .map(|(pts, _, payload)| (pts, payload))
        .collect();
    assert_ne!(
        expected[0].0 % 1000,
        0,
        "clipped fixture must exercise a sub-millisecond audio offset"
    );
    assert_audio_matches(&actual, &expected);
}

fn with_audio_tracks(directory: &Path, index: u32, codecs: &[&str]) -> PathBuf {
    let video = source(directory, index, 2, false);
    let path = directory.join(format!("audio-tracks-{index}.insv"));
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error", "-i"]).arg(&video);
    for (index, _) in codecs.iter().enumerate() {
        command.args([
            "-f",
            "lavfi",
            "-i",
            &format!(
                "sine=frequency={}:sample_rate=48000:duration=0.5",
                500 + index * 200
            ),
        ]);
    }
    command.args(["-map", "0:v", "-c:v", "copy"]);
    for (index, codec) in codecs.iter().enumerate() {
        command.args([
            "-map",
            &format!("{}:a", index + 1),
            &format!("-c:a:{index}"),
            codec,
            &format!("-metadata:s:a:{index}"),
            if index == 0 {
                "language=eng"
            } else {
                "language=fra"
            },
            &format!("-disposition:a:{index}"),
            if index == 0 { "default" } else { "0" },
        ]);
    }
    let output = command.args(["-f", "mp4"]).arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&tail(index, 2))
        .unwrap();
    path
}

#[derive(Debug, PartialEq, Eq)]
struct AudioTrackSnapshot {
    codec: ffmpeg::codec::Id,
    language: String,
    disposition: i32,
    packets: Vec<(i64, i64, Vec<u8>)>,
}

fn audio_tracks(path: &Path) -> Vec<AudioTrackSnapshot> {
    ffmpeg::init().unwrap();
    let mut input = ffmpeg::format::input(path).unwrap();
    let indices: Vec<_> = input
        .streams()
        .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio)
        .map(|stream| stream.index())
        .collect();
    let mut result: Vec<_> = indices
        .iter()
        .map(|index| {
            let stream = input.stream(*index).unwrap();
            AudioTrackSnapshot {
                codec: stream.parameters().id(),
                language: stream.metadata().get("language").unwrap().to_owned(),
                // SAFETY: the input owns the live stream during this scalar read.
                disposition: unsafe { (*stream.as_ptr()).disposition },
                packets: Vec::new(),
            }
        })
        .collect();
    for (stream, packet) in input.packets() {
        if let Some(index) = indices.iter().position(|index| *index == stream.index()) {
            result[index].packets.push((
                packet
                    .pts()
                    .unwrap()
                    .rescale(stream.time_base(), (1, 1_000_000)),
                (packet.pts().unwrap() + packet.duration())
                    .rescale(stream.time_base(), (1, 1_000_000)),
                packet.data().unwrap().to_vec(),
            ));
        }
    }
    result
}

#[test]
fn mixed_aac_alac_tracks_preserve_packets_tags_and_dispositions_after_nonframe_cut() {
    let directory = tempfile::tempdir().unwrap();
    let first = with_audio_tracks(directory.path(), 0, &["aac", "alac"]);
    let second = with_audio_tracks(directory.path(), 1, &["aac", "alac"]);
    let mut expected = audio_tracks(&first);
    let second_tracks = audio_tracks(&second);
    for (track, second) in expected.iter_mut().zip(second_tracks) {
        track
            .packets
            .retain(|(pts, end, _)| *pts >= 0 && *end <= 500_000);
        track.packets.extend(
            second
                .packets
                .into_iter()
                .filter(|(pts, end, _)| *pts >= 0 && *end <= 500_000)
                .map(|(pts, end, payload)| (pts + 500_000, end + 500_000, payload)),
        );
        // The 250 ms cut's first retained video frame is at 300 ms. Both audio
        // tracks use that same origin, never their independent first packet.
        track
            .packets
            .retain(|(pts, end, _)| *pts >= 300_000 && *end <= 850_000);
        for (pts, end, _) in &mut track.packets {
            *pts -= 300_000;
            *end -= 300_000;
        }
    }
    let exporter = exporter(
        RecordingSequence::new(vec![
            InputSet::discover(first).unwrap(),
            InputSet::discover(second).unwrap(),
        ])
        .unwrap(),
    );
    let options = VideoExportOptions {
        start: Some(Duration::from_millis(250)),
        duration: Some(Duration::from_millis(600)),
        ..options(AudioPolicy::Copy)
    };
    assert_eq!(exporter.preflight_video(&options).unwrap().audio_tracks, 2);
    let output = directory.path().join("mixed.mp4");
    let (frames, events) = run(&exporter, &output, options);
    assert_eq!(frames, 6);
    assert!(events.iter().any(
        |event| matches!(event, ExportEvent::EncoderSelected(report) if report.audio_tracks == 2)
    ));
    let actual = audio_tracks(&output);
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.codec, expected.codec);
        assert_eq!(actual.language, expected.language);
        assert_eq!(actual.disposition, expected.disposition);
        assert!(!actual.packets.is_empty());
        assert_audio_matches(
            &actual
                .packets
                .iter()
                .map(|(pts, _, payload)| (*pts, payload.clone()))
                .collect::<Vec<_>>(),
            &expected
                .packets
                .iter()
                .map(|(pts, _, payload)| (*pts, payload.clone()))
                .collect::<Vec<_>>(),
        );
    }
}

#[test]
fn incompatible_audio_layouts_fail_preflight_and_export_but_can_be_dropped() {
    for codecs in [vec!["alac"], vec!["aac", "alac"], vec!["ac3"]] {
        let directory = tempfile::tempdir().unwrap();
        let first = with_audio_tracks(directory.path(), 0, &["aac"]);
        let second = with_audio_tracks(directory.path(), 1, &codecs);
        let exporter = exporter(
            RecordingSequence::new(vec![
                InputSet::discover(first).unwrap(),
                InputSet::discover(second).unwrap(),
            ])
            .unwrap(),
        );
        let copy = options(AudioPolicy::Copy);
        assert!(matches!(
            exporter.preflight_video(&copy),
            Err(insta360_rs::Error::MissingCapability(_))
        ));
        let output = directory.path().join("copy.mp4");
        assert!(matches!(
            exporter.export_video(&output, copy).wait(),
            Err(insta360_rs::Error::MissingCapability(_))
        ));
        assert!(!output.exists());
        assert!(exporter
            .preflight_video(&options(AudioPolicy::Drop))
            .is_ok());
        assert_eq!(run(&exporter, &output, options(AudioPolicy::Drop)).0, 10);
        assert!(audio_tracks(&output).is_empty());
    }
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
            housing: Housing::None,
            environment: Environment::Air,
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

#[test]
fn preflight_rejects_unknown_lens_order_before_export_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let source = source(directory.path(), 0, 1, false);
    let file = fs::OpenOptions::new().append(true).open(&source).unwrap();
    file.set_len(file.metadata().unwrap().len() - tail(0, 1).len() as u64)
        .unwrap();
    (&file).write_all(&tail_with_stream_type(0, 1, 0)).unwrap();
    let sequence = RecordingSequence::single(InputSet::discover(source).unwrap()).unwrap();
    assert_eq!(
        sequence.chapters[0]
            .inspection
            .metadata
            .reverse_video_track_order,
        None
    );
    let exporter = exporter(sequence);
    let output = directory.path().join("unknown-order.mp4");
    let error = exporter
        .export_video(&output, options(AudioPolicy::Drop))
        .wait()
        .unwrap_err();
    assert!(error.to_string().contains("camera A/B"), "{error}");
    let error = exporter
        .preflight_video(&options(AudioPolicy::Drop))
        .unwrap_err();
    assert!(error.to_string().contains("camera A/B"), "{error}");
    assert!(!output.exists());
}

#[test]
fn repeated_preflight_and_competing_exports_publish_once_without_leaking_temporary_files() {
    let directory = tempfile::tempdir().unwrap();
    let first = source(directory.path(), 0, 2, true);
    let second = source(directory.path(), 1, 2, true);
    let exporter = exporter(
        RecordingSequence::new(vec![
            InputSet::discover(first).unwrap(),
            InputSet::discover(second).unwrap(),
        ])
        .unwrap(),
    );
    let options = options(AudioPolicy::Copy);
    let expected = exporter.preflight_video(&options).unwrap();
    for _ in 0..16 {
        assert_eq!(exporter.preflight_video(&options).unwrap(), expected);
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    let output = directory.path().join("competing.mp4");
    let jobs: Vec<_> = (0..6)
        .map(|_| exporter.export_video(&output, options.clone()))
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|job| job.wait()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .into_iter()
            .find_map(Result::ok)
            .unwrap()
            .frames_written,
        10
    );
    assert_eq!(decoded(&output).len(), 128 * 64 * 3 * 10);
    assert!(!audio_packets(&output).is_empty());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 3);
}
