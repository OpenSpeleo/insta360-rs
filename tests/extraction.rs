#![cfg(feature = "media")]

use std::collections::{BTreeMap, BTreeSet};
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
    fixture_with_records(
        path,
        &[
            (1, 1, b"\x12\x0eUnknown Camera".to_vec()),
            (0xee, 7, b"opaque first".to_vec()),
            (0xee, 9, b"opaque second".to_vec()),
        ],
    )
}

fn fixture_with_records(path: &Path, records: &[(u8, u8, Vec<u8>)]) -> Option<Vec<u8>> {
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

    let mut payload = Vec::new();
    // Real indexed tails reserve all-zero slots for absent record IDs.
    let mut directory = vec![0; 10];
    for (id, format, bytes) in records {
        directory.extend([*id, *format]);
        directory.extend((bytes.len() as u32).to_le_bytes());
        directory.extend((payload.len() as u32).to_le_bytes());
        payload.extend(bytes);
        payload.extend([*format, *id]);
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

fn protobuf_varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        output.push(value as u8 | 128);
        value >>= 7;
    }
    output.push(value as u8);
}

fn protobuf_integer(output: &mut Vec<u8>, tag: u32, value: u64) {
    protobuf_varint(output, u64::from(tag) * 8);
    protobuf_varint(output, value);
}

fn telemetry_records(epoch_micros: i64, blend_angle: u64, mapping: u64) -> Vec<(u8, u8, Vec<u8>)> {
    let mut metadata = b"\x12\x0eUnknown Camera".to_vec();
    protobuf_integer(&mut metadata, 24, epoch_micros as u64);
    protobuf_integer(&mut metadata, 29, 0);
    protobuf_integer(&mut metadata, 62, 1);
    protobuf_integer(&mut metadata, 64, mapping);
    protobuf_integer(&mut metadata, 128, blend_angle);
    // 32 g and 2000 degrees/s; no camera calibration is needed for extraction.
    let range = [8, 32, 16, 0xd0, 0x0f];
    protobuf_varint(&mut metadata, 65 * 8 + 2);
    protobuf_varint(&mut metadata, range.len() as u64);
    metadata.extend(range);
    let mut motion = Vec::new();
    let mut exposure = Vec::new();
    for delta in [-20_000_i64, 50_000, 150_000] {
        let timestamp = epoch_micros + delta;
        motion.extend(timestamp.to_le_bytes());
        for value in [33_792_u16, 32_768, 32_768, 32_768, 33_792, 32_768] {
            motion.extend(value.to_le_bytes());
        }
        exposure.extend(timestamp.to_le_bytes());
        exposure.extend(0.01_f64.to_le_bytes());
    }
    vec![
        (1, 1, metadata),
        (3, 0, motion),
        (4, 0, exposure),
        (0xee, 7, format!("opaque first {epoch_micros}").into_bytes()),
        (
            0xee,
            9,
            format!("opaque second {epoch_micros}").into_bytes(),
        ),
    ]
}

fn json_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("JSONL output")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSONL row"))
        .collect()
}

const ARCHIVE_CATEGORIES: [&str; 5] = [
    "calibration",
    "container",
    "extra-info",
    "metadata",
    "streams",
];

fn artifact_bytes(output: &Path, reference: &Value) -> Vec<u8> {
    let path = Path::new(reference["path"].as_str().expect("artifact payload path"));
    assert!(path.is_relative());
    assert!(path.starts_with("metadata"));
    assert!(!path
        .components()
        .any(|component| component == std::path::Component::ParentDir));
    let bytes = fs::read(output.join(path)).expect("consolidated payload");
    let offset = usize::try_from(reference["offset"].as_u64().expect("artifact offset"))
        .expect("offset fits");
    let size =
        usize::try_from(reference["size"].as_u64().expect("artifact size")).expect("size fits");
    assert!(
        offset <= bytes.len() && size <= bytes.len() - offset,
        "artifact slice bounds: {reference}"
    );
    bytes[offset..offset + size].to_vec()
}

fn artifact_json_lines(output: &Path, reference: &Value) -> Vec<Value> {
    String::from_utf8(artifact_bytes(output, reference))
        .expect("artifact JSONL")
        .lines()
        .map(|line| serde_json::from_str(line).expect("artifact JSONL row"))
        .collect()
}

fn archived_artifact(output: &Path, category: &str, chapter: usize, original_path: &str) -> Value {
    let entries = json_lines(&output.join(format!("metadata/{category}/artifacts.jsonl")));
    let found = entries
        .iter()
        .filter(|entry| {
            entry["chapter_index"] == chapter
                && entry["input_index"] == 0
                && entry["original_path"] == original_path
        })
        .collect::<Vec<_>>();
    assert_eq!(
        found.len(),
        1,
        "one artifact for chapter {chapter} {original_path}"
    );
    found[0]["slice"].clone()
}

fn assert_artifact_references(output: &Path, value: &Value) {
    match value {
        Value::Object(object)
            if object.contains_key("path")
                && object.contains_key("offset")
                && object.contains_key("size") =>
        {
            artifact_bytes(output, value);
        }
        Value::Object(object) => {
            for (key, value) in object {
                assert_ne!(
                    key, "source_directory",
                    "no stale source directory references"
                );
                assert_artifact_references(output, value);
            }
        }
        Value::Array(array) => {
            for value in array {
                assert_artifact_references(output, value);
            }
        }
        Value::String(string) => {
            assert!(!string.starts_with("parts/"), "stale part path: {string}")
        }
        _ => {}
    }
}

fn assert_merged_layout(output: &Path) {
    let root_dirs = fs::read_dir(output)
        .expect("output entries")
        .map(|entry| entry.expect("entry"))
        .filter(|entry| entry.file_type().expect("entry type").is_dir())
        .map(|entry| entry.file_name().into_string().expect("directory name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(root_dirs, BTreeSet::from(["metadata".to_owned()]));
    let categories = fs::read_dir(output.join("metadata"))
        .expect("metadata categories")
        .map(|entry| entry.expect("category"))
        .map(|entry| {
            assert!(entry.file_type().expect("category type").is_dir());
            entry.file_name().into_string().expect("category name")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        categories,
        ARCHIVE_CATEGORIES.map(str::to_owned).into_iter().collect()
    );
    for category in ARCHIVE_CATEGORIES {
        let directory = output.join("metadata").join(category);
        assert!(directory.join("payload.bin").is_file());
        assert!(directory.join("artifacts.jsonl").is_file());
        assert!(
            fs::read_dir(directory)
                .expect("category entries")
                .all(|entry| entry
                    .expect("entry")
                    .file_type()
                    .expect("entry type")
                    .is_file()),
            "no per-part subdirectories inside {category}"
        );
    }
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
fn sequence_unpack_consolidates_all_artifacts_and_joins_video_without_reencoding() {
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
    assert_eq!(manifest["schema_version"], 3);
    assert_merged_layout(&output);
    assert_artifact_references(&output, &manifest);
    // The standalone extractor is an independent reference for every original
    // artifact, including stream indexes, codec configuration and unknown data.
    let reference = temp.path().join("reference");
    let reference_report = extract(
        &InputSet::new(vec![first.clone()]).expect("reference input"),
        &reference,
    )
    .expect("reference extraction");
    let reference_base = fs::canonicalize(reference.join("input-00")).expect("reference base");
    let expected_artifacts = reference_report
        .files
        .iter()
        .filter_map(|path| path.strip_prefix(&reference_base).ok())
        .filter(|path| {
            !path
                .file_name()
                .expect("artifact name")
                .to_string_lossy()
                .starts_with("media.")
        })
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect::<BTreeSet<_>>();
    let mut archived_paths = [BTreeSet::new(), BTreeSet::new()];
    for category in ARCHIVE_CATEGORIES {
        let artifacts = json_lines(&output.join(format!("metadata/{category}/artifacts.jsonl")));
        for artifact in &artifacts {
            let chapter = artifact["chapter_index"].as_u64().expect("chapter") as usize;
            assert!(chapter < sequence.chapters.len());
            assert_eq!(artifact["input_index"], 0);
            assert_eq!(
                artifact["timeline_start_ns"],
                sequence.chapters[chapter]
                    .timeline_start
                    .as_nanos()
                    .to_string()
            );
            assert_eq!(
                artifact["duration_ns"],
                sequence.chapters[chapter].duration.as_nanos().to_string()
            );
            let original_path = artifact["original_path"]
                .as_str()
                .expect("original artifact name");
            assert!(
                archived_paths[chapter].insert(original_path.to_owned()),
                "artifact indexed once per source"
            );
            let actual = artifact_bytes(&output, &artifact["slice"]);
            let expected = fs::read(reference.join("input-00").join(original_path))
                .expect("reference artifact");
            if original_path.starts_with("streams/") && original_path.ends_with("/metadata.json") {
                let actual: Value =
                    serde_json::from_slice(&actual).expect("merged stream description");
                let mut expected: Value =
                    serde_json::from_slice(&expected).expect("original stream description");
                expected
                    .as_object_mut()
                    .expect("stream object")
                    .remove("media_file");
                assert_eq!(
                    actual, expected,
                    "original stream description except standalone playable copy"
                );
            } else {
                assert_eq!(
                    actual, expected,
                    "chapter {chapter} artifact {original_path}"
                );
            }
        }
    }
    assert_eq!(
        archived_paths,
        [expected_artifacts.clone(), expected_artifacts]
    );
    for chapter in 0..2 {
        let source = &manifest["inputs"][chapter];
        assert!(
            source.get("directory").is_none(),
            "no per-input output path"
        );
        assert_eq!(
            artifact_bytes(&output, &source["container"]["trailer"]["raw_path"]),
            tail
        );
        for stream in source["media"]["streams"].as_array().expect("streams") {
            let index = stream["index"].as_u64().expect("stream index") as usize;
            let payload = artifact_bytes(&output, &stream["packet_payload"]);
            assert_eq!(payload, expected[&index].concat());
            let packet_index = artifact_json_lines(&output, &stream["packet_index"]);
            assert_eq!(packet_index.len(), expected[&index].len());
            let side_data = artifact_bytes(&output, &stream["side_data_payload"]);
            for (packet, expected_payload) in packet_index.iter().zip(&expected[&index]) {
                let offset = packet["offset"].as_u64().expect("packet offset") as usize;
                let size = packet["size"].as_u64().expect("packet size") as usize;
                assert_eq!(
                    &payload[offset..offset + size],
                    expected_payload,
                    "packet offsets are relative to the referenced artifact slice"
                );
                for side in packet["side_data"].as_array().expect("side data") {
                    let offset = side["offset"].as_u64().expect("side-data offset") as usize;
                    let size = side["size"].as_u64().expect("side-data size") as usize;
                    assert!(offset <= side_data.len() && size <= side_data.len() - offset);
                }
            }
            assert!(
                stream.get("media_file").is_none(),
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
fn sequence_metadata_and_telemetry_share_the_continuous_video_timeline() {
    let temp = tempdir().expect("tempdir");
    let first = temp.path().join("first.insv");
    let second = temp.path().join("second.insv");
    let originals = [
        telemetry_records(1_000_000, 10, 1),
        telemetry_records(9_000_000, 20, 1),
    ];
    let Some(first_tail) = fixture_with_records(&first, &originals[0]) else {
        return;
    };
    let second_tail = fixture_with_records(&second, &originals[1]).expect("second fixture");
    let sequence = two_part_sequence(&first, &second);
    let second_start_ns = sequence.chapters[1].timeline_start.as_nanos() as i128;
    let output = temp.path().join("output");
    let report = extract_sequence(&sequence, &output, &AtomicBool::new(false), |_| {})
        .expect("unified metadata extraction");
    let metadata: Value = serde_json::from_slice(
        &fs::read(output.join("metadata/metadata/recording.json")).expect("recording metadata"),
    )
    .expect("metadata JSON");
    assert_eq!(metadata["schema_version"], 2);
    assert_eq!(
        metadata["timeline"]["duration_ns"],
        sequence.duration.as_nanos().to_string()
    );
    assert_eq!(metadata["timeline"]["complete"], sequence.complete);
    assert_eq!(metadata["common"]["camera_name"], "Unknown Camera");
    assert!(
        metadata["common"].get("blend_angle").is_none(),
        "changing settings must retain their time ranges"
    );
    assert_eq!(metadata["settings"].as_array().expect("settings").len(), 2);
    for (chapter, angle) in [(0, 10), (1, 20)] {
        let settings = &metadata["settings"][chapter];
        assert_eq!(settings["chapter_index"], chapter);
        assert_eq!(settings["input_index"], 0);
        assert_eq!(settings["metadata"]["blend_angle"], angle);
        assert!(settings.get("source_directory").is_none());
        for stream in settings["media"]["streams"]
            .as_array()
            .expect("stream metadata")
        {
            for key in [
                "packet_payload",
                "packet_index",
                "side_data_payload",
                "extradata",
            ] {
                artifact_bytes(&output, &stream[key]);
            }
        }
        assert_artifact_references(&output, &settings["decoded_records"]);
        assert_eq!(
            settings["timeline_start_ns"],
            sequence.chapters[chapter]
                .timeline_start
                .as_nanos()
                .to_string()
        );
        assert_eq!(
            settings["duration_ns"],
            sequence.chapters[chapter].duration.as_nanos().to_string()
        );
    }
    for (filename, kind) in [("motion.jsonl", "motion"), ("exposure.jsonl", "exposure")] {
        assert_eq!(
            metadata["telemetry"][kind]["path"],
            format!("metadata/metadata/{filename}")
        );
        assert_eq!(metadata["telemetry"][kind]["samples"], 6);
        let rows = json_lines(&output.join("metadata/metadata").join(filename));
        assert_eq!(rows.len(), 6);
        for (index, row) in rows.iter().enumerate() {
            let chapter = index / 3;
            let local_ns = [-20_000_000_i128, 50_000_000, 150_000_000][index % 3];
            let global_ns = local_ns + if chapter == 0 { 0 } else { second_start_ns };
            assert_eq!(row["chapter_index"], chapter);
            assert_eq!(row["input_index"], 0);
            assert_eq!(row["sample_index"], index % 3);
            assert_eq!(
                row["timeline_timestamp_ns"],
                global_ns.to_string(),
                "{filename} row {index}"
            );
            let epoch_ns = if chapter == 0 {
                1_000_000_000
            } else {
                9_000_000_000
            };
            assert_eq!(
                row["camera_timestamp_ns"],
                (epoch_ns + local_ns).to_string()
            );
            if kind == "motion" {
                assert_eq!(row["acceleration_g"], serde_json::json!([1.0, 0.0, 0.0]));
                assert!(row["angular_velocity_rad_s"].is_array());
            } else {
                assert_eq!(row["shutter_ns"], "10000000");
            }
        }
    }
    assert_eq!(
        metadata["records"]["index"],
        "metadata/extra-info/records.jsonl"
    );
    assert!(
        metadata["records"].get("payload").is_none(),
        "raw records use archive slices"
    );
    assert_eq!(metadata["records"]["count"], 10);
    let records = json_lines(&output.join("metadata/extra-info/records.jsonl"));
    assert_eq!(records.len(), 10);
    for row in &records {
        let chapter = row["chapter_index"].as_u64().expect("chapter") as usize;
        let index = row["record_index"].as_u64().expect("record") as usize;
        let original = &originals[chapter][index];
        assert_eq!(row["record"]["id"], original.0);
        assert_eq!(row["record"]["format"], original.1);
        assert!(row.get("source_directory").is_none());
        assert_eq!(
            artifact_bytes(&output, &row["record"]["raw_path"]),
            original.2
        );
        if let Some(decoded) = row["record"].pointer("/decoded/path") {
            let decoded = artifact_bytes(&output, decoded);
            serde_json::from_slice::<Value>(&decoded)
                .expect("decoded metadata remains readable JSON");
        }
        assert_artifact_references(&output, row);
    }
    assert_eq!(
        records
            .iter()
            .filter(|row| row["record"]["id"] == 0xee)
            .count(),
        4,
        "repeated opaque records retain their separate payloads and provenance"
    );
    for (chapter, tail) in [(0, first_tail), (1, second_tail)] {
        let artifact = archived_artifact(&output, "extra-info", chapter, "extra-info/tail.bin");
        assert_eq!(artifact_bytes(&output, &artifact), tail);
    }
    assert_merged_layout(&output);
    for filename in [
        "metadata/metadata/recording.json",
        "metadata/metadata/motion.jsonl",
        "metadata/metadata/exposure.jsonl",
        "metadata/extra-info/records.jsonl",
        "metadata/extra-info/payload.bin",
    ] {
        assert!(
            report
                .files
                .contains(&fs::canonicalize(output.join(filename)).expect("published file")),
            "report lists {filename}"
        );
    }
    assert!(report.files.iter().all(|path| path.is_file()));
    let manifest: Value =
        serde_json::from_slice(&fs::read(report.manifest_path).expect("manifest"))
            .expect("manifest JSON");
    assert_eq!(
        metadata["continuous_streams"],
        manifest["continuous_streams"]
    );
    assert!(output.join("camera_A.mp4").is_file());
    assert!(output.join("camera_B.mp4").is_file());
}

#[test]
fn unsupported_metadata_clock_mapping_preserves_consolidated_original_records() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.insv");
    let originals = telemetry_records(1_000_000, 10, 99);
    let Some(tail) = fixture_with_records(&source, &originals) else {
        return;
    };
    let sequence =
        RecordingSequence::single(InputSet::new(vec![source]).expect("input")).expect("sequence");
    let output = temp.path().join("output");
    let report = extract_sequence(&sequence, &output, &AtomicBool::new(false), |_| {})
        .expect("unsupported mapping does not prevent unpacking");
    for filename in ["motion.jsonl", "exposure.jsonl"] {
        let rows = json_lines(&output.join("metadata/metadata").join(filename));
        assert_eq!(rows.len(), 3);
        assert!(rows
            .iter()
            .all(|row| row.get("timeline_timestamp_ns") == Some(&Value::Null)));
        assert!(rows
            .iter()
            .all(|row| row["camera_timestamp_ns"].is_string()));
    }
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("mapping") || warning.contains("timeline")),
        "unsupported alignment must be visible: {:?}",
        report.warnings
    );
    let records = json_lines(&output.join("metadata/extra-info/records.jsonl"));
    assert_eq!(records.len(), originals.len());
    for (row, original) in records.iter().zip(&originals) {
        assert_eq!(
            artifact_bytes(&output, &row["record"]["raw_path"]),
            original.2
        );
    }
    assert_eq!(
        artifact_bytes(
            &output,
            &archived_artifact(&output, "extra-info", 0, "extra-info/tail.bin")
        ),
        tail
    );
    assert_merged_layout(&output);
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
        let artifact = archived_artifact(
            &output,
            "streams",
            1,
            &format!("streams/{index:03}/packets.bin"),
        );
        assert_eq!(artifact_bytes(&output, &artifact), packets.concat());
    }
    assert_eq!(
        artifact_bytes(
            &output,
            &archived_artifact(&output, "extra-info", 1, "extra-info/tail.bin")
        ),
        tail
    );
    assert_merged_layout(&output);
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
