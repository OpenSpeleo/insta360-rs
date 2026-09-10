//! Verified camera chapters, kept separate from simultaneous lens-file pairs.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::container::{FileSplitType, InsvInspection, RecordingGroup};
use crate::error::io_error;
use crate::{Error, InputSet, InsvReader, Result};

const MAX_CHAPTERS: usize = 4096;

/// One temporal chapter and its simultaneous lens inputs.
#[derive(Clone, Debug)]
pub struct RecordingChapter {
    pub inputs: InputSet,
    pub inspection: InsvInspection,
    pub timeline_start: Duration,
    pub duration: Duration,
    pub group_index: Option<u32>,
}

/// Camera-associated original chapters; group submedia metadata does not prove coverage.
#[derive(Clone, Debug)]
pub struct RecordingSequence {
    pub chapters: Vec<RecordingChapter>,
    pub duration: Duration,
    /// Coverage of the selected scope is verified. Split-group submedia metadata
    /// cannot establish this; nonsplit and explicit single selections can.
    pub complete: bool,
    pub warnings: Vec<String>,
}

impl RecordingSequence {
    /// Opens precisely one chapter, even when other recording parts are missing.
    pub fn single(inputs: InputSet) -> Result<Self> {
        let chapter = inspect_chapter(inputs)?;
        Ok(Self {
            duration: chapter.duration,
            chapters: vec![chapter],
            complete: true,
            warnings: Vec::new(),
        })
    }

    /// Validates and orders explicitly provided camera chapters by raw group index.
    /// Arbitrary concatenation is intentionally not supported.
    pub fn new(inputs: Vec<InputSet>) -> Result<Self> {
        if inputs.is_empty() || inputs.len() > MAX_CHAPTERS {
            return Err(invalid("a recording requires between 1 and 4096 chapters"));
        }
        let chapters = inputs
            .into_iter()
            .map(inspect_chapter)
            .collect::<Result<Vec<_>>>()?;
        for chapter in &chapters {
            validate_sequence_metadata(&chapter.inspection)?;
        }
        if chapters.len() == 1
            && chapters[0].inspection.metadata.file_split_type != Some(FileSplitType::Split)
        {
            let duration = chapters[0].duration;
            return Ok(Self {
                chapters,
                duration,
                complete: true,
                warnings: Vec::new(),
            });
        }
        Self::from_chapters(chapters)
    }

    /// Finds temporal siblings only when the selected file explicitly declares splitting.
    /// Invalid/unrelated neighbors never change the selected file's association.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let selected = inspect_chapter(InputSet::discover(path.as_ref())?)?;
        validate_sequence_metadata(&selected.inspection)?;
        if selected.inspection.metadata.file_split_type != Some(FileSplitType::Split) {
            return Self::single(selected.inputs);
        }
        let identity = chapter_group(&selected)?.clone();
        let parent = path
            .as_ref()
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let entries = fs::read_dir(parent).map_err(|error| io_error(parent, error))?;
        let mut chapters = vec![selected];
        let mut seen = BTreeSet::new();
        seen.insert(canonical_paths(&chapters[0].inputs)?);
        let mut inspected = 0;
        for entry in entries {
            let entry = entry.map_err(|error| io_error(parent, error))?;
            let candidate = entry.path();
            if !candidate
                .extension()
                .is_some_and(|value| value.eq_ignore_ascii_case("insv"))
                || !candidate.is_file()
            {
                continue;
            }
            inspected += 1;
            if inspected > MAX_CHAPTERS * 2 {
                return Err(invalid(
                    "directory contains too many INSV candidates; select chapters explicitly",
                ));
            }
            let Ok(inputs) = InputSet::discover(&candidate) else {
                continue;
            };
            if !seen.insert(canonical_paths(&inputs)?) {
                continue;
            }
            let Ok(chapter) = inspect_chapter(inputs) else {
                continue;
            };
            if chapter.inspection.metadata.file_split_type == Some(FileSplitType::Split)
                && chapter
                    .inspection
                    .metadata
                    .recording_group
                    .as_ref()
                    .is_some_and(|group| {
                        group.identity == identity.identity
                            && group.capture_type == identity.capture_type
                    })
            {
                chapters.push(chapter);
            }
        }
        Self::from_chapters(chapters)
    }

    /// Returns original paths in chapter order, primary lens first.
    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.chapters
            .iter()
            .flat_map(|chapter| chapter.inputs.paths())
    }

    /// Reports a substantial uncovered interval on the camera's capture clock.
    ///
    /// This is positive evidence of discontinuous footage, not a completeness
    /// test. Raw-gyro timestamps use microseconds. A one-second allowance avoids
    /// treating encoder priming, frame rounding and small boundary overlaps as
    /// absent footage. Unknown clocks and time-lapse captures remain unverified;
    /// filename counters and submedia index gaps are never consulted.
    pub fn has_capture_gap(&self) -> bool {
        self.chapters.windows(2).any(|chapters| {
            let first = &chapters[0];
            let next = &chapters[1];
            let (Some(start), Some(next_start)) = (
                capture_start(&first.inspection),
                capture_start(&next.inspection),
            ) else {
                return false;
            };
            start
                .checked_add(first.duration)
                .and_then(|end| end.checked_add(Duration::from_secs(1)))
                .is_some_and(|end| next_start > end)
        })
    }

    /// Whether an associated preview represents footage absent from these originals.
    ///
    /// Preview files are optional. When one exists, its camera, identity, split
    /// declaration and capture clock must agree before it can provide evidence.
    /// Comparing actual capture intervals avoids assumptions about preview index
    /// stride, original filenames, or how many previews a camera produces.
    pub fn lacks_preview_footage(&self, preview: &InsvInspection) -> bool {
        let Some(first) = self.chapters.first() else {
            return false;
        };
        let (Some(group), Some(preview_group)) = (
            first.inspection.metadata.recording_group.as_ref(),
            preview.metadata.recording_group.as_ref(),
        ) else {
            return false;
        };
        if preview.metadata.file_split_type != Some(FileSplitType::Split)
            || preview.metadata.sequence_metadata_invalid
            || first.inspection.metadata.file_split_type != Some(FileSplitType::Split)
            || group.identity.is_empty()
            || group.identity != preview_group.identity
            || group.capture_type != preview_group.capture_type
            || first.inspection.metadata.serial.is_none()
            || first.inspection.metadata.serial != preview.metadata.serial
            || first.inspection.metadata.camera_name != preview.metadata.camera_name
        {
            return false;
        }
        let Some(preview_start) = capture_start(preview) else {
            return false;
        };
        let Some(preview_end) = preview
            .duration
            .filter(|duration| !duration.is_zero())
            .and_then(|duration| preview_start.checked_add(duration))
        else {
            return false;
        };
        // An unknown original clock cannot establish an uncovered interval.
        let starts: Option<Vec<_>> = self
            .chapters
            .iter()
            .map(|chapter| capture_start(&chapter.inspection))
            .collect();
        let Some(starts) = starts else {
            return false;
        };
        let allowance = Duration::from_secs(1);
        let mut covered_until = preview_start;
        for (chapter, start) in self.chapters.iter().zip(starts) {
            let Some(end) = start.checked_add(chapter.duration) else {
                return false;
            };
            if end < covered_until.saturating_sub(allowance) {
                continue;
            }
            if start > covered_until.saturating_add(allowance) {
                return true;
            }
            covered_until = covered_until.max(end);
            if preview_end <= covered_until.saturating_add(allowance) {
                return false;
            }
        }
        true
    }

    /// Resolves a half-open recording time to its chapter index.
    pub fn chapter_at(&self, time: Duration) -> Option<usize> {
        self.chapters.iter().position(|chapter| {
            time >= chapter.timeline_start && time < chapter.timeline_start + chapter.duration
        })
    }

    /// Rejects unresolved coverage before promising the entire recording.
    ///
    /// Split-group indices and totals enumerate submedia, including previews;
    /// they cannot establish how many original video chapters should exist.
    /// Export operations process validated available chapters without this guard.
    pub fn require_complete(&self) -> Result<()> {
        if self.complete {
            Ok(())
        } else {
            Err(invalid(
                "recording completeness is unknown; group metadata cannot verify that all original recording parts are available",
            ))
        }
    }

    fn from_chapters(mut chapters: Vec<RecordingChapter>) -> Result<Self> {
        if chapters.is_empty() || chapters.len() > MAX_CHAPTERS {
            return Err(invalid("invalid recording chapter count"));
        }
        let identity = chapter_group(&chapters[0])?.clone();
        let mut known_total = (identity.total != 0).then_some(identity.total);
        let first_inspection = chapters[0].inspection.clone();
        for chapter in &chapters {
            let group = chapter_group(chapter)?;
            if group.identity != identity.identity || group.capture_type != identity.capture_type {
                return Err(invalid(
                    "chapters do not share a camera recording identity and capture type",
                ));
            }
            if group.total != 0 {
                if known_total.is_some_and(|total| total != group.total) {
                    return Err(invalid("chapters declare conflicting recording totals"));
                }
                known_total = Some(group.total);
            }
            validate_compatibility(&first_inspection, &chapter.inspection)?;
        }
        chapters.sort_by_key(|chapter| chapter.group_index);
        for pair in chapters.windows(2) {
            if pair[0].group_index == pair[1].group_index {
                return Err(invalid("duplicate recording chapter index"));
            }
        }
        let mut duration = Duration::ZERO;
        for chapter in &mut chapters {
            chapter.timeline_start = duration;
            duration = duration
                .checked_add(chapter.duration)
                .ok_or_else(|| invalid("recording duration overflow"))?;
        }
        Ok(Self {
            chapters,
            duration,
            complete: false,
            warnings: Vec::new(),
        })
    }
}

fn capture_start(inspection: &InsvInspection) -> Option<Duration> {
    let metadata = &inspection.metadata;
    if metadata.is_raw_gyro != Some(true)
        || metadata
            .timelapse_interval
            .is_some_and(|interval| interval > 0.0)
        || metadata
            .timelapse_interval_ms
            .is_some_and(|interval| interval > 0)
    {
        return None;
    }
    let timestamp = u64::try_from(metadata.first_frame_timestamp?).ok()?;
    Some(Duration::from_micros(timestamp))
}

fn chapter_group(chapter: &RecordingChapter) -> Result<&RecordingGroup> {
    let metadata = &chapter.inspection.metadata;
    if metadata.sequence_metadata_invalid || metadata.file_split_type != Some(FileSplitType::Split)
    {
        return Err(invalid(
            "temporal grouping requires an unambiguous split-recording declaration",
        ));
    }
    metadata
        .recording_group
        .as_ref()
        .filter(|group| !group.identity.is_empty())
        .ok_or_else(|| invalid("split recording is missing its group identity"))
}

fn validate_sequence_metadata(inspection: &InsvInspection) -> Result<()> {
    if inspection.metadata.sequence_metadata_invalid {
        Err(invalid(
            "recording has malformed or conflicting sequence metadata; select a single chapter explicitly",
        ))
    } else {
        Ok(())
    }
}

fn canonical_paths(inputs: &InputSet) -> Result<Vec<PathBuf>> {
    inputs
        .paths()
        .iter()
        .map(|path| fs::canonicalize(path).map_err(|error| io_error(path, error)))
        .collect()
}

fn inspect_chapter(inputs: InputSet) -> Result<RecordingChapter> {
    let mut inspections = Vec::new();
    for path in inputs.paths() {
        let file = File::open(path).map_err(|error| io_error(path, error))?;
        inspections.push(InsvReader::new(file)?.inspect()?);
    }
    let inspection = inspections.remove(0);
    for secondary in inspections {
        if inspection.metadata.recording_group != secondary.metadata.recording_group
            || inspection.metadata.file_split_type != secondary.metadata.file_split_type
            || inspection.metadata.serial != secondary.metadata.serial
            || inspection.metadata.camera_name != secondary.metadata.camera_name
        {
            return Err(invalid(
                "simultaneous lens files disagree about recording identity",
            ));
        }
        if inspection.duration != secondary.duration
            || inspection.fps != secondary.fps
            || !inspection
                .video_tracks
                .iter()
                .map(|track| (track.width, track.height))
                .eq(secondary
                    .video_tracks
                    .iter()
                    .map(|track| (track.width, track.height)))
            || inspection.metadata.crop_window != secondary.metadata.crop_window
            || inspection.metadata.file_rotation != secondary.metadata.file_rotation
            || inspection.metadata.stream_type != secondary.metadata.stream_type
            || inspection.metadata.pano_record_type != secondary.metadata.pano_record_type
            || inspection.metadata.gamma_mode != secondary.metadata.gamma_mode
            || inspection.metadata.recorded_color_mode != secondary.metadata.recorded_color_mode
            || inspection.metadata.recorded_color_mode_invalid
                != secondary.metadata.recorded_color_mode_invalid
            || (!secondary.metadata.offsets.is_empty()
                && inspection.metadata.offsets != secondary.metadata.offsets)
            || (!secondary.metadata.profiles.is_empty()
                && inspection.metadata.profiles != secondary.metadata.profiles)
            || inspection.metadata.offset_state != secondary.metadata.offset_state
        {
            return Err(invalid(
                "simultaneous lens files disagree about video timing, geometry, calibration, or color",
            ));
        }
    }
    let duration = inspection
        .duration
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| invalid("recording chapter has no positive video duration"))?;
    let group_index = inspection
        .metadata
        .recording_group
        .as_ref()
        .map(|group| group.index);
    Ok(RecordingChapter {
        inputs,
        inspection,
        timeline_start: Duration::ZERO,
        duration,
        group_index,
    })
}

fn validate_compatibility(first: &InsvInspection, next: &InsvInspection) -> Result<()> {
    if first.metadata.serial.is_none()
        || first.metadata.serial != next.metadata.serial
        || first.metadata.camera_name != next.metadata.camera_name
        || first.video_tracks != next.video_tracks
        || first.fps != next.fps
        || first.metadata.reverse_video_track_order != next.metadata.reverse_video_track_order
        || first.metadata.crop_window != next.metadata.crop_window
        || first.metadata.gamma_mode != next.metadata.gamma_mode
        || first.metadata.recorded_color_mode != next.metadata.recorded_color_mode
    {
        return Err(invalid(
            "recording chapters have incompatible camera, video, timing, or color properties",
        ));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidMedia(message.into())
}
