use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use insta360_rs::container::{
    CaptureOffsetVersion, CodecType, FileCategory, FileRotation, GuardDetectedType, ImuRange,
    InsvInspection, InsvMetadata, LensAccessoryType, OffsetState, PanoRecordTrackOrder,
    PanoRecordType, RecordedColorMode, StreamType, UnknownMetadataValue, VideoPtsMapType,
};
use insta360_rs::motion::decode_motion_record;
use insta360_rs::telemetry::{decode_camera_exposure_record, decode_exposure_record};
use insta360_rs::timing::ExposureTimeline;
use insta360_rs::{probe, CameraModel, InputSet, InsvReader};
use tempfile::tempdir;

const MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";

#[test]
fn parses_indexed_x5_container_without_scanning_media_payloads() {
    let fixture = x5_fixture();
    let mut reader = InsvReader::new(Cursor::new(fixture)).expect("reader");
    let inspection = reader.inspect().expect("inspection");

    assert_eq!(inspection.trailer.version, 3);
    assert_eq!(inspection.trailer.record_count, 3);
    assert_eq!(
        inspection
            .records
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        vec![1, 3, 4]
    );
    assert_eq!(
        inspection.metadata.camera_name.as_deref(),
        Some("Insta360 X5")
    );
    assert_eq!(inspection.metadata.serial.as_deref(), Some("TEST-X5-0001"));
    assert_eq!(
        inspection.metadata.firmware.as_deref(),
        Some("v1.2.3_build4")
    );
    assert_eq!(inspection.metadata.gyro_sample_count, 2);
    assert_eq!(inspection.metadata.exposure_sample_count, 2);
    assert_eq!(inspection.metadata.first_frame_timestamp, Some(1_000_000));
    assert_eq!(inspection.metadata.gyro_timestamp_adjust_ms, Some(1.5));
    assert_eq!(inspection.metadata.rolling_shutter_time, Some(12.5));
    assert_eq!(inspection.metadata.has_gyro_timestamp_adjust, Some(true));
    assert_eq!(inspection.metadata.timelapse_interval, Some(2.5));
    assert_eq!(
        inspection.metadata.timelapse_interval_is_milliseconds,
        Some(false)
    );
    assert_eq!(inspection.metadata.timelapse_interval_ms, Some(2_500));
    assert_eq!(
        inspection.metadata.video_pts_map_type,
        Some(VideoPtsMapType::ReadingInExposureFile)
    );
    assert_eq!(inspection.metadata.file_duration_tailor_time_ms, Some(25));
    assert_eq!(inspection.metadata.blend_angle, Some(183));
    assert_eq!(
        inspection.metadata.file_category,
        Some(FileCategory::DoubleFisheyePanorama)
    );
    assert_eq!(
        inspection.metadata.file_rotation,
        Some(FileRotation::Degrees0)
    );
    assert_eq!(
        inspection.metadata.stream_type,
        Some(StreamType::DualStreamTracks)
    );
    assert_eq!(inspection.metadata.codec_type, Some(CodecType::H265));
    assert_eq!(
        inspection.metadata.capture_offset_version,
        Some(CaptureOffsetVersion::V6)
    );
    assert_eq!(
        inspection.metadata.offset_state,
        Some(OffsetState::Automatic)
    );
    assert_eq!(
        inspection.metadata.guard_detected_type,
        Some(GuardDetectedType::Glass)
    );
    assert_eq!(
        inspection.metadata.pano_record_type,
        Some(PanoRecordType::MultiTrack)
    );
    assert_eq!(
        inspection.metadata.pano_record_track_order,
        Some(PanoRecordTrackOrder::Track0IsStream10)
    );
    assert_eq!(inspection.metadata.video_track_count, Some(2));
    assert_eq!(inspection.metadata.reverse_video_track_order, Some(true));
    assert_eq!(inspection.metadata.expected_bitrate, Some(120_000_000));
    assert_eq!(inspection.metadata.is_pre_recorded, Some(true));
    assert_eq!(
        inspection.metadata.lens_accessory_type,
        Some(LensAccessoryType::BlackMistFilter)
    );
    assert_eq!(inspection.metadata.p3_fake_on, Some(true));
    let crop = inspection
        .metadata
        .crop_window
        .as_ref()
        .expect("crop window");
    assert_eq!(
        (
            crop.source_width,
            crop.source_height,
            crop.destination_width,
            crop.destination_height,
            crop.x_offset,
            crop.y_offset,
        ),
        (5_888, 2_944, 5_760, 2_880, -64, 32)
    );
    assert_eq!(crop.unknown_fields.len(), 1);
    assert_eq!(crop.unknown_fields[0].number, 7);
    assert_eq!(
        inspection.metadata.imu_range,
        Some(ImuRange {
            accelerometer_g: 32.0,
            gyroscope_degrees_per_second: 2_000.0,
        })
    );
    assert_eq!(
        inspection
            .metadata
            .offsets
            .iter()
            .map(|offset| (offset.version, offset.original))
            .collect::<Vec<_>>(),
        vec![(1, false), (2, false), (3, false), (6, false)]
    );
    assert_eq!(
        inspection
            .metadata
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        vec!["InvisibleDiveWater", "bare"]
    );
    assert_eq!(inspection.video_tracks.len(), 2);
    assert!(inspection
        .video_tracks
        .iter()
        .all(|track| track.width == 2880 && track.height == 2880 && track.codec == "hvc1"));
    assert_eq!(inspection.duration, Some(Duration::from_millis(10_010)));
    assert!((inspection.fps.expect("fps") - 29.970_029_970_029_97).abs() < 1e-9);

    assert_eq!(inspection.metadata.unknown_fields.len(), 4);
    assert_eq!(inspection.metadata.unknown_fields[0].number, 300);
    assert_eq!(
        inspection.metadata.unknown_fields[0].value,
        UnknownMetadataValue::Varint(42)
    );
    assert_eq!(
        inspection.metadata.unknown_fields[1].value,
        UnknownMetadataValue::Fixed64(0x0102_0304_0506_0708)
    );
    assert_eq!(
        inspection.metadata.unknown_fields[2].value,
        UnknownMetadataValue::LengthDelimited(vec![9, 8, 7])
    );
    assert_eq!(
        inspection.metadata.unknown_fields[3].value,
        UnknownMetadataValue::Fixed32(0xaabb_ccdd)
    );
}

#[test]
fn reads_explicit_ilog_capture_mode_without_conflating_legacy_gamma() {
    // ExtraMetadata 212 (wire 2), ShootingParamInfo 8 (wire 0), ILOG enum 2.
    // Uninterpreted fields 1 and 15 share the same preserved submessage.
    let shooting = [0x08, 0x01, 0x40, 0x02, 0x78, 0x07];
    let mut wire = vec![0xa2, 0x0d, 0x06];
    wire.extend_from_slice(&shooting);
    // Legacy ExtraMetadata.gamma_mode is tag 22, wire 2.
    wire.extend_from_slice(b"\xb2\x01\x03log");
    let metadata = parse_fixture_metadata(wire);

    assert_eq!(metadata.gamma_mode.as_deref(), Some("log"));
    assert_eq!(metadata.recorded_color_mode, Some(RecordedColorMode::ILog));
    assert!(!metadata.recorded_color_mode_invalid);
    assert!(metadata
        .unknown_fields
        .iter()
        .any(|field| field.number == 212
            && field.value == UnknownMetadataValue::LengthDelimited(shooting.to_vec())));

    let legacy_only = parse_fixture_metadata(b"\xb2\x01\x03log".to_vec());
    assert_eq!(legacy_only.gamma_mode.as_deref(), Some("log"));
    assert_eq!(legacy_only.recorded_color_mode, None);
    assert!(!legacy_only.recorded_color_mode_invalid);
    let absent = parse_fixture_metadata(Vec::new());
    assert_eq!(absent.gamma_mode, None);
    assert_eq!(absent.recorded_color_mode, None);
    assert!(!absent.recorded_color_mode_invalid);
}

#[test]
fn retains_signed_first_frame_camera_timestamps() {
    for timestamp in [i64::MIN, -1_000_000, -1, 0, i64::MAX] {
        let mut wire = Vec::new();
        protobuf_varint(&mut wire, 24, timestamp as u64);
        let metadata = parse_fixture_metadata(wire);
        assert_eq!(metadata.first_frame_timestamp, Some(timestamp));
        assert!(metadata.unknown_fields.is_empty());
    }
}

#[test]
fn retains_unknown_capture_color_enums_without_selecting_ilog() {
    for (raw, expected) in [
        (0, RecordedColorMode::Unknown),
        (1, RecordedColorMode::Standard),
        (2, RecordedColorMode::ILog),
        (3, RecordedColorMode::Dolby),
        (77, RecordedColorMode::Other(77)),
        (u32::MAX, RecordedColorMode::Other(u32::MAX)),
    ] {
        let mut shooting = Vec::new();
        protobuf_varint(&mut shooting, 8, u64::from(raw));
        let mut wire = Vec::new();
        protobuf_bytes(&mut wire, 212, &shooting);
        let metadata = parse_fixture_metadata(wire);

        assert_eq!(metadata.recorded_color_mode, Some(expected));
        assert!(!metadata.recorded_color_mode_invalid);
        assert_eq!(expected.raw_value(), raw);
        assert_eq!(metadata.unknown_fields.len(), 1);
        assert_eq!(
            metadata.unknown_fields[0].value,
            UnknownMetadataValue::LengthDelimited(shooting),
        );
    }
}

#[test]
fn conflicting_or_malformed_color_declarations_cannot_enable_ilog() {
    let mut overflowing = Vec::new();
    protobuf_varint(&mut overflowing, 8, u64::from(u32::MAX) + 1);
    // An overflowing ten-byte varint must not truncate back to ILOG=2.
    let overflowing_varint = vec![
        0x40, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02,
    ];
    for shooting in [
        vec![0x40],                      // Truncated enum varint.
        vec![0x42, 0x01, 0x02],          // Color field has wire type 2.
        vec![0x40, 0x02, 0x40, 0x01],    // Conflicting nested duplicates.
        vec![0x40, 0x01, 0x40, 0x02],    // Reverse order must also be ambiguous.
        vec![0x40, 0x02, 0x42, 0x01, 2], // Wrong wire after a valid I-Log value.
        vec![0x40, 0x02, 0x00],          // Malformed unrelated nested field.
        overflowing,
        overflowing_varint,
    ] {
        let mut wire = Vec::new();
        protobuf_bytes(&mut wire, 212, &shooting);
        let metadata = parse_fixture_metadata(wire);
        assert_eq!(metadata.recorded_color_mode, None, "{shooting:?}");
        assert!(metadata.recorded_color_mode_invalid, "{shooting:?}");
        assert_eq!(
            metadata.unknown_fields[0].value,
            UnknownMetadataValue::LengthDelimited(shooting),
        );
    }

    for second in [vec![0x40, 0x01], vec![0x40, 0x4d], vec![0x40]] {
        for reverse in [false, true] {
            let mut first = vec![0x40, 0x02];
            let mut second = second.clone();
            if reverse {
                std::mem::swap(&mut first, &mut second);
            }
            let mut wire = Vec::new();
            protobuf_bytes(&mut wire, 212, &first);
            protobuf_bytes(&mut wire, 212, &second);
            let metadata = parse_fixture_metadata(wire);
            assert_eq!(metadata.recorded_color_mode, None);
            assert!(metadata.recorded_color_mode_invalid);
            assert_eq!(metadata.unknown_fields.len(), 2);
        }
    }

    let mut wrong_outer_wire = vec![0xa2, 0x0d, 0x02, 0x40, 0x02];
    protobuf_varint(&mut wrong_outer_wire, 212, 2);
    let metadata = parse_fixture_metadata(wrong_outer_wire);
    assert_eq!(metadata.recorded_color_mode, None);
    assert!(metadata.recorded_color_mode_invalid);
}

#[test]
fn invalid_capture_color_remains_distinct_from_absence_with_ilog_gamma() {
    for shooting in [
        vec![0x40, 0x02, 0x40, 0x01],
        vec![0x40, 0x01, 0x40, 0x02],
        vec![0x40],
        vec![0x42, 0x01, 0x02],
    ] {
        let mut wire = Vec::new();
        protobuf_bytes(&mut wire, 22, b"I_Log");
        protobuf_bytes(&mut wire, 212, &shooting);
        let metadata = parse_fixture_metadata(wire);

        assert_eq!(metadata.gamma_mode.as_deref(), Some("I_Log"));
        assert_eq!(metadata.recorded_color_mode, None);
        assert!(metadata.recorded_color_mode_invalid);
        assert!(metadata
            .unknown_fields
            .iter()
            .any(|field| field.number == 212
                && field.value == UnknownMetadataValue::LengthDelimited(shooting.clone())));
    }

    for shooting in [vec![], vec![0x08, 0x01]] {
        let mut wire = Vec::new();
        protobuf_bytes(&mut wire, 22, b"I_Log");
        protobuf_bytes(&mut wire, 212, &shooting);
        let metadata = parse_fixture_metadata(wire);

        assert_eq!(metadata.gamma_mode.as_deref(), Some("I_Log"));
        assert_eq!(metadata.recorded_color_mode, None);
        assert!(!metadata.recorded_color_mode_invalid);
    }
}

#[test]
fn identical_color_duplicates_and_unrelated_shooting_fields_merge() {
    let mut wire = Vec::new();
    protobuf_bytes(&mut wire, 212, &[0x40, 0x02, 0x40, 0x02]);
    protobuf_bytes(&mut wire, 212, &[0x40, 0x02]);
    protobuf_bytes(&mut wire, 212, &[0x08, 0x01]);
    protobuf_bytes(&mut wire, 212, &[]);
    let metadata = parse_fixture_metadata(wire);

    assert_eq!(metadata.recorded_color_mode, Some(RecordedColorMode::ILog));
    assert_eq!(metadata.unknown_fields.len(), 4);
    assert!(!metadata.recorded_color_mode_invalid);
}

#[test]
fn gamma_labels_preserve_raw_text_and_reject_ambiguous_declarations() {
    for gamma in ["", "linear", "log", "I_Log", "future-gamma"] {
        let mut wire = Vec::new();
        protobuf_bytes(&mut wire, 22, gamma.as_bytes());
        protobuf_bytes(&mut wire, 22, gamma.as_bytes());
        let metadata = parse_fixture_metadata(wire);
        assert_eq!(metadata.gamma_mode.as_deref(), Some(gamma));
        assert_eq!(metadata.recorded_color_mode, None);
        assert_eq!(metadata.unknown_fields.len(), 2);
    }
    for second in [b"linear".as_slice(), &[0xff]] {
        let mut wire = Vec::new();
        protobuf_bytes(&mut wire, 22, b"log");
        protobuf_bytes(&mut wire, 22, second);
        let metadata = parse_fixture_metadata(wire);
        assert_eq!(metadata.gamma_mode, None);
        assert_eq!(metadata.unknown_fields.len(), 2);
    }
    let mut wire = Vec::new();
    protobuf_bytes(&mut wire, 22, b"log");
    protobuf_varint(&mut wire, 22, 2);
    let metadata = parse_fixture_metadata(wire);
    assert_eq!(metadata.gamma_mode, None);
    assert_eq!(metadata.unknown_fields.len(), 2);
}

#[test]
fn rejects_semantically_invalid_crop_without_losing_its_payload() {
    let mut metadata = metadata_record();
    let mut invalid_crop = Vec::new();
    protobuf_varint(&mut invalid_crop, 1, 0);
    protobuf_varint(&mut invalid_crop, 2, 2_944);
    protobuf_varint(&mut invalid_crop, 3, 5_760);
    protobuf_varint(&mut invalid_crop, 4, 2_880);
    protobuf_bytes(&mut metadata, 27, &invalid_crop);

    let mut reader =
        InsvReader::new(Cursor::new(x5_fixture_with_metadata(metadata))).expect("reader");
    let parsed = reader.metadata().expect("metadata");

    assert_eq!(parsed.crop_window, None);
    assert!(parsed.unknown_fields.iter().any(|field| field.number == 27
        && matches!(
            &field.value,
            UnknownMetadataValue::LengthDelimited(value) if value == &invalid_crop
        )));
}

#[test]
fn rejects_protobuf_field_numbers_beyond_the_schema_limit() {
    let mut metadata = Vec::new();
    protobuf_varint(&mut metadata, 1 << 29, 1);
    let mut reader =
        InsvReader::new(Cursor::new(x5_fixture_with_metadata(metadata))).expect("reader");

    let error = reader
        .metadata()
        .expect_err("invalid field number must fail");
    assert!(error.to_string().contains("invalid field number"));
}

#[test]
fn bounds_unknown_field_retention_by_limiting_field_count() {
    let mut metadata = Vec::new();
    for _ in 0..=65_536 {
        protobuf_varint(&mut metadata, 300, 1);
    }
    let mut reader =
        InsvReader::new(Cursor::new(x5_fixture_with_metadata(metadata))).expect("reader");

    let error = reader
        .metadata()
        .expect_err("excessive field count must fail");
    assert!(error.to_string().contains("field count"));
}

#[test]
fn probe_flattens_public_media_information() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("VID_20260907_120000_00_001.insv");
    std::fs::write(&path, x5_fixture()).expect("fixture");

    let inputs = InputSet::new(vec![path.clone()]).expect("input set");
    let info = probe(&inputs).expect("probe");

    assert_eq!(info.inputs, vec![path]);
    assert_eq!(info.camera, CameraModel::X5);
    assert_eq!(info.offset_versions, vec![1, 2, 3, 6]);
    assert_eq!(info.video_tracks.len(), 2);
    assert_eq!(info.gyro_sample_count, 2);
    assert!(info
        .optical_profiles
        .contains(&"InvisibleDiveWater".to_owned()));
}

#[test]
fn discovers_and_orders_legacy_pairs() {
    let directory = tempdir().expect("tempdir");
    let primary = directory.path().join("VID_20260907_120000_00_001.insv");
    let secondary = directory.path().join("VID_20260907_120000_10_001.insv");
    File::create(&primary).expect("primary");
    File::create(&secondary).expect("secondary");

    let inputs = InputSet::discover(&secondary).expect("discovery");

    assert_eq!(inputs.paths(), &[primary, secondary]);
}

#[test]
fn rejects_mismatched_legacy_pairs() {
    let directory = tempdir().expect("tempdir");
    let primary = directory.path().join("VID_20260907_120000_00_001.insv");
    let secondary = directory.path().join("VID_20260907_120001_10_001.insv");
    File::create(&primary).expect("primary");
    File::create(&secondary).expect("secondary");

    let error = InputSet::new(vec![secondary, primary]).expect_err("mismatch must fail");

    assert!(error.to_string().contains("same `_00_`/`_10_` recording"));
}

#[test]
fn rejects_a_record_index_that_points_outside_the_trailer() {
    let mut fixture = x5_fixture();
    let index_start = fixture.len() - 72 - 6 - 310;
    let gyro_entry = index_start + 3 * 10;
    fixture[gyro_entry + 6..gyro_entry + 10].copy_from_slice(&u32::MAX.to_le_bytes());

    let mut reader = InsvReader::new(Cursor::new(fixture)).expect("reader");
    let error = reader.inspect().expect_err("invalid index must fail");

    assert!(error
        .to_string()
        .contains("record extends into the trailer index"));
}

#[test]
fn rejects_records_inside_the_index_even_when_their_footer_matches() {
    let mut index = vec![0; 20];
    index[0] = 7;
    index[6..10].copy_from_slice(&10_u32.to_le_bytes());
    // This zero-length record's matching footer is inside the second index slot.
    index[10..16].copy_from_slice(&[0, 7, 0, 0, 0, 0]);
    index.extend_from_slice(&[0, 0, 20, 0, 0, 0]);
    let mut reader = InsvReader::new(Cursor::new(fixture_with_record_region(index))).unwrap();
    let error = reader
        .records()
        .expect_err("index bytes are not record payloads");
    assert!(error
        .to_string()
        .contains("record extends into the trailer index"));
}

#[test]
fn rejects_duplicate_and_overlapping_index_ranges() {
    for duplicate in [false, true] {
        let mut fixture = x5_fixture();
        let index_start = fixture.len() - 72 - 6 - 310;
        let entry_start = index_start + 7 * 10;
        if duplicate {
            let metadata_entry = fixture[index_start + 10..index_start + 20].to_vec();
            fixture[entry_start..entry_start + 10].copy_from_slice(&metadata_entry);
        } else {
            let gyro_entry = index_start + 3 * 10;
            let gyro_offset =
                u32::from_le_bytes(fixture[gyro_entry + 6..gyro_entry + 10].try_into().unwrap());
            let trailer_size = u32::from_le_bytes(
                fixture[fixture.len() - 40..fixture.len() - 36]
                    .try_into()
                    .unwrap(),
            ) as usize;
            let payload_start = fixture.len() - trailer_size;
            let offset = gyro_offset + 10;
            fixture[payload_start + offset as usize..payload_start + offset as usize + 6]
                .copy_from_slice(&[0, 7, 0, 0, 0, 0]);
            fixture[entry_start] = 7;
            fixture[entry_start + 6..entry_start + 10].copy_from_slice(&offset.to_le_bytes());
        }
        let error = InsvReader::new(Cursor::new(fixture))
            .unwrap()
            .records()
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("overlapping or duplicate record ranges"));
    }
}

#[test]
fn sequential_records_reject_partial_prefixes() {
    for prefix_length in 0..6 {
        let mut region = vec![0xaa; prefix_length];
        region.extend_from_slice(&[0, 3, 0, 0, 0, 0]);
        let records = InsvReader::new(Cursor::new(fixture_with_record_region(region)))
            .unwrap()
            .records();
        if prefix_length == 0 {
            assert_eq!(records.unwrap().len(), 1);
        } else {
            assert!(records
                .unwrap_err()
                .to_string()
                .contains("partial record prefix"));
        }
    }
}

#[test]
fn v3_reader_accepts_empty_tails_and_requires_both_directory_marker_bytes() {
    for region in [vec![], vec![0, 0, 0, 0, 0, 0]] {
        let mut reader = InsvReader::new(Cursor::new(fixture_with_record_region(region))).unwrap();
        assert!(reader.records().unwrap().is_empty());
        assert_eq!(reader.inspect().unwrap().trailer.record_count, 0);
    }
    let region = vec![0xff, 9, 0, 1, 0, 0, 0];
    let mut reader = InsvReader::new(Cursor::new(fixture_with_record_region(region))).unwrap();
    let records = reader.records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        (records[0].id, records[0].format, records[0].size),
        (0, 9, 1)
    );
}

#[test]
fn typed_v3_reader_rejects_unknown_trailer_versions_as_unsupported() {
    for version in [0_u32, 2, 4, 256] {
        let mut fixture = x5_fixture();
        let version_offset = fixture.len() - 36;
        fixture[version_offset..version_offset + 4].copy_from_slice(&version.to_le_bytes());
        let error = InsvReader::new(Cursor::new(fixture))
            .unwrap()
            .records()
            .unwrap_err();
        assert!(matches!(error, insta360_rs::Error::MissingCapability(_)));
    }
}

fn fixture_with_record_region(mut payload: Vec<u8>) -> Vec<u8> {
    let size = payload.len() as u32 + 72;
    payload.extend_from_slice(&[0; 32]);
    payload.extend_from_slice(&size.to_le_bytes());
    payload.extend_from_slice(&3_u32.to_le_bytes());
    payload.extend_from_slice(MAGIC);
    let mut fixture = bmff_box(*b"ftyp", b"isom\0\0\0\0isom");
    fixture.extend_from_slice(&bmff_box(*b"inst", &payload));
    fixture
}

#[test]
fn rejects_oversized_iso_box_without_allocating_its_payload() {
    let mut data = Vec::new();
    data.extend_from_slice(&u32::MAX.to_be_bytes());
    data.extend_from_slice(b"ftyp");

    let mut reader = InsvReader::new(Cursor::new(data)).expect("reader");
    let error = reader.inspect().expect_err("invalid box must fail");

    assert!(error.to_string().contains("invalid ISO-BMFF box size"));
}

#[test]
fn record_payload_reads_are_indexed_and_size_bounded() {
    let fixture = x5_fixture();
    let mut reader = InsvReader::new(Cursor::new(fixture)).expect("reader");
    let gyro = reader
        .records()
        .expect("records")
        .into_iter()
        .find(|record| record.id == 3)
        .expect("gyro record");

    let error = reader
        .read_record_payload(&gyro, gyro.size - 1)
        .expect_err("limit must be honored");
    assert!(error.to_string().contains("read limit"));
    assert_eq!(
        reader
            .read_record_payload(&gyro, gyro.size)
            .expect("payload")
            .len(),
        40
    );
}

#[test]
fn typed_metadata_does_not_decode_an_unknown_record_encoding_as_protobuf() {
    let mut fixture = x5_fixture();
    let index_start = fixture.len() - 72 - 6 - 310;
    let mut reader = InsvReader::new(Cursor::new(&fixture)).unwrap();
    let metadata = reader
        .records()
        .unwrap()
        .into_iter()
        .find(|record| record.id == 1)
        .unwrap();
    fixture[(metadata.offset + metadata.size) as usize] = 99;
    fixture[index_start + 11] = 99;
    let mut reader = InsvReader::new(Cursor::new(fixture)).unwrap();
    assert_eq!(reader.records().unwrap()[0].format, 99);
    assert!(matches!(
        reader.metadata(),
        Err(insta360_rs::Error::MissingCapability(_))
    ));
}

#[test]
fn deterministic_mutations_reach_nested_boxes_indexes_and_metadata_without_panicking() {
    let baseline = x5_fixture();
    let mut state = 0xa409_3822_299f_31d0_u64;
    for iteration in 0..1024 {
        let mut bytes = baseline.clone();
        for _ in 0..1 + iteration % 5 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let offset = state as usize % bytes.len();
            bytes[offset] ^= (state >> 32) as u8;
        }
        let mut reader = InsvReader::new(Cursor::new(bytes)).unwrap();
        let _ = reader.inspect();
        let _ = reader.metadata();
        let _ = reader.records();
    }
}

#[test]
fn video_pts_expand_variable_durations_and_reorder_b_frames_without_reading_mdat() {
    let movie = timing_movie(
        &[(2, 30), (3, 60)],
        1_000,
        Some((1, &[(1, 0), (1, 60), (1, -30), (1, -60), (1, 0)])),
        None,
        5,
    );
    let payload_start = 8;
    let mut fixture = bmff_box(*b"mdat", &[0; 8_192]);
    let payload_end = fixture.len() as u64;
    fixture.extend_from_slice(&movie);
    let input = RejectMediaReads {
        cursor: Cursor::new(fixture),
        forbidden: payload_start..payload_end,
    };
    let timestamps = InsvReader::new(input)
        .unwrap()
        .video_presentation_timestamps()
        .unwrap();
    assert_eq!(timestamps, [vec![0, 30_000, 60_000, 90_000, 180_000]]);
}

#[test]
fn video_pts_rescale_signed_and_unsigned_composition_offsets() {
    let signed = timing_movie(&[(3, 1_001)], 30_000, Some((1, &[(3, -1_001)])), None, 3);
    assert_eq!(read_timing(signed).unwrap(), [vec![-33_367, 0, 33_367]]);
    let unsigned = timing_movie(
        &[(1, 1)],
        1_000,
        Some((0, &[(1, i64::from(u32::MAX))])),
        None,
        1,
    );
    assert_eq!(read_timing(unsigned).unwrap(), [vec![4_294_967_295_000]]);
}

#[test]
fn video_pts_apply_simple_edit_origins_and_leading_empty_edits() {
    for version in [0, 1] {
        let trimmed = timing_movie(
            &[(8, 1_001)],
            30_000,
            None,
            Some((version, &[(100, 1_001, 1)])),
            8,
        );
        assert!(matches!(
            read_timing(trimmed),
            Err(insta360_rs::Error::MissingCapability(_))
        ));
        let delayed = timing_movie(
            &[(3, 1_001)],
            30_000,
            Some((0, &[(3, 2_002)])),
            Some((version, &[(40, -1, 1), (101, 2_002, 1)])),
            3,
        );
        assert_eq!(
            read_timing(delayed).unwrap(),
            [vec![40_000, 73_367, 106_733]]
        );
        let reordered_origin = timing_movie(
            &[(3, 1_001)],
            30_000,
            Some((0, &[(3, 2_002)])),
            Some((version, &[(101, 2_002, 1)])),
            3,
        );
        assert_eq!(
            read_timing(reordered_origin).unwrap(),
            [vec![0, 33_367, 66_733]]
        );
    }
}

#[test]
fn video_pts_reject_unsupported_edits_and_ambiguous_sample_tables() {
    for edits in [
        vec![(100, 0, 1), (100, 30, 1)],
        vec![(100, 0, 0)],
        vec![(100, 0, 2)],
        vec![(100, -2, 1)],
    ] {
        let fixture = timing_movie(&[(3, 30)], 1_000, None, Some((0, &edits)), 3);
        assert!(matches!(
            read_timing(fixture),
            Err(insta360_rs::Error::MissingCapability(_))
        ));
    }
    for fixture in [
        timing_movie(&[(3, 30)], 1_000, None, None, 4),
        timing_movie(&[(3, 0)], 1_000, None, None, 3),
        timing_movie(&[(0, 30)], 1_000, None, None, 3),
        timing_movie(&[(3, 30)], 0, None, None, 3),
        timing_movie(&[(3, 30)], 1_000, Some((1, &[(2, 0)])), None, 3),
        timing_movie(&[(3, 30)], 1_000, Some((1, &[(1, 30), (2, 0)])), None, 3),
        timing_movie(&[(3, 1)], 3_000_000, None, None, 3),
        timing_movie(&[(5_000_001, 30)], 1_000, None, None, 5_000_001),
    ] {
        assert!(read_timing(fixture).is_err());
    }
    let mut fragmented = timing_movie(&[(3, 30)], 1_000, None, None, 3);
    fragmented.extend_from_slice(&bmff_box(*b"moof", &[]));
    assert!(matches!(
        read_timing(fragmented),
        Err(insta360_rs::Error::MissingCapability(_))
    ));
}

fn read_timing(fixture: Vec<u8>) -> insta360_rs::Result<Vec<Vec<i64>>> {
    InsvReader::new(Cursor::new(fixture))?.video_presentation_timestamps()
}

struct RejectMediaReads {
    cursor: Cursor<Vec<u8>>,
    forbidden: std::ops::Range<u64>,
}

impl Read for RejectMediaReads {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let position = self.cursor.position();
        assert!(
            position >= self.forbidden.end
                || position + buffer.len() as u64 <= self.forbidden.start,
            "compressed media must not be read"
        );
        self.cursor.read(buffer)
    }
}

impl Seek for RejectMediaReads {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(position)
    }
}

type CompositionRuns<'a> = (u8, &'a [(u32, i64)]);
type EditEntries<'a> = (u8, &'a [(u64, i64, i16)]);

fn timing_movie(
    durations: &[(u32, u32)],
    timescale: u32,
    composition: Option<CompositionRuns<'_>>,
    edits: Option<EditEntries<'_>>,
    sample_count: u32,
) -> Vec<u8> {
    let header = |scale: u32| {
        let mut payload = vec![0; 12];
        payload.extend_from_slice(&scale.to_be_bytes());
        payload.extend_from_slice(&[0; 8]);
        payload
    };
    let mut stts = vec![0; 4];
    stts.extend_from_slice(&(durations.len() as u32).to_be_bytes());
    for (count, delta) in durations {
        stts.extend_from_slice(&count.to_be_bytes());
        stts.extend_from_slice(&delta.to_be_bytes());
    }
    let mut stsz = vec![0; 4];
    stsz.extend_from_slice(&1_u32.to_be_bytes());
    stsz.extend_from_slice(&sample_count.to_be_bytes());
    let mut stbl = bmff_box(*b"stts", &stts);
    stbl.extend_from_slice(&bmff_box(*b"stsz", &stsz));
    if let Some((version, runs)) = composition {
        let mut ctts = vec![version, 0, 0, 0];
        ctts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
        for (count, offset) in runs {
            ctts.extend_from_slice(&count.to_be_bytes());
            ctts.extend_from_slice(&(*offset as u32).to_be_bytes());
        }
        stbl.extend_from_slice(&bmff_box(*b"ctts", &ctts));
    }
    let mut mdia = bmff_box(*b"mdhd", &header(timescale));
    mdia.extend_from_slice(&bmff_box(*b"hdlr", b"\0\0\0\0\0\0\0\0vide"));
    mdia.extend_from_slice(&bmff_box(*b"minf", &bmff_box(*b"stbl", &stbl)));
    let mut trak = bmff_box(*b"mdia", &mdia);
    if let Some((version, entries)) = edits {
        let mut elst = vec![version, 0, 0, 0];
        elst.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (duration, start, rate) in entries {
            if version == 1 {
                elst.extend_from_slice(&duration.to_be_bytes());
                elst.extend_from_slice(&start.to_be_bytes());
            } else {
                elst.extend_from_slice(&(*duration as u32).to_be_bytes());
                elst.extend_from_slice(&(*start as i32).to_be_bytes());
            }
            elst.extend_from_slice(&rate.to_be_bytes());
            elst.extend_from_slice(&[0; 2]);
        }
        trak.extend_from_slice(&bmff_box(*b"edts", &bmff_box(*b"elst", &elst)));
    }
    let mut moov = bmff_box(*b"mvhd", &header(1_000));
    moov.extend_from_slice(&bmff_box(*b"trak", &trak));
    bmff_box(*b"moov", &moov)
}

#[test]
fn probes_external_x5_fixture_when_configured() {
    let Ok(path) = std::env::var("INSTA360_RS_X5_SAMPLE") else {
        return;
    };
    let inputs = InputSet::discover(PathBuf::from(path)).expect("external input");
    let info = probe(&inputs).expect("external probe");

    assert_eq!(info.camera, CameraModel::X5);
    assert_eq!(info.video_tracks.len(), 2);
    assert!(info.offset_versions.contains(&3));
    assert!(info.offset_versions.contains(&6));
    assert!(info.gyro_sample_count > 1_000_000);
    assert!(info
        .optical_profiles
        .contains(&"InvisibleDiveWater".to_owned()));

    let file = File::open(inputs.paths()[0].clone()).expect("external file");
    let mut reader = InsvReader::new(file).expect("external reader");
    let metadata = reader.metadata().expect("external metadata");
    assert_eq!(metadata.is_raw_gyro, Some(true));
    assert_eq!(
        metadata.video_pts_map_type,
        Some(VideoPtsMapType::ReadingInExposureFile)
    );
    assert_eq!(metadata.first_frame_timestamp, Some(35_434_464));
    assert_eq!(
        metadata.imu_range,
        Some(ImuRange {
            accelerometer_g: 32.0,
            gyroscope_degrees_per_second: 2_000.0,
        })
    );
    let gyro_record = reader
        .records()
        .expect("external records")
        .into_iter()
        .find(|record| record.id == 3)
        .expect("external gyro record");
    let gyro_payload = reader
        .read_record_payload(&gyro_record, 64 * 1024 * 1024)
        .expect("external gyro payload");
    let error = decode_motion_record(&gyro_payload, &metadata)
        .expect_err("the sample requires exposure-file PTS alignment");
    assert!(matches!(error, insta360_rs::Error::MissingCapability(_)));
    let exposure_record = reader
        .records()
        .expect("external records")
        .into_iter()
        .find(|record| record.id == 4)
        .expect("external exposure record");
    let exposure_payload = reader
        .read_record_payload(&exposure_record, 2 * 1024 * 1024)
        .expect("external exposure payload");
    let exposures =
        decode_exposure_record(&exposure_payload, &metadata).expect("external exposures");
    assert!(exposures.len() > 30_000);
    assert!(exposures[0].timestamp < Duration::from_millis(40));
    let camera_exposures = decode_camera_exposure_record(&exposure_payload, &metadata)
        .expect("external camera exposure clock");
    assert_eq!(camera_exposures.len(), 39_905);
    assert_eq!(camera_exposures[23].timestamp_micros, 35_434_464);
    let tracks = reader
        .video_presentation_timestamps()
        .expect("actual video PTS");
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0], tracks[1]);
    assert_eq!(tracks[0].len(), 39_878);
    assert_eq!(&tracks[0][..3], &[0, 33_367, 66_733]);
    assert_eq!(tracks[0][39_877], 1_330_562_567);
    let timeline = ExposureTimeline::new(&camera_exposures, 35_434_464, &tracks[0]).unwrap();
    assert_eq!(timeline.timestamp_micros_at_pts(0).unwrap(), 35_434_464.0);
    assert_eq!(
        timeline.timestamp_micros_at_pts(33_367).unwrap(),
        35_467_830.0
    );
    assert_eq!(
        timeline.timestamp_micros_at_pts(1_330_562_567).unwrap(),
        1_365_977_092.0
    );
}

fn x5_fixture() -> Vec<u8> {
    x5_fixture_with_metadata(metadata_record())
}

fn parse_fixture_metadata(metadata: Vec<u8>) -> InsvMetadata {
    InsvReader::new(Cursor::new(x5_fixture_with_metadata(metadata)))
        .expect("reader")
        .metadata()
        .expect("metadata")
}

fn x5_fixture_with_metadata(metadata: Vec<u8>) -> Vec<u8> {
    let ftyp = bmff_box(*b"ftyp", b"isom\0\0\0\0isom");
    let track_a = video_track_box(300, 1_001);
    let track_b = video_track_box(300, 1_001);
    let mut moov_payload = track_a;
    moov_payload.extend_from_slice(&track_b);
    let moov = bmff_box(*b"moov", &moov_payload);

    let gyro = vec![0_u8; 40];
    let exposure = vec![0_u8; 32];
    let records = [(1_u8, 1_u8, metadata), (3, 0, gyro), (4, 0, exposure)];

    let mut payload = Vec::new();
    let mut index = vec![0_u8; 31 * 10];
    for (id, format, data) in records {
        let relative_offset = u32::try_from(payload.len()).expect("small fixture");
        let size = u32::try_from(data.len()).expect("small fixture");
        payload.extend_from_slice(&data);
        payload.push(format);
        payload.push(id);
        payload.extend_from_slice(&size.to_le_bytes());

        let entry = usize::from(id) * 10;
        index[entry] = id;
        index[entry + 1] = format;
        index[entry + 2..entry + 6].copy_from_slice(&size.to_le_bytes());
        index[entry + 6..entry + 10].copy_from_slice(&relative_offset.to_le_bytes());
    }
    let index_size = u32::try_from(index.len()).expect("small fixture");
    payload.extend_from_slice(&index);
    payload.extend_from_slice(&[0, 0]);
    payload.extend_from_slice(&index_size.to_le_bytes());
    payload.extend_from_slice(&[0; 32]);

    let extra_size = u32::try_from(payload.len() + 8 + MAGIC.len()).expect("small fixture");
    payload.extend_from_slice(&extra_size.to_le_bytes());
    payload.extend_from_slice(&3_u32.to_le_bytes());
    payload.extend_from_slice(MAGIC);

    let inst = bmff_box(*b"inst", &payload);
    let mut fixture = ftyp;
    fixture.extend_from_slice(&moov);
    fixture.extend_from_slice(&inst);
    fixture
}

fn video_track_box(sample_count: u32, sample_delta: u32) -> Vec<u8> {
    let mut mdhd = vec![0; 12];
    mdhd.extend_from_slice(&30_000_u32.to_be_bytes());
    mdhd.extend_from_slice(
        &sample_count
            .checked_mul(sample_delta)
            .expect("duration")
            .to_be_bytes(),
    );
    mdhd.extend_from_slice(&[0; 4]);

    let mut hdlr = vec![0; 8];
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0; 12]);

    let mut sample_entry = vec![0; 36];
    sample_entry[0..4].copy_from_slice(&36_u32.to_be_bytes());
    sample_entry[4..8].copy_from_slice(b"hvc1");
    sample_entry[32..34].copy_from_slice(&2_880_u16.to_be_bytes());
    sample_entry[34..36].copy_from_slice(&2_880_u16.to_be_bytes());
    let mut stsd = vec![0; 4];
    stsd.extend_from_slice(&1_u32.to_be_bytes());
    stsd.extend_from_slice(&sample_entry);

    let mut stts = vec![0; 4];
    stts.extend_from_slice(&1_u32.to_be_bytes());
    stts.extend_from_slice(&sample_count.to_be_bytes());
    stts.extend_from_slice(&sample_delta.to_be_bytes());

    let mut stbl = bmff_box(*b"stsd", &stsd);
    stbl.extend_from_slice(&bmff_box(*b"stts", &stts));
    let minf = bmff_box(*b"minf", &bmff_box(*b"stbl", &stbl));

    let mut mdia = bmff_box(*b"mdhd", &mdhd);
    mdia.extend_from_slice(&bmff_box(*b"hdlr", &hdlr));
    mdia.extend_from_slice(&minf);
    bmff_box(*b"trak", &bmff_box(*b"mdia", &mdia))
}

fn metadata_record() -> Vec<u8> {
    let mut data = Vec::new();
    protobuf_bytes(&mut data, 1, b"TEST-X5-0001");
    protobuf_bytes(&mut data, 2, b"Insta360 X5");
    protobuf_bytes(&mut data, 3, b"v1.2.3_build4");
    protobuf_bytes(&mut data, 5, b"2_1.0_2.0_3.0");
    protobuf_varint(&mut data, 20, 30);
    protobuf_varint(&mut data, 24, 1_000_000);
    protobuf_fixed64(&mut data, 25, 12.5_f64.to_bits());
    let mut crop = Vec::new();
    protobuf_varint(&mut crop, 1, 5_888);
    protobuf_varint(&mut crop, 2, 2_944);
    protobuf_varint(&mut crop, 3, 5_760);
    protobuf_varint(&mut crop, 4, 2_880);
    protobuf_varint(
        &mut crop,
        5,
        u64::from(u32::from_ne_bytes((-64_i32).to_ne_bytes())),
    );
    protobuf_varint(&mut crop, 6, 32);
    protobuf_bytes(&mut crop, 7, b"future-crop-data");
    protobuf_bytes(&mut data, 27, &crop);
    protobuf_fixed64(&mut data, 28, 1.5_f64.to_bits());
    protobuf_varint(&mut data, 29, 1);
    protobuf_fixed32(&mut data, 30, 2.5_f32.to_bits());
    protobuf_bytes(&mut data, 53, b"2_2.0_3.0_4.0");
    protobuf_bytes(&mut data, 54, b"2_3.0_4.0_5.0");
    protobuf_varint(&mut data, 59, 0);
    protobuf_varint(&mut data, 62, 1);
    protobuf_varint(&mut data, 64, 2);
    let mut imu_range = Vec::new();
    protobuf_varint(&mut imu_range, 1, 32);
    protobuf_varint(&mut imu_range, 2, 2_000);
    protobuf_bytes(&mut data, 65, &imu_range);
    protobuf_varint(&mut data, 68, 6);
    protobuf_varint(&mut data, 79, 2);
    protobuf_varint(&mut data, 80, 1);
    protobuf_varint(&mut data, 104, 2);
    protobuf_bytes(&mut data, 111, b"2_6.0_7.0_8.0");
    protobuf_varint(&mut data, 127, 25);
    protobuf_varint(&mut data, 128, 183);
    protobuf_varint(&mut data, 129, 2);
    protobuf_varint(&mut data, 130, 1);
    protobuf_varint(&mut data, 131, 3);
    protobuf_varint(&mut data, 132, 3);
    protobuf_varint(&mut data, 133, 2_500);
    protobuf_varint(&mut data, 134, 1);
    protobuf_varint(&mut data, 135, 120_000_000);
    protobuf_varint(&mut data, 136, 4);

    let mut bare = Vec::new();
    protobuf_bytes(&mut bare, 1, b"bare");
    let mut water = Vec::new();
    protobuf_bytes(&mut water, 1, b"InvisibleDiveWater");
    let mut profile_group = Vec::new();
    protobuf_bytes(&mut profile_group, 1, &bare);
    protobuf_bytes(&mut profile_group, 1, &water);
    protobuf_bytes(&mut data, 145, &profile_group);
    protobuf_varint(&mut data, 186, 2);
    protobuf_varint(&mut data, 193, 1);
    protobuf_varint(&mut data, 300, 42);
    protobuf_fixed64(&mut data, 301, 0x0102_0304_0506_0708);
    protobuf_bytes(&mut data, 302, &[9, 8, 7]);
    protobuf_fixed32(&mut data, 303, 0xaabb_ccdd);
    data
}

fn protobuf_bytes(output: &mut Vec<u8>, field: u64, value: &[u8]) {
    encode_varint(output, field << 3 | 2);
    encode_varint(output, value.len() as u64);
    output.extend_from_slice(value);
}

fn protobuf_varint(output: &mut Vec<u8>, field: u64, value: u64) {
    encode_varint(output, field << 3);
    encode_varint(output, value);
}

fn protobuf_fixed64(output: &mut Vec<u8>, field: u64, value: u64) {
    encode_varint(output, (field << 3) | 1);
    output.extend_from_slice(&value.to_le_bytes());
}

fn protobuf_fixed32(output: &mut Vec<u8>, field: u64, value: u32) {
    encode_varint(output, (field << 3) | 5);
    output.extend_from_slice(&value.to_le_bytes());
}

fn encode_varint(output: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn bmff_box(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).expect("small fixture");
    let mut output = Vec::with_capacity(size as usize);
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(payload);
    output
}

#[allow(dead_code)]
fn assert_inspection_is_send_sync(_: &InsvInspection) {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InsvInspection>();
}
