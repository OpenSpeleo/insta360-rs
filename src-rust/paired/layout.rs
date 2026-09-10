//! One validated A/B mapping shared by sequential decoding and preview workers.

use crate::calibration::{parse_offset, CalibrationCandidate, OffsetSource};
use crate::container::{FileCategory, FileRotation, PanoRecordType, StreamType};
use crate::{Error, RecordingChapter, Result};

#[derive(Clone, Copy, Debug)]
pub(super) struct LensSource {
    pub input: usize,
    pub video: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DecodedLayout {
    pub(super) lenses: [LensSource; 2],
    pub(super) packed: Option<(u32, u32)>,
}

impl DecodedLayout {
    pub(crate) fn lens_width(&self, chapter: &RecordingChapter) -> u32 {
        self.packed
            .map_or(chapter.inspection.video_tracks[0].width, |(width, _)| {
                width / 2
            })
    }

    pub(crate) fn inspect(chapter: &RecordingChapter) -> Result<Self> {
        let metadata = &chapter.inspection.metadata;
        if metadata
            .file_rotation
            .is_some_and(|rotation| rotation != FileRotation::Degrees0)
        {
            return Err(missing(
                "recorded rotation has no prepared decoded lens transform",
            ));
        }
        if matches!(
            metadata.file_category,
            Some(
                FileCategory::Plane
                    | FileCategory::WideAngle
                    | FileCategory::EquirectangularPanorama
                    | FileCategory::DoubleHalfFisheyePanorama
                    | FileCategory::Other(_)
            )
        ) || matches!(metadata.pano_record_type, Some(PanoRecordType::Other(_)))
        {
            return Err(missing(
                "recorded projection is not a supported full dual-fisheye source",
            ));
        }
        match (
            chapter.inputs.paths().len(),
            chapter.inspection.video_tracks.len(),
        ) {
            (1, 2) => {
                if matches!(
                    metadata.stream_type,
                    Some(
                        StreamType::SingleStreamFile
                            | StreamType::DualStreamFiles
                            | StreamType::Other(_)
                    )
                ) || metadata.pano_record_type == Some(PanoRecordType::SplitFile)
                {
                    return Err(missing("recorded layout contradicts its two video tracks"));
                }
                let reverse = metadata.reverse_video_track_order.ok_or_else(|| {
                    missing("recording does not declare which video track belongs to camera A/B")
                })?;
                Ok(Self {
                    packed: None,
                    lenses: [
                        LensSource {
                            input: 0,
                            video: usize::from(reverse),
                        },
                        LensSource {
                            input: 0,
                            video: usize::from(!reverse),
                        },
                    ],
                })
            }
            (1, 1) => Self::packed(chapter),
            (2, 1) => {
                if matches!(
                    metadata.stream_type,
                    Some(
                        StreamType::SingleStreamFile
                            | StreamType::DualStreamTracks
                            | StreamType::DualStreamTracksReversed
                            | StreamType::Other(_)
                    )
                ) || metadata.pano_record_type == Some(PanoRecordType::MultiTrack)
                {
                    return Err(missing(
                        "recorded layout contradicts the legacy lens-file pair",
                    ));
                }
                // InputSet validates a common filename identity and orders _00_
                // before _10_. Those identify lenses independently of caller order.
                Ok(Self {
                    packed: None,
                    lenses: [
                        LensSource { input: 0, video: 0 },
                        LensSource { input: 1, video: 0 },
                    ],
                })
            }
            _ => Err(missing(
                "paired decoding requires declared dual tracks, a validated _00_/_10_ pair, or proven packed lens coordinates",
            )),
        }
    }

    fn packed(chapter: &RecordingChapter) -> Result<Self> {
        let metadata = &chapter.inspection.metadata;
        let track = &chapter.inspection.video_tracks[0];
        let normal_video_name = chapter.inputs.paths()[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("VID_"));
        if metadata.file_category != Some(FileCategory::DoubleFisheyePanorama)
            || metadata.stream_type != Some(StreamType::SingleStreamFile)
            || metadata.file_rotation != Some(FileRotation::Degrees0)
            || matches!(
                metadata.pano_record_type,
                Some(PanoRecordType::SplitFile | PanoRecordType::MultiTrack)
            )
            || !normal_video_name
            || track.width == 0
            || track.height == 0
            || !track.width.is_multiple_of(4)
            || !track.height.is_multiple_of(2)
            || track.height.checked_mul(2) != Some(track.width)
        {
            return Err(missing(
                "packed decoding requires an unrotated VID_ single-stream full dual-fisheye recording with an even 2:1 canvas",
            ));
        }
        // Native RenderFactory::AutoJudgeImageLayout maps VID_ with one track to
        // HorizontalMerged. Its UV transform preserves calibration coordinates,
        // unlike the separate-lens x*2 transform. Prove A-left/B-right ownership;
        // neither dimensions nor filenames alone are sufficient.
        let offset = metadata
            .offsets
            .iter()
            .filter(|offset| !offset.original)
            .max_by_key(|offset| offset.version)
            .ok_or_else(|| missing("packed source requires a current two-lens calibration"))?;
        let calibration = parse_offset(
            &CalibrationCandidate::new(offset.version, None, &offset.value),
            OffsetSource::Current,
        )?;
        let half = f64::from(calibration.canvas_width) / 2.0;
        if calibration.canvas_width != calibration.canvas_height.saturating_mul(2)
            || calibration.lenses.iter().enumerate().any(|(index, lens)| {
                lens.cx < half * index as f64
                    || lens.cx >= half * (index + 1) as f64
                    || lens.cy < 0.0
                    || lens.cy >= f64::from(calibration.canvas_height)
            })
        {
            return Err(missing(
                "packed calibration does not prove left-camera A and right-camera B source windows",
            ));
        }
        Ok(Self {
            packed: Some((track.width, track.height)),
            lenses: [LensSource { input: 0, video: 0 }; 2],
        })
    }
}

fn missing(message: &str) -> Error {
    Error::MissingCapability(message.into())
}
