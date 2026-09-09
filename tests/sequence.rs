use std::fs;
use std::time::Duration;

use insta360_rs::container::FileSplitType;
use insta360_rs::{InputSet, InsvReader, RecordingSequence};

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push((value as u8) | 128);
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
fn bmff(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(payload);
    out
}
fn metadata(index: u32, total: u32, split: u32, identity: &str) -> Vec<u8> {
    let mut data = Vec::new();
    bytes(&mut data, 1, b"SERIAL-1");
    bytes(&mut data, 2, b"Insta360 X5");
    let mut group = Vec::new();
    integer(&mut group, 1, 20);
    integer(&mut group, 2, index.into());
    bytes(&mut group, 3, identity.as_bytes());
    integer(&mut group, 4, total.into());
    bytes(&mut data, 26, &group);
    integer(&mut data, 88, split.into());
    integer(&mut data, 80, 2);
    integer(&mut data, 131, 3);
    data
}
fn trailer(data: &[u8]) -> Vec<u8> {
    let mut payload = data.to_vec();
    payload.extend([1, 1]);
    payload.extend((data.len() as u32).to_le_bytes());
    let mut index = vec![0u8; 20];
    index[10] = 1;
    index[11] = 1;
    index[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
    payload.extend(&index);
    payload.extend([0, 0]);
    payload.extend((index.len() as u32).to_le_bytes());
    payload.extend([0u8; 32]);
    payload.extend(((payload.len() + 40) as u32).to_le_bytes());
    payload.extend(3u32.to_le_bytes());
    payload.extend(b"8db42d694ccc418790edff439fe026bf");
    bmff(b"inst", &payload)
}
fn track() -> Vec<u8> {
    let mut mdhd = vec![0u8; 12];
    mdhd.extend(30u32.to_be_bytes());
    mdhd.extend(30u32.to_be_bytes());
    mdhd.extend([0u8; 4]);
    let mut hdlr = vec![0u8; 8];
    hdlr.extend(b"vide");
    hdlr.extend([0u8; 12]);
    let mut sample = vec![0u8; 36];
    sample[0..4].copy_from_slice(&36u32.to_be_bytes());
    sample[4..8].copy_from_slice(b"mp4v");
    sample[32..34].copy_from_slice(&64u16.to_be_bytes());
    sample[34..36].copy_from_slice(&64u16.to_be_bytes());
    let mut stsd = vec![0u8; 4];
    stsd.extend(1u32.to_be_bytes());
    stsd.extend(sample);
    let mut stts = vec![0u8; 4];
    stts.extend(1u32.to_be_bytes());
    stts.extend(30u32.to_be_bytes());
    stts.extend(1u32.to_be_bytes());
    let mut stbl = bmff(b"stsd", &stsd);
    stbl.extend(bmff(b"stts", &stts));
    let mut mdia = bmff(b"mdhd", &mdhd);
    mdia.extend(bmff(b"hdlr", &hdlr));
    mdia.extend(bmff(b"minf", &bmff(b"stbl", &stbl)));
    bmff(b"trak", &bmff(b"mdia", &mdia))
}
fn fixture(data: &[u8]) -> Vec<u8> {
    let mut out = bmff(b"ftyp", b"isom\0\0\0\0isom");
    let mut tracks = track();
    tracks.extend(track());
    out.extend(bmff(b"moov", &tracks));
    out.extend(trailer(data));
    out
}
fn write(
    dir: &std::path::Path,
    index: u32,
    total: u32,
    split: u32,
    identity: &str,
) -> std::path::PathBuf {
    let path = dir.join(format!("VID_20260101_120000_00_{index:03}.insv"));
    fs::write(&path, fixture(&metadata(index, total, split, identity))).unwrap();
    path
}
#[test]
fn decodes_split_group_and_retains_original_fields() {
    let data = fixture(&metadata(1, 3, 2, "camera-group"));
    let inspection = InsvReader::new(std::io::Cursor::new(data))
        .unwrap()
        .inspect()
        .unwrap();
    assert_eq!(
        inspection.metadata.file_split_type,
        Some(FileSplitType::Split)
    );
    let group = inspection.metadata.recording_group.unwrap();
    assert_eq!((group.index, group.total, group.capture_type), (1, 3, 20));
    assert_eq!(group.identity, "camera-group");
    assert!(inspection
        .metadata
        .unknown_fields
        .iter()
        .any(|field| field.number == 26));
    assert!(inspection
        .metadata
        .unknown_fields
        .iter()
        .any(|field| field.number == 88));
}
#[test]
fn conflicting_or_malformed_group_is_never_used_for_discovery() {
    let mut data = metadata(0, 2, 2, "first");
    let mut other = Vec::new();
    bytes(&mut other, 3, b"second");
    bytes(&mut data, 26, &other);
    let inspection = InsvReader::new(std::io::Cursor::new(fixture(&data)))
        .unwrap()
        .inspect()
        .unwrap();
    assert!(inspection.metadata.sequence_metadata_invalid);
    assert!(inspection.metadata.recording_group.is_none());
    let mut data = metadata(0, 2, 2, "first");
    integer(&mut data, 88, u64::MAX);
    assert!(
        InsvReader::new(std::io::Cursor::new(fixture(&data)))
            .unwrap()
            .inspect()
            .unwrap()
            .metadata
            .sequence_metadata_invalid
    );
}
#[test]
fn orders_camera_chapters_and_resolves_half_open_global_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let b = write(dir.path(), 1, 2, 2, "same");
    let a = write(dir.path(), 0, 2, 2, "same");
    let sequence = RecordingSequence::new(vec![
        InputSet::discover(b).unwrap(),
        InputSet::discover(a).unwrap(),
    ])
    .unwrap();
    assert!(sequence.complete);
    assert_eq!(sequence.duration, Duration::from_secs(2));
    assert_eq!(sequence.chapters[0].group_index, Some(0));
    assert_eq!(sequence.chapter_at(Duration::from_millis(999)), Some(0));
    assert_eq!(sequence.chapter_at(Duration::from_secs(1)), Some(1));
    assert_eq!(sequence.chapter_at(Duration::from_secs(2)), None);
}
#[test]
fn discovery_does_not_group_nonsplit_identity_and_reports_missing_parts() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), 0, 3, 1, "same");
    write(dir.path(), 1, 3, 1, "same");
    assert_eq!(RecordingSequence::discover(&a).unwrap().chapters.len(), 1);
    write(dir.path(), 0, 3, 2, "same");
    write(dir.path(), 2, 3, 2, "same");
    let sequence = RecordingSequence::discover(&a).unwrap();
    assert_eq!(sequence.chapters.len(), 2);
    assert!(!sequence.complete);
    assert!(sequence.require_complete().is_err());
    assert!(
        RecordingSequence::single(InputSet::discover(a).unwrap())
            .unwrap()
            .complete
    );
}
#[test]
fn unknown_total_and_duplicate_or_unrelated_members_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), 0, 0, 2, "same");
    let b = write(dir.path(), 1, 0, 2, "same");
    assert!(!RecordingSequence::discover(&a).unwrap().complete);
    assert!(RecordingSequence::new(vec![
        InputSet::discover(&a).unwrap(),
        InputSet::discover(&a).unwrap()
    ])
    .is_err());
    fs::write(&b, fixture(&metadata(1, 2, 2, "different"))).unwrap();
    assert!(RecordingSequence::new(vec![
        InputSet::discover(a).unwrap(),
        InputSet::discover(b).unwrap()
    ])
    .is_err());
}

#[cfg(feature = "media")]
mod decoded {
    use super::*;
    use insta360_rs::PairedReader;
    use std::sync::atomic::AtomicBool;

    fn video(dir: &std::path::Path, index: u32, total: u32) -> std::path::PathBuf {
        let path = dir.join(format!("VID_20260101_130000_00_{index:03}.insv"));
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=30:duration=0.4",
                "-map",
                "0:v",
                "-map",
                "0:v",
                "-c:v",
                "mpeg4",
                "-bf",
                "2",
                "-threads",
                "1",
                "-f",
                "mp4",
            ])
            .arg(&path)
            .output()
            .expect("fixture ffmpeg");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&trailer(&metadata(index, total, 2, "decoded-group")))
            .unwrap();
        path
    }
    #[test]
    fn decodes_matching_pairs_and_drains_delayed_frames_across_chapters() {
        let dir = tempfile::tempdir().unwrap();
        let a = video(dir.path(), 0, 2);
        video(dir.path(), 1, 2);
        let sequence = RecordingSequence::discover(a).unwrap();
        let cancel = AtomicBool::new(false);
        let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let mut times = Vec::new();
        let mut chapters = Vec::new();
        while let Some(pair) = reader.next_pair(&cancel).unwrap() {
            assert_eq!(pair.a.data(0), pair.b.data(0));
            times.push(pair.timestamp_micros);
            chapters.push(pair.chapter_index);
        }
        assert_eq!(times.len(), 24);
        assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(&chapters[..12], &[0; 12]);
        assert_eq!(&chapters[12..], &[1; 12]);
        assert_eq!(times[12], 400000);
    }
    #[test]
    fn seeks_recording_time_and_honors_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let a = video(dir.path(), 0, 2);
        video(dir.path(), 1, 2);
        let sequence = RecordingSequence::discover(a).unwrap();
        let mut reader = PairedReader::open(&sequence, Duration::from_millis(500)).unwrap();
        let pair = reader.next_pair(&AtomicBool::new(false)).unwrap().unwrap();
        assert_eq!(pair.chapter_index, 1);
        assert_eq!(pair.timestamp_micros, 500000);
        assert!(matches!(
            reader.next_pair(&AtomicBool::new(true)),
            Err(insta360_rs::Error::Cancelled)
        ));
    }

    #[test]
    fn missing_camera_frame_never_shifts_later_pair_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.insv");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=30:duration=0.4",
                "-filter_complex",
                "[0:v]split[a][b];[b]select='not(eq(n,5))'[filtered]",
                "-map",
                "[a]",
                "-map",
                "[filtered]",
                "-fps_mode",
                "passthrough",
                "-c:v",
                "mpeg4",
                "-bf",
                "2",
                "-threads",
                "1",
                "-f",
                "mp4",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&trailer(&metadata(0, 1, 1, "single")))
            .unwrap();
        let sequence = RecordingSequence::single(InputSet::discover(path).unwrap()).unwrap();
        let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let cancel = AtomicBool::new(false);
        for _ in 0..5 {
            assert!(reader.next_pair(&cancel).unwrap().is_some());
        }
        let error = match reader.next_pair(&cancel) {
            Err(error) => error,
            _ => panic!("missing partner must fail"),
        };
        assert!(error.to_string().contains("not simultaneous"));
    }

    #[test]
    fn native_pair_identity_survives_a_rounded_up_display_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = video(dir.path(), 0, 1);
        let sequence = RecordingSequence::discover(path).unwrap();
        let cancel = AtomicBool::new(false);
        let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        reader.next_pair(&cancel).unwrap();
        reader.next_pair(&cancel).unwrap();
        let displayed = reader.next_pair(&cancel).unwrap().unwrap();
        assert_eq!(displayed.timestamp_micros, 66667);
        let mut reopened = PairedReader::open_at_pair(&sequence, displayed.identity()).unwrap();
        let saved = reopened.next_pair(&cancel).unwrap().unwrap();
        assert_eq!(displayed.identity(), saved.identity());
        assert_eq!(displayed.a.data(0), saved.a.data(0));
    }

    #[test]
    fn seeking_b_frames_keeps_the_previous_gop_for_both_lenses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gop.insv");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=10:duration=1",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=10:duration=1,hue=h=90",
                "-map",
                "0:v",
                "-map",
                "1:v",
                "-c:v",
                "mpeg4",
                "-g",
                "3",
                "-bf",
                "1",
                "-threads",
                "1",
                "-f",
                "mp4",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&trailer(&metadata(0, 1, 1, "gop")))
            .unwrap();
        let sequence = RecordingSequence::single(InputSet::discover(path).unwrap()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut original = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let mut identities = Vec::new();
        while let Some(pair) = original.next_pair(&cancel).unwrap() {
            identities.push(pair.identity());
        }
        assert_eq!(identities.len(), 10);
        for start in 1..10 {
            let mut reader =
                PairedReader::open(&sequence, Duration::from_millis(start * 100)).unwrap();
            for expected in &identities[start as usize..] {
                assert_eq!(
                    reader.next_pair(&cancel).unwrap().unwrap().identity(),
                    *expected
                );
            }
            assert!(reader.next_pair(&cancel).unwrap().is_none());
            let expected = identities[start as usize];
            let mut exact = PairedReader::open_at_pair(&sequence, expected).unwrap();
            assert_eq!(
                exact.next_pair(&cancel).unwrap().unwrap().identity(),
                expected
            );
        }
    }

    #[test]
    fn forwards_original_audio_and_drains_a_clipped_tail_without_video_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.insv");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=30:duration=0.4",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=500:sample_rate=44100:duration=0.4",
                "-map",
                "0:v",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:v",
                "mpeg4",
                "-bf",
                "2",
                "-c:a",
                "aac",
                "-threads",
                "1",
                "-f",
                "mp4",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&trailer(&metadata(0, 1, 1, "single")))
            .unwrap();
        let sequence = RecordingSequence::single(InputSet::discover(&path).unwrap()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut reader = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        reader.enable_audio();
        let mut forwarded = Vec::new();
        while let Some(pair) = reader.next_pair(&cancel).unwrap() {
            forwarded.extend(reader.take_audio_packets());
            if pair.timestamp_micros >= 200000 {
                break;
            }
        }
        reader
            .finish_audio(Duration::from_millis(200), &cancel)
            .unwrap();
        forwarded.extend(reader.take_audio_packets());
        assert!(!forwarded.is_empty());
        assert!(forwarded.iter().all(|(chapter, packet)| *chapter == 0
            && packet.stream() == 2
            && packet.pts().is_some()));
        let mut input = ffmpeg_next::format::input(&path).unwrap();
        let original = input
            .packets()
            .filter(|(stream, _)| stream.index() == 2)
            .map(|(_, packet)| (packet.pts(), packet.data().unwrap().to_vec()))
            .collect::<Vec<_>>();
        for ((_, packet), (pts, payload)) in forwarded.iter().zip(&original) {
            assert_eq!(packet.pts(), *pts);
            assert_eq!(packet.data().unwrap(), payload);
        }
        assert!(forwarded.last().unwrap().1.pts().unwrap() >= 8820);
    }
}
