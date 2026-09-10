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

/// Opt-in real-camera qualification of hardware decode against the serial
/// software reader. Compares active native Y/U/V pixels, excluding row padding.
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
