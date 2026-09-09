#![cfg(feature = "media")]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ffmpeg::util::mathematics::rescale::Rescale;
use ffmpeg_next as ffmpeg;
use insta360_rs::extraction::{extract_controlled, extract_sequence, ExtractionPhase};
use insta360_rs::RecordingSequence;
use insta360_rs::{extract, InputSet, MediaSource, StreamKind};
use serde_json::Value;
use tempfile::tempdir;

fn fixture(path: &Path) -> Option<Vec<u8>> {
    let result = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=48x32:rate=6",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=48x32:r=6",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-map",
            "0:v",
            "-map",
            "1:v",
            "-map",
            "2:a",
            "-t",
            "1.2",
            "-c:v",
            "mpeg4",
            "-bf",
            "2",
            "-g",
            "6",
            "-c:a",
            "aac",
            "-metadata",
            "title=Preserved recording",
            "-timecode",
            "00:00:00:00",
            "-f",
            "mp4",
        ])
        .arg(path)
        .output();
    let result = match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping generated-media test: ffmpeg executable unavailable");
            return None;
        }
        result => result.expect("run fixture generator"),
    };
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let metadata = b"\x12\x0eUnknown Camera";
    let mut payload = Vec::new();
    // Real indexed tails reserve all-zero slots for absent record IDs.
    let mut directory = vec![0; 10];
    for (id, format, bytes) in [
        (1_u8, 1_u8, metadata.as_slice()),
        (0xee, 7, b"opaque first".as_slice()),
        (0xee, 9, b"opaque second".as_slice()),
    ] {
        directory.extend([id, format]);
        directory.extend((bytes.len() as u32).to_le_bytes());
        directory.extend((payload.len() as u32).to_le_bytes());
        payload.extend(bytes);
        payload.extend([format, id]);
        payload.extend((bytes.len() as u32).to_le_bytes());
    }
    directory.extend([0; 10]);
    payload.extend(&directory);
    payload.extend([0, 0]);
    payload.extend((directory.len() as u32).to_le_bytes());
    let size = payload.len() + 72;
    payload.extend([0x5a; 32]);
    payload.extend((size as u32).to_le_bytes());
    payload.extend(3_u32.to_le_bytes());
    payload.extend(b"8db42d694ccc418790edff439fe026bf");
    let mut tail = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    tail.extend(b"inst");
    tail.extend(payload);
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append tail")
        .write_all(&tail)
        .expect("write tail");
    Some(tail)
}

fn packet_payloads(path: &Path) -> BTreeMap<usize, Vec<Vec<u8>>> {
    ffmpeg::init().expect("FFmpeg");
    let mut input = ffmpeg::format::input(path).expect("open packets");
    let mut packets = BTreeMap::<usize, Vec<Vec<u8>>>::new();
    loop {
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut input) {
            Ok(()) => packets
                .entry(packet.stream())
                .or_default()
                .push(packet.data().unwrap_or_default().to_vec()),
            Err(ffmpeg::Error::Eof) => break,
            Err(error) => panic!("packet read failed: {error}"),
        }
    }
    packets
}

fn two_part_sequence(first: &Path, second: &Path) -> RecordingSequence {
    let mut sequence = RecordingSequence::single(InputSet::new(vec![first.into()]).expect("input"))
        .expect("first part");
    sequence.chapters[0]
        .inspection
        .metadata
        .reverse_video_track_order = Some(false);
    let mut next = RecordingSequence::single(InputSet::new(vec![second.into()]).expect("input"))
        .expect("second part")
        .chapters
        .remove(0);
    next.inspection.metadata.reverse_video_track_order = Some(false);
    next.timeline_start = sequence.duration;
    sequence.duration += next.duration;
    sequence.chapters.push(next);
    sequence
}

#[test]
fn controlled_unpack_reports_bytes_and_cancel_keeps_the_empty_target() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("recording.insv");
    if fixture(&source).is_none() {
        return;
    }
    let inputs = InputSet::new(vec![source.clone()]).expect("inputs");
    let output = temp.path().join("output");
    fs::create_dir(&output).expect("empty output");
    let cancelled = AtomicBool::new(false);
    let result = extract_controlled(&inputs, &output, &cancelled, |progress| {
        if progress.phase == ExtractionPhase::Streams {
            cancelled.store(true, Ordering::Relaxed);
        }
    });
    assert!(matches!(result, Err(insta360_rs::Error::Cancelled)));
    assert_eq!(fs::read_dir(&output).expect("retained target").count(), 0);
    assert_eq!(fs::read_dir(temp.path()).expect("no staging").count(), 2);
    cancelled.store(false, Ordering::Relaxed);
    let mut events = Vec::new();
    let report = extract_controlled(&inputs, &output, &cancelled, |progress| {
        events.push(progress)
    })
    .expect("retry");
    assert!(events.len() >= 4);
    assert!(events
        .windows(2)
        .all(|p| p[1].bytes_copied >= p[0].bytes_copied));
    assert!(events.last().expect("completion").completed);
    assert!(events.last().expect("completion").bytes_copied > 0);
    assert!(report.manifest_path.exists());
}

#[test]
fn sequence_unpack_preserves_each_archive_and_joins_video_without_reencoding() {
    let temp = tempdir().expect("tempdir");
    let first = temp.path().join("first.insv");
    let Some(tail) = fixture(&first) else {
        return;
    };
    let second = temp.path().join("second.insv");
    fs::copy(&first, &second).expect("second original");
    let original = fs::read(&first).expect("original");
    let expected = packet_payloads(&first);
    let sequence = two_part_sequence(&first, &second);
    let output = temp.path().join("output");
    let report = extract_sequence(&sequence, &output, &AtomicBool::new(false), |_| {})
        .expect("unpack sequence");
    assert_eq!(report.input_count, 2);
    assert_eq!(report.record_count, 6);
    let manifest: Value =
        serde_json::from_slice(&fs::read(&report.manifest_path).expect("manifest")).expect("json");
    assert_eq!(manifest["schema_version"], 2);
    for part in ["0001", "0002"] {
        let base = output.join(format!("parts/{part}/input-00"));
        assert_eq!(
            fs::read(base.join("extra-info/tail.bin")).expect("complete tail"),
            tail
        );
        for (stream, packets) in &expected {
            assert_eq!(
                fs::read(base.join(format!("streams/{stream:03}/packets.bin")))
                    .expect("raw payload"),
                packets.concat()
            );
            assert!(
                !base.join(format!("streams/{stream:03}/media.mp4")).exists(),
                "no redundant chapter playable copy"
            );
        }
    }
    for (index, name) in [(0, "camera_A.mp4"), (1, "camera_B.mp4")] {
        let copied = packet_payloads(&output.join(name));
        let expected = expected[&index]
            .iter()
            .chain(expected[&index].iter())
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            copied[&0], expected,
            "continuous encoded camera payloads are unchanged"
        );
        let mut original_video = ffmpeg::format::input(&first).expect("source video");
        let time_base = original_video
            .stream(index)
            .expect("camera stream")
            .time_base();
        let offset = (sequence.chapters[1].timeline_start.as_nanos() as i64)
            .rescale((1, 1_000_000_000), time_base);
        let original_clock = original_video
            .packets()
            .filter(|(stream, _)| stream.index() == index)
            .map(|(_, packet)| {
                (
                    packet.pts().expect("source PTS"),
                    packet.dts().expect("source DTS"),
                )
            })
            .collect::<Vec<_>>();
        let expected_clock = original_clock
            .iter()
            .copied()
            .chain(
                original_clock
                    .iter()
                    .map(|(pts, dts)| (pts + offset, dts + offset)),
            )
            .collect::<Vec<_>>();
        let mut video = ffmpeg::format::input(&output.join(name)).expect("open copied camera");
        let copied_clock = video
            .packets()
            .map(|(stream, packet)| {
                (
                    packet
                        .pts()
                        .expect("PTS")
                        .rescale(stream.time_base(), time_base),
                    packet
                        .dts()
                        .expect("DTS")
                        .rescale(stream.time_base(), time_base),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            copied_clock, expected_clock,
            "shared camera timestamps survive MP4 fragmentation"
        );
        assert!(copied_clock.windows(2).all(|pair| pair[1].1 > pair[0].1));
    }
    assert!(
        output.join("audio_001.m4a").exists(),
        "{:?}",
        report.warnings
    );
    let mut original_audio = ffmpeg::format::input(&first).expect("source audio");
    let time_base = original_audio.stream(2).expect("audio track").time_base();
    let original_audio = original_audio
        .packets()
        .filter(|(stream, _)| stream.index() == 2)
        .map(|(_, packet)| {
            (
                packet.pts().expect("audio PTS"),
                packet.data().expect("payload").to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let offset = (sequence.chapters[1].timeline_start.as_nanos() as i64)
        .rescale((1, 1_000_000_000), time_base);
    let limit =
        (sequence.chapters[0].duration.as_nanos() as i64).rescale((1, 1_000_000_000), time_base);
    let expected_audio = original_audio
        .iter()
        .filter(|(pts, _)| *pts < limit)
        .cloned()
        .chain(
            original_audio
                .iter()
                .filter(|(pts, _)| *pts >= 0 && *pts < limit)
                .map(|(pts, data)| (pts + offset, data.clone())),
        )
        .collect::<Vec<_>>();
    let mut copied_audio =
        ffmpeg::format::input(&output.join("audio_001.m4a")).expect("continuous audio");
    let copied_audio = copied_audio
        .packets()
        .map(|(stream, packet)| {
            (
                packet
                    .pts()
                    .expect("copied audio PTS")
                    .rescale(stream.time_base(), time_base),
                packet.data().expect("copied payload").to_vec(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        copied_audio.len(),
        expected_audio.len(),
        "audio packet count"
    );
    for (index, (copied, expected)) in copied_audio.iter().zip(&expected_audio).enumerate() {
        assert_eq!(copied.0, expected.0, "audio packet {index} PTS");
        assert!(
            copied.1 == expected.1,
            "audio packet {index} payload unchanged"
        );
    }
    assert_eq!(fs::read(&first).expect("first unchanged"), original);
    assert_eq!(fs::read(&second).expect("second unchanged"), original);
    assert!(report.files.iter().all(|path| path.exists()));
}

#[test]
fn incompatible_continuous_cameras_preserve_the_complete_original_archives() {
    let temp = tempdir().expect("tempdir");
    let first = temp.path().join("first.insv");
    let Some(tail) = fixture(&first) else {
        return;
    };
    let second = temp.path().join("second.insv");
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&first)
        .args([
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-vf",
            "scale=64:48",
            "-c:v",
            "mpeg4",
            "-bf",
            "2",
            "-c:a",
            "copy",
            "-f",
            "mp4",
        ])
        .arg(&second)
        .output()
        .expect("changed codec fixture");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&second)
        .expect("open tail")
        .write_all(&tail)
        .expect("append tail");
    let expected = packet_payloads(&second);
    let output = temp.path().join("output");
    let report = extract_sequence(
        &two_part_sequence(&first, &second),
        &output,
        &AtomicBool::new(false),
        |_| {},
    )
    .expect("raw extraction succeeds without compatible convenience video");
    assert!(!output.join("camera_A.mp4").exists());
    assert!(!output.join("camera_B.mp4").exists());
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("codec configuration changes")));
    for (index, packets) in expected {
        assert_eq!(
            fs::read(output.join(format!(
                "parts/0002/input-00/streams/{index:03}/packets.bin"
            )))
            .expect("raw packets"),
            packets.concat()
        );
    }
    assert_eq!(
        fs::read(output.join("parts/0002/input-00/extra-info/tail.bin")).expect("raw metadata"),
        tail
    );
}

#[test]
fn sequence_cancellation_never_publishes_a_partial_camera_copy() {
    let temp = tempdir().expect("tempdir");
    let first = temp.path().join("first.insv");
    if fixture(&first).is_none() {
        return;
    }
    let second = temp.path().join("second.insv");
    fs::copy(&first, &second).expect("second original");
    let sequence = two_part_sequence(&first, &second);
    let cancelled = AtomicBool::new(false);
    let output = temp.path().join("output");
    let result = extract_sequence(&sequence, &output, &cancelled, |progress| {
        if progress.input_index == 1 {
            cancelled.store(true, Ordering::Relaxed);
        }
    });
    assert!(matches!(result, Err(insta360_rs::Error::Cancelled)));
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).expect("no staging").count(), 2);
}

#[test]
fn extracts_all_tracks_opaque_records_and_byte_identical_encoded_packets() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("recording.insv");
    let Some(tail) = fixture(&source) else {
        return;
    };
    let original = fs::read(&source).expect("original");
    let expected_packets = packet_payloads(&source);
    let output = temp.path().join("extracted");
    let report = extract(
        &InputSet::new(vec![source.clone()]).expect("inputs"),
        &output,
    )
    .expect("extract without camera calibration");
    assert_eq!(fs::read(&source).expect("unchanged input"), original);
    assert_eq!(report.input_count, 1);
    assert_eq!(report.record_count, 3);
    assert!(report.files.iter().all(|path| path.is_file()));
    assert_eq!(
        fs::read(output.join("input-00/extra-info/tail.bin")).expect("tail"),
        tail
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(&report.manifest_path).expect("manifest")).expect("JSON");
    assert_eq!(manifest["schema_version"], 1);
    let base = output.join("input-00");
    let streams = manifest["inputs"][0]["media"]["streams"]
        .as_array()
        .expect("streams");
    assert_eq!(streams.len(), report.stream_count);
    assert_eq!(
        streams
            .iter()
            .filter(|stream| stream["type"] == "video")
            .count(),
        2
    );
    assert_eq!(
        streams
            .iter()
            .filter(|stream| stream["type"] == "audio")
            .count(),
        1
    );
    assert!(streams.iter().any(|stream| stream["type"] == "data"));
    for stream in streams {
        let index = stream["index"].as_u64().expect("index") as usize;
        let payload = fs::read(base.join(stream["packet_payload"].as_str().expect("payload path")))
            .expect("raw packets");
        let rows =
            fs::read_to_string(base.join(stream["packet_index"].as_str().expect("index path")))
                .expect("packet index");
        let indexed: Vec<Value> = rows
            .lines()
            .map(|line| serde_json::from_str(line).expect("packet JSON"))
            .collect();
        let expected = expected_packets.get(&index).cloned().unwrap_or_default();
        assert_eq!(payload, expected.concat(), "stream {index} bytes");
        assert_eq!(indexed.len(), expected.len(), "stream {index} packet count");
        if matches!(stream["type"].as_str(), Some("video" | "audio")) {
            let playable = base.join(stream["media_file"].as_str().expect("playable copy"));
            let actual = packet_payloads(&playable);
            assert_eq!(actual.len(), 1, "single-track media copy");
            assert_eq!(actual[&0], expected, "stream {index} was not re-encoded");
        }
    }
    let records = manifest["inputs"][0]["container"]["records"]
        .as_array()
        .expect("records");
    assert_eq!(
        records.iter().filter(|record| record["id"] == 238).count(),
        2
    );
    assert!(report
        .files
        .iter()
        .any(|path| fs::read(path).expect("artifact") == b"opaque second"));
}

#[test]
fn failed_second_input_never_publishes_partial_first_input() {
    let temp = tempdir().expect("tempdir");
    let primary = temp.path().join("VID_20260101_120000_00_001.insv");
    if fixture(&primary).is_none() {
        return;
    }
    let secondary = temp.path().join("VID_20260101_120000_10_001.insv");
    fs::write(&secondary, b"invalid media").expect("second input");
    let output = temp.path().join("extracted");
    let inputs = InputSet::new(vec![secondary, primary]).expect("pair");
    assert!(extract(&inputs, &output).is_err());
    assert!(!output.exists());
    assert_eq!(
        fs::read_dir(temp.path())
            .expect("no staging remains")
            .count(),
        2
    );
}

#[test]
fn direct_streams_read_packets_decode_and_seek_without_output_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("recording.insv");
    if fixture(&source).is_none() {
        return;
    }
    let original = fs::read(&source).expect("source bytes");
    let expected_packets = packet_payloads(&source);
    let media = MediaSource::open(InputSet::new(vec![source.clone()]).expect("inputs"))
        .expect("direct source");
    let videos = media.video_streams().collect::<Vec<_>>();
    assert_eq!(videos.len(), 2);
    let first = videos[0];
    assert_eq!((first.info().width, first.info().height), (48, 32));
    let mut packets = first.open_packets().expect("packet reader");
    let mut independent = first.open_packets().expect("independent packet reader");
    let first_packet = packets.read_packet().expect("read").expect("first packet");
    assert_eq!(
        independent
            .read_packet()
            .expect("read")
            .expect("packet")
            .data,
        first_packet.data
    );
    let mut encoded = vec![first_packet.data];
    while let Some(packet) = packets.read_packet().expect("remaining packets") {
        encoded.push(packet.data);
    }
    assert_eq!(encoded, expected_packets[&first.info().stream_index]);
    assert!(packets.read_packet().expect("stable EOF").is_none());
    packets
        .seek(Duration::ZERO)
        .expect("seek packets after EOF");
    assert!(packets.read_packet().expect("read after seek").is_some());
    assert!(packets.seek(Duration::MAX).is_err());

    let mut frames = first.open_video().expect("video decoder");
    let mut decoded = Vec::new();
    while let Some(frame) = frames.read_frame().expect("decode frame") {
        assert_eq!(frame.data.len(), 48 * 32 * 3);
        decoded.push(frame);
    }
    assert_eq!(
        decoded.len(),
        encoded.len(),
        "decoder drains delayed B-frames"
    );
    assert!(decoded
        .windows(2)
        .all(|pair| pair[0].timestamp < pair[1].timestamp));
    assert!(frames.read_frame().expect("stable frame EOF").is_none());
    let target = Duration::from_millis(700);
    let sought = frames
        .frame_at(target)
        .expect("seek after EOF")
        .expect("frame");
    assert!(sought.timestamp.expect("timestamp") >= target);
    let replay = frames
        .frame_at(Duration::ZERO)
        .expect("seek backwards")
        .expect("frame");
    assert_eq!(replay.data, decoded[0].data);
    assert!(frames
        .frame_at(Duration::from_secs(100))
        .expect("seek beyond last frame")
        .is_none());
    assert!(frames
        .frame_at(Duration::ZERO)
        .expect("seek back from beyond EOF")
        .is_some());

    let mut second = videos[1].open_video().expect("second lens decoder");
    assert_ne!(
        second.read_frame().expect("read").expect("frame").data,
        decoded[0].data
    );
    let audio = media
        .streams()
        .iter()
        .find(|stream| stream.info().kind == StreamKind::Audio)
        .expect("audio stream");
    assert!(audio.open_video().is_err());
    assert!(audio
        .open_packets()
        .expect("audio packets")
        .read_packet()
        .expect("read")
        .is_some());
    assert_eq!(
        fs::read_dir(temp.path()).expect("no intermediates").count(),
        1
    );
    assert_eq!(fs::read(&source).expect("unchanged source"), original);
}

#[test]
fn direct_frame_timestamps_and_seeks_are_relative_to_nonzero_stream_start() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("recording.insv");
    if fixture(&source).is_none() {
        return;
    }
    let shifted = temp.path().join("shifted.insv");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&source)
        .args([
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-c",
            "copy",
            "-output_ts_offset",
            "5",
            "-f",
            "mp4",
        ])
        .arg(&shifted)
        .output()
        .expect("shift time origin");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let media = MediaSource::open(InputSet::new(vec![shifted]).expect("input"))
        .expect("open without ExtraInfo");
    for video in media.video_streams() {
        assert!(video.info().start_time.expect("nonzero start") > 0);
        let mut frames = video.open_video().expect("reader");
        let first = frames
            .frame_at(Duration::ZERO)
            .expect("seek")
            .expect("frame");
        assert_eq!(first.timestamp, Some(Duration::ZERO));
        let target = Duration::from_millis(700);
        let frame = frames.frame_at(target).expect("seek").expect("frame");
        assert!(frame.timestamp.expect("timestamp") >= target);
        assert!(frame.timestamp.expect("timestamp") < Duration::from_secs(2));
    }
    assert_eq!(
        fs::read_dir(temp.path())
            .expect("no intermediate files")
            .count(),
        2
    );
}

#[cfg(feature = "cli")]
#[test]
fn cli_extract_returns_a_completed_manifest() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("recording.insv");
    if fixture(&source).is_none() {
        return;
    }
    let output = temp.path().join("extracted");
    let command = Command::new(env!("CARGO_BIN_EXE_insta360-rs"))
        .arg("extract")
        .arg(&source)
        .arg(&output)
        .arg("--json")
        .output()
        .expect("CLI");
    assert!(
        command.status.success(),
        "{}",
        String::from_utf8_lossy(&command.stderr)
    );
    let report: insta360_rs::ExtractionReport =
        serde_json::from_slice(&command.stdout).expect("report");
    assert!(report.manifest_path.is_file());
    assert!(report.stream_count >= 4);
    assert_eq!(report.record_count, 3);
}
