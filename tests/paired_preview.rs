#![cfg(feature = "media")]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use insta360_rs::paired::{PairedPreviewReader, PreviewAcceleration};
use insta360_rs::{InputSet, PairedReader, RecordingSequence};

fn varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        output.push(value as u8 | 128);
        value >>= 7;
    }
    output.push(value as u8);
}

fn original(dir: &Path, name: &str, b_frames: bool, missing_b: bool) -> PathBuf {
    original_with_offset(dir, name, b_frames, missing_b, "0")
}

fn original_with_offset(
    dir: &Path,
    name: &str,
    b_frames: bool,
    missing_b: bool,
    offset: &str,
) -> PathBuf {
    let path = dir.join(format!("{name}.insv"));
    let mut command = std::process::Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=30:duration=1",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=30:duration=1,hue=h=90",
    ]);
    if missing_b {
        command.args([
            "-filter_complex",
            "[1:v]select='not(eq(n,5))'[b]",
            "-map",
            "0:v",
            "-map",
            "[b]",
            "-fps_mode",
            "passthrough",
        ]);
    } else {
        command.args(["-map", "0:v", "-map", "1:v"]);
    }
    let result = command
        .args([
            "-c:v",
            "mpeg4",
            "-g",
            "6",
            "-bf",
            if b_frames { "2" } else { "0" },
            "-threads",
            "1",
            "-output_ts_offset",
            offset,
            "-f",
            "mp4",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut metadata = vec![18, 11];
    metadata.extend(b"Insta360 X5");
    for (tag, value) in [(80, 2), (131, 3)] {
        varint(&mut metadata, tag * 8);
        varint(&mut metadata, value);
    }
    let mut payload = metadata.clone();
    payload.extend([1, 1]);
    payload.extend((metadata.len() as u32).to_le_bytes());
    let mut index = vec![0u8; 20];
    index[10] = 1;
    index[11] = 1;
    index[12..16].copy_from_slice(&(metadata.len() as u32).to_le_bytes());
    payload.extend(&index);
    payload.extend([0, 0]);
    payload.extend((index.len() as u32).to_le_bytes());
    payload.extend([0u8; 32]);
    payload.extend(((payload.len() + 40) as u32).to_le_bytes());
    payload.extend(3u32.to_le_bytes());
    payload.extend(b"8db42d694ccc418790edff439fe026bf");
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&((payload.len() + 8) as u32).to_be_bytes())
        .unwrap();
    file.write_all(b"inst").unwrap();
    file.write_all(&payload).unwrap();
    path
}

fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn random_access_preserves_every_native_pair_and_pixel_with_and_without_b_frames() {
    for b_frames in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = original(dir.path(), "reference", b_frames, false);
        let sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
        let mut reference = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let mut expected = Vec::new();
        while let Some(pair) = reference.next_pair(&cancel()).unwrap() {
            expected.push(pair);
        }
        assert_eq!(expected.len(), 30);
        for policy in [PreviewAcceleration::Software, PreviewAcceleration::Auto] {
            let mut preview = PairedPreviewReader::new(&sequence, policy).unwrap();
            // Adjacent, backward, repeated, GOP-edge and EOF-drain positions.
            for index in [0, 1, 2, 17, 16, 7, 6, 29, 28, 0, 0, 5, 4, 3, 12, 11] {
                let target = Duration::from_micros(
                    expected[index].timestamp_micros.saturating_sub(1).max(0) as u64,
                );
                let actual = preview.frame_at(target, cancel()).unwrap();
                assert_eq!(
                    actual.identity(),
                    expected[index].identity(),
                    "index={index}, B={b_frames}, policy={policy:?}"
                );
                for (actual, expected) in [
                    (&actual.a, &expected[index].a),
                    (&actual.b, &expected[index].b),
                ] {
                    assert_eq!(actual.format(), expected.format());
                    for plane in 0..actual.planes() {
                        assert_eq!(actual.data(plane), expected.data(plane));
                    }
                }
            }
        }
    }
}

#[test]
fn chapter_switches_and_reverse_track_order_keep_exact_camera_identity() {
    let dir = tempfile::tempdir().unwrap();
    let first = original(dir.path(), "first", true, false);
    let second = original(dir.path(), "second", false, false);
    let mut sequence = RecordingSequence::single(InputSet::new(vec![first]).unwrap()).unwrap();
    let mut next = RecordingSequence::single(InputSet::new(vec![second]).unwrap())
        .unwrap()
        .chapters
        .remove(0);
    next.timeline_start = sequence.duration;
    next.inspection.metadata.reverse_video_track_order = Some(true);
    sequence.duration += next.duration;
    sequence.chapters.push(next);
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
    for micros in [1_000_000, 0, 1_400_000, 400_000] {
        let target = Duration::from_micros(micros);
        let expected = PairedReader::open(&sequence, target)
            .unwrap()
            .next_pair(&cancel())
            .unwrap()
            .unwrap();
        let actual = preview.frame_at(target, cancel()).unwrap();
        assert_eq!(actual.identity(), expected.identity());
        assert_eq!(actual.a.data(0), expected.a.data(0));
        assert_eq!(actual.b.data(0), expected.b.data(0));
    }
}

#[test]
fn fractional_nonzero_origin_keeps_the_first_native_pair() {
    let dir = tempfile::tempdir().unwrap();
    let path = original_with_offset(dir.path(), "offset", false, false, "0.067");
    let sequence = RecordingSequence::single(InputSet::new(vec![path.clone()]).unwrap()).unwrap();
    let input = ffmpeg_next::format::input(&path).unwrap();
    let stream = input.stream(0).unwrap();
    let expected_pts = stream.start_time();
    // The generated origin is between microseconds and rounds upwards.
    assert_ne!(
        (expected_pts * 1_000_000) % i64::from(stream.time_base().denominator()),
        0
    );
    let pair = PairedReader::open(&sequence, Duration::ZERO)
        .unwrap()
        .next_pair(&cancel())
        .unwrap()
        .unwrap();
    assert_eq!(
        pair.a_pts, expected_pts,
        "serial reader must retain the first source frame"
    );
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
    let pair = preview.frame_at(Duration::ZERO, cancel()).unwrap();
    assert_eq!(
        pair.a_pts, expected_pts,
        "preview must retain the first source frame"
    );
    assert_eq!(pair.timestamp_micros, 0);
    let time_base = stream.time_base();
    drop(input);
    let mut source = ffmpeg_next::format::input(&path).unwrap();
    let mut original_pts = source
        .packets()
        .filter(|(stream, _)| stream.index() == 0)
        .map(|(_, packet)| packet.pts().unwrap())
        .collect::<Vec<_>>();
    original_pts.sort_unstable();
    assert_eq!(original_pts.len(), 30);
    let mut serial = PairedReader::open(&sequence, Duration::ZERO).unwrap();
    for pts in &original_pts {
        assert_eq!(serial.next_pair(&cancel()).unwrap().unwrap().a_pts, *pts);
    }
    assert!(serial.next_pair(&cancel()).unwrap().is_none());
    // Deterministic repeated, forward and backward seeks use independently
    // demuxed source PTS, with no serial-reader-derived timing expectations.
    for iteration in 0..512 {
        let index = (iteration * 17) % original_pts.len();
        let relative_ns = (original_pts[index] - expected_pts) as u128
            * time_base.numerator() as u128
            * 1_000_000_000
            / time_base.denominator() as u128;
        let target = Duration::from_nanos(relative_ns as u64);
        assert_eq!(
            preview.frame_at(target, cancel()).unwrap().a_pts,
            original_pts[index]
        );
        if iteration % 31 == 0 {
            assert!(matches!(
                preview.frame_at(target, Arc::new(AtomicBool::new(true))),
                Err(insta360_rs::Error::Cancelled)
            ));
        }
    }
    // One nanosecond after the exact fractional first-frame interval must
    // select the following frame even though both requests truncate to 33333us.
    let after_first_interval = Duration::from_nanos(33_333_334);
    assert_eq!(
        preview
            .frame_at(after_first_interval, cancel())
            .unwrap()
            .a_pts,
        original_pts[2]
    );
    assert_eq!(
        PairedReader::open(&sequence, after_first_interval)
            .unwrap()
            .next_pair(&cancel())
            .unwrap()
            .unwrap()
            .a_pts,
        original_pts[2]
    );
}

#[test]
fn preview_advances_to_the_next_chapter_after_the_last_presented_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = original(dir.path(), "boundary", true, false);
    let mut sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
    let mut second = sequence.chapters[0].clone();
    second.timeline_start = sequence.duration;
    sequence.duration += second.duration;
    sequence.chapters.push(second);
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
    let pair = preview
        .frame_at(Duration::from_millis(990), cancel())
        .unwrap();
    assert_eq!(pair.chapter_index, 1);
    assert_eq!(pair.timestamp_micros, 1_000_000);
    for iteration in 0..64 {
        let time = if iteration % 2 == 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(990)
        };
        let pair = preview.frame_at(time, cancel()).unwrap();
        assert_eq!(pair.chapter_index, iteration % 2);
    }
    assert!(preview
        .frame_at(Duration::from_millis(1990), cancel())
        .is_err());
    assert_eq!(
        preview
            .frame_at(Duration::ZERO, cancel())
            .unwrap()
            .timestamp_micros,
        0
    );
}

#[test]
fn exact_pair_reopening_rejects_invalid_native_identities() {
    let dir = tempfile::tempdir().unwrap();
    let path = original(dir.path(), "identity", false, false);
    let sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
    let identity = PairedReader::open(&sequence, Duration::ZERO)
        .unwrap()
        .next_pair(&cancel())
        .unwrap()
        .unwrap()
        .identity();
    let mut invalid = identity;
    invalid.chapter_index = sequence.chapters.len();
    assert!(PairedReader::open_at_pair(&sequence, invalid).is_err());
    for time_base in [(0, 1), (1, 0), (-1, 1), (1, -1)] {
        invalid = identity;
        invalid.a_time_base = time_base.into();
        assert!(PairedReader::open_at_pair(&sequence, invalid).is_err());
    }
    invalid = identity;
    invalid.b_pts += 1;
    assert!(PairedReader::open_at_pair(&sequence, invalid).is_err());
    for pts in [identity.a_pts + 1, 1_000_000] {
        invalid = identity;
        invalid.a_pts = pts;
        invalid.b_pts = pts;
        assert!(PairedReader::open_at_pair(&sequence, invalid)
            .unwrap()
            .next_pair(&cancel())
            .is_err());
    }
}

#[test]
fn missing_partner_is_rejected_and_both_worker_replies_are_drained() {
    let dir = tempfile::tempdir().unwrap();
    let path = original(dir.path(), "missing", true, true);
    let sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
    let error = preview
        .frame_at(Duration::from_micros(166_666), cancel())
        .err()
        .expect("missing fifth partner must fail");
    assert!(error.to_string().contains("not simultaneous"), "{error}");
    let pair = preview
        .frame_at(Duration::from_millis(200), cancel())
        .unwrap();
    assert_eq!(pair.timestamp_micros, 200_000);
    assert!(matches!(
        preview.frame_at(Duration::ZERO, Arc::new(AtomicBool::new(true))),
        Err(insta360_rs::Error::Cancelled)
    ));
    assert_eq!(
        preview
            .frame_at(Duration::ZERO, cancel())
            .unwrap()
            .timestamp_micros,
        0
    );
    assert!(preview.frame_at(sequence.duration, cancel()).is_err());
}

/// Opt-in real-camera parity of Auto preview against the serial software reader.
/// Auto may fall back to software; this alone cannot establish hardware coverage.
/// Compares active native Y/U/V pixels, excluding row padding.
#[test]
fn configured_real_x5_preview_matches_software_native_pixels() {
    let Ok(path) = std::env::var("INSTA360_RS_PREVIEW_SAMPLE") else {
        return;
    };
    let sequence = RecordingSequence::single(InputSet::new(vec![path.into()]).unwrap()).unwrap();
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Auto).unwrap();
    for seconds in [0.0, 120.0, 120.05, 1199.0, 1198.0] {
        let time = Duration::from_secs_f64(seconds);
        let expected = PairedReader::open(&sequence, time)
            .unwrap()
            .next_pair(&cancel())
            .unwrap()
            .unwrap();
        let actual = preview.frame_at(time, cancel()).unwrap();
        assert_eq!(actual.identity(), expected.identity());
        for (actual, expected) in [(&actual.a, &expected.a), (&actual.b, &expected.b)] {
            use ffmpeg_next::format::Pixel;
            assert!(matches!(
                expected.format(),
                Pixel::YUV420P | Pixel::YUVJ420P
            ));
            assert!(matches!(
                actual.format(),
                Pixel::YUV420P | Pixel::YUVJ420P | Pixel::NV12
            ));
            assert_eq!(actual.color_space(), expected.color_space());
            assert_eq!(actual.color_range(), expected.color_range());
            assert_eq!(
                (actual.width(), actual.height()),
                (expected.width(), expected.height())
            );
            for plane in 0..3 {
                let width = expected.plane_width(plane) as usize;
                let height = expected.plane_height(plane) as usize;
                for row in 0..height {
                    let expected = &expected.data(plane)[row * expected.stride(plane)..][..width];
                    if actual.format() == Pixel::NV12 && plane > 0 {
                        let interleaved = &actual.data(1)[row * actual.stride(1)..][..width * 2];
                        let component = interleaved
                            .chunks_exact(2)
                            .map(|uv| uv[plane - 1])
                            .collect::<Vec<_>>();
                        assert_eq!(
                            component, expected,
                            "native chroma at {seconds}s, plane{plane}, row{row}"
                        );
                    } else {
                        assert_eq!(
                            &actual.data(plane)[row * actual.stride(plane)..][..width],
                            expected,
                            "native pixels at {seconds}s, plane{plane}, row{row}"
                        );
                    }
                }
            }
        }
    }
}
