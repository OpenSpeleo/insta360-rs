#![cfg(feature = "media")]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use insta360_rs::paired::{PairedPreviewReader, PreviewAcceleration, PreviewSelection};
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
    original_with_pattern(dir, name, b_frames, missing_b, offset, false)
}

fn original_with_pattern(
    dir: &Path,
    name: &str,
    b_frames: bool,
    missing_b: bool,
    offset: &str,
    gaps: bool,
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
    if gaps {
        command.args([
            "-filter_complex",
            "[0:v]select='not(between(n,5,15))'[a];[1:v]select='not(between(n,5,15))'[b]",
            "-map",
            "[a]",
            "-map",
            "[b]",
            "-fps_mode",
            "passthrough",
        ]);
    } else if missing_b {
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
fn continuous_preview_matches_every_original_pair_and_drains_chapters() {
    let dir = tempfile::tempdir().unwrap();
    for b_frames in [false, true] {
        let path = original(
            dir.path(),
            &format!("continuous-{b_frames}"),
            b_frames,
            false,
        );
        let mut sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
        let mut second = sequence.chapters[0].clone();
        second.timeline_start = sequence.duration;
        sequence.duration += second.duration;
        sequence.chapters.push(second);
        let mut expected = PairedReader::open(&sequence, Duration::ZERO).unwrap();
        let mut actual =
            PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
        let mut count = 0;
        while let Some(pair) = expected.next_pair(&cancel()).unwrap() {
            // Repeated lookahead requests must still advance by exactly one pair.
            actual.prefetch_next(cancel()).unwrap();
            actual.prefetch_next(cancel()).unwrap();
            let preview = actual
                .next_pair(cancel())
                .unwrap()
                .expect("matching original pair");
            assert_eq!(preview.identity(), pair.identity());
            assert_eq!(preview.timestamp_micros, pair.timestamp_micros);
            for (left, right) in [(&preview.a, &pair.a), (&preview.b, &pair.b)] {
                for plane in 0..left.planes() {
                    for row in 0..left.plane_height(plane) as usize {
                        let width = left.plane_width(plane) as usize;
                        assert_eq!(
                            &left.data(plane)
                                [row * left.stride(plane)..row * left.stride(plane) + width],
                            &right.data(plane)
                                [row * right.stride(plane)..row * right.stride(plane) + width]
                        );
                    }
                }
            }
            count += 1;
        }
        assert_eq!(count, 60);
        actual.prefetch_next(cancel()).unwrap();
        assert!(actual.next_pair(cancel()).unwrap().is_none());
        actual.prefetch_next(cancel()).unwrap();
        assert!(actual.next_pair(cancel()).unwrap().is_none());
        let sought = actual
            .frame_at(Duration::from_millis(400), cancel())
            .unwrap();
        let next = actual.next_pair(cancel()).unwrap().unwrap();
        assert!(next.a_pts > sought.a_pts);
        let mut reference = PairedReader::open_at_pair(&sequence, sought.identity()).unwrap();
        reference.next_pair(&cancel()).unwrap().unwrap();
        assert_eq!(
            next.identity(),
            reference.next_pair(&cancel()).unwrap().unwrap().identity()
        );
    }
}

#[test]
fn prefetched_pairs_do_not_change_retained_originals_or_escape_seek_and_cancellation() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let path = original(dir.path(), "lookahead", true, false);
    let sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
    let retained = preview.frame_at(Duration::ZERO, cancel()).unwrap();
    let identity = retained.identity();
    let original = [retained.a.data(0).to_vec(), retained.b.data(0).to_vec()];
    preview.prefetch_next(cancel()).unwrap();
    let sought = preview
        .frame_at(Duration::from_millis(400), cancel())
        .unwrap();
    let mut reference = PairedReader::open(&sequence, Duration::from_millis(400)).unwrap();
    assert_eq!(
        sought.identity(),
        reference.next_pair(&cancel()).unwrap().unwrap().identity()
    );
    preview.prefetch_next(cancel()).unwrap();
    assert_eq!(
        preview.next_pair(cancel()).unwrap().unwrap().identity(),
        reference.next_pair(&cancel()).unwrap().unwrap().identity()
    );

    let cancelled = cancel();
    preview.prefetch_next(cancelled.clone()).unwrap();
    cancelled.store(true, Ordering::Relaxed);
    assert!(matches!(
        preview.next_pair(cancelled),
        Err(insta360_rs::Error::Cancelled)
    ));
    assert_eq!(
        preview
            .frame_at(Duration::ZERO, cancel())
            .unwrap()
            .identity(),
        identity
    );
    let cancelled = cancel();
    preview.prefetch_next(cancelled.clone()).unwrap();
    cancelled.store(true, Ordering::Relaxed);
    // A fresh seek drains cancelled replies without consuming them as its result.
    assert_eq!(
        preview
            .frame_at(Duration::ZERO, cancel())
            .unwrap()
            .identity(),
        identity
    );

    for time in [
        sequence.duration,
        sequence.duration + Duration::from_secs(1),
    ] {
        preview.prefetch_next(cancel()).unwrap();
        assert!(preview.seek_pair(time, cancel()).unwrap().is_none());
        assert!(preview.next_pair(cancel()).unwrap().is_none());
        assert_eq!(
            preview
                .frame_at(Duration::ZERO, cancel())
                .unwrap()
                .identity(),
            identity
        );
    }
    assert_eq!(retained.identity(), identity);
    assert_eq!(
        [retained.a.data(0).to_vec(), retained.b.data(0).to_vec()],
        original,
        "lookahead must not overwrite pixels retained for Save"
    );
    preview.prefetch_next(cancel()).unwrap();
    let (done, completion) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(preview);
        done.send(()).unwrap();
    });
    completion
        .recv_timeout(Duration::from_secs(3))
        .expect("dropping one in-flight pair joins workers");
}

#[test]
fn prefetched_mismatched_lenses_are_drained_before_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = original(dir.path(), "lookahead-missing", true, true);
    let sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
    let mut preview = PairedPreviewReader::new(&sequence, PreviewAcceleration::Software).unwrap();
    preview
        .frame_at(Duration::from_millis(100), cancel())
        .unwrap();
    preview.prefetch_next(cancel()).unwrap();
    assert_eq!(
        preview
            .next_pair(cancel())
            .unwrap()
            .unwrap()
            .timestamp_micros,
        133_333
    );
    preview.prefetch_next(cancel()).unwrap();
    let error = preview
        .next_pair(cancel())
        .err()
        .expect("missing partner fails");
    assert!(error.to_string().contains("not simultaneous"), "{error}");
    assert_eq!(
        preview
            .frame_at(Duration::from_millis(200), cancel())
            .unwrap()
            .timestamp_micros,
        200_000
    );
    preview.prefetch_next(cancel()).unwrap();
    assert_eq!(
        preview
            .next_pair(cancel())
            .unwrap()
            .unwrap()
            .timestamp_micros,
        233_333
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
    assert!(preview
        .seek_pair(sequence.duration, cancel())
        .unwrap()
        .is_none());
    assert!(preview.next_pair(cancel()).unwrap().is_none());
    assert!(preview
        .seek_pair(sequence.duration + Duration::from_secs(1), cancel())
        .unwrap()
        .is_none());
    assert_eq!(
        preview
            .frame_at(Duration::ZERO, cancel())
            .unwrap()
            .timestamp_micros,
        0
    );
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

fn repeated_next_selection(
    reader: &mut PairedPreviewReader,
    selection: PreviewSelection,
) -> Option<insta360_rs::FramePair> {
    let mut pair = reader.next_pair(cancel()).unwrap()?;
    for _ in 1..selection.advance.get() {
        let Some(next) = reader.next_pair(cancel()).unwrap() else {
            break;
        };
        pair = next;
    }
    while selection
        .not_before
        .is_some_and(|time| Duration::from_micros(pair.timestamp_micros.max(0) as u64) < time)
    {
        let Some(next) = reader.next_pair(cancel()).unwrap() else {
            break;
        };
        pair = next;
    }
    Some(pair)
}

fn assert_same_selected_pair(actual: &insta360_rs::FramePair, expected: &insta360_rs::FramePair) {
    assert_eq!(actual.identity(), expected.identity());
    assert_eq!(actual.timestamp_micros, expected.timestamp_micros);
    for (actual, expected) in [(&actual.a, &expected.a), (&actual.b, &expected.b)] {
        assert_eq!(actual.format(), expected.format());
        assert_eq!(actual.color_space(), expected.color_space());
        assert_eq!(actual.color_range(), expected.color_range());
        assert_eq!(actual.planes(), expected.planes());
        for plane in 0..actual.planes() {
            // FFmpeg's format descriptor accounts for planar/interleaved and
            // high-depth software formats without counting uninitialized padding.
            let width = unsafe {
                ffmpeg_next::ffi::av_image_get_linesize(
                    actual.format().into(),
                    actual.width() as i32,
                    plane as i32,
                )
            };
            assert!(width > 0, "public preview pixels must be CPU-readable");
            let width = width as usize;
            for row in 0..actual.plane_height(plane) as usize {
                assert_eq!(
                    &actual.data(plane)[row * actual.stride(plane)..][..width],
                    &expected.data(plane)[row * expected.stride(plane)..][..width],
                );
            }
        }
    }
}

#[test]
fn selected_native_pairs_match_repeated_decoding_across_gaps_chapters_and_eof() {
    for b_frames in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let first =
            original_with_pattern(dir.path(), "selection-gap", b_frames, false, "0.067", true);
        let second = original(dir.path(), "selection-next", b_frames, false);
        let mut sequence = RecordingSequence::single(InputSet::new(vec![first]).unwrap()).unwrap();
        let mut chapter = RecordingSequence::single(InputSet::new(vec![second]).unwrap())
            .unwrap()
            .chapters
            .remove(0);
        chapter.timeline_start = sequence.duration;
        chapter.inspection.metadata.reverse_video_track_order = Some(true);
        sequence.duration += chapter.duration;
        sequence.chapters.push(chapter);
        for policy in [PreviewAcceleration::Software, PreviewAcceleration::Auto] {
            let mut actual = PairedPreviewReader::new(&sequence, policy).unwrap();
            let mut reference = PairedPreviewReader::new(&sequence, policy).unwrap();
            for (advance, nanoseconds) in [
                (1, None),
                (3, Some(300_000_001)),
                (2, Some(1_100_000_001)),
                (1, None),
                (100, Some(10_000_000_000)),
                (1, None),
            ] {
                let selection = PreviewSelection {
                    advance: std::num::NonZeroU32::new(advance).unwrap(),
                    not_before: nanoseconds.map(Duration::from_nanos),
                };
                actual.prefetch_next(cancel()).unwrap();
                actual.prefetch_next(cancel()).unwrap();
                let expected = repeated_next_selection(&mut reference, selection);
                let selected = actual.next_selected_pair(selection, cancel()).unwrap();
                assert_eq!(selected.is_some(), expected.is_some());
                if let (Some(selected), Some(expected)) = (selected, expected) {
                    assert_same_selected_pair(&selected, &expected);
                }
            }
            let stats = actual.decode_stats();
            assert_eq!(
                stats.validated_pairs,
                reference.decode_stats().materialized_pairs
            );
            assert_eq!(stats.materialized_pairs, 5);
            assert!(stats.validated_pairs > stats.materialized_pairs);
            assert!(
                stats.hardware_transfer_attempts
                    <= 2 * (stats.materialized_pairs + stats.speculative_pairs)
            );
            // Cancellation and a recovery seek must discard any old selected/prefetched state.
            actual.seek_pair(Duration::ZERO, cancel()).unwrap();
            let cancelled = cancel();
            actual.prefetch_next(cancelled.clone()).unwrap();
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(matches!(
                actual.next_selected_pair(PreviewSelection::default(), cancelled),
                Err(insta360_rs::Error::Cancelled)
            ));
            let first = actual.seek_pair(Duration::ZERO, cancel()).unwrap().unwrap();
            assert_same_selected_pair(
                &first,
                &reference
                    .seek_pair(Duration::ZERO, cancel())
                    .unwrap()
                    .unwrap(),
            );
        }
    }
}

#[test]
fn selection_validates_missing_partners_even_when_their_frames_would_be_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let path = original(dir.path(), "selection-missing", true, true);
    let sequence = RecordingSequence::single(InputSet::new(vec![path]).unwrap()).unwrap();
    for policy in [PreviewAcceleration::Software, PreviewAcceleration::Auto] {
        let mut reader = PairedPreviewReader::new(&sequence, policy).unwrap();
        let error = reader
            .next_selected_pair(
                PreviewSelection {
                    advance: std::num::NonZeroU32::new(12).unwrap(),
                    not_before: Some(Duration::from_millis(500)),
                },
                cancel(),
            )
            .err()
            .expect("discarded unmatched pair must still fail");
        assert!(error.to_string().contains("not simultaneous"), "{error}");
        assert_eq!(reader.decode_stats().materialized_pairs, 0);
        assert_eq!(reader.decode_stats().hardware_transfer_attempts, 0);
        assert_eq!(
            reader
                .seek_pair(Duration::from_millis(200), cancel())
                .unwrap()
                .unwrap()
                .timestamp_micros,
            200_000
        );
    }
}

/// Optional real-source admission comparison. Hardware coverage is reported by
/// hardware_transfer_attempts; Auto is allowed to fall back to software.
#[test]
fn configured_real_x5_selected_preview_matches_repeated_native_decoding() {
    let Ok(path) = std::env::var("INSTA360_RS_PREVIEW_SAMPLE") else {
        return;
    };
    let sequence = RecordingSequence::single(InputSet::new(vec![path.into()]).unwrap()).unwrap();
    let mut actual = PairedPreviewReader::new(&sequence, PreviewAcceleration::Auto).unwrap();
    let mut reference = PairedPreviewReader::new(&sequence, PreviewAcceleration::Auto).unwrap();
    let start = std::env::var("INSTA360_RS_PREVIEW_SAMPLE_START")
        .map(|value| Duration::from_secs_f64(value.parse().expect("sample start must be seconds")))
        .unwrap_or(Duration::ZERO);
    let selected = actual
        .seek_pair(start, cancel())
        .unwrap()
        .expect("sample must include its configured start");
    let expected = reference
        .seek_pair(start, cancel())
        .unwrap()
        .expect("reference sample must include its configured start");
    assert_same_selected_pair(&selected, &expected);
    for advance in [1, 4, 2, 8] {
        let selection = PreviewSelection {
            advance: std::num::NonZeroU32::new(advance).unwrap(),
            not_before: None,
        };
        actual.prefetch_next(cancel()).unwrap();
        let expected = repeated_next_selection(&mut reference, selection);
        let selected = actual.next_selected_pair(selection, cancel()).unwrap();
        assert_eq!(selected.is_some(), expected.is_some());
        if let (Some(selected), Some(expected)) = (selected, expected) {
            assert_same_selected_pair(&selected, &expected);
        }
    }
    assert_eq!(
        actual.decode_stats().validated_pairs,
        reference.decode_stats().materialized_pairs
    );
    assert!(
        actual.decode_stats().hardware_transfer_attempts
            <= 2 * (actual.decode_stats().materialized_pairs
                + actual.decode_stats().speculative_pairs)
    );
    println!(
        "Native selection at {start:?}: selected={:?}, repeated={:?}",
        actual.decode_stats(),
        reference.decode_stats()
    );
}
