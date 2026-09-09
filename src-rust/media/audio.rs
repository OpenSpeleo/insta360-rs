//! Original audio packets in the same timestamp space as the stitched video.

use crate::{Error, RecordingSequence, Result};
use ffmpeg::util::mathematics::rescale::Rescale;
use ffmpeg_next as ffmpeg;

pub(super) struct AudioTrack {
    pub source_index: usize,
    pub time_base: ffmpeg::Rational,
    pub parameters: ffmpeg::codec::Parameters,
    metadata: ffmpeg::Dictionary<'static>,
    disposition: i32,
}

pub(super) struct AudioChapter {
    pub tracks: Vec<AudioTrack>,
    pub video_origin_micros: i64,
    pub start_micros: i64,
    pub end_micros: i64,
}

pub(super) struct AudioLayout {
    pub chapters: Vec<AudioChapter>,
}

impl AudioLayout {
    pub fn inspect(sequence: &RecordingSequence) -> Result<Self> {
        let mut chapters: Vec<AudioChapter> = Vec::with_capacity(sequence.chapters.len());
        for chapter in &sequence.chapters {
            let input = crate::stream::open_input(&chapter.inputs.paths()[0])?;
            let video = input
                .streams()
                .find(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
                .ok_or_else(|| {
                    Error::InvalidMedia("recording chapter has no video stream".into())
                })?;
            let video_origin_micros = if video.start_time() == ffmpeg::ffi::AV_NOPTS_VALUE {
                0
            } else {
                video.start_time().rescale(
                    video.time_base(),
                    ffmpeg::util::mathematics::rescale::TIME_BASE,
                )
            };
            let mut tracks = Vec::new();
            for stream in input
                .streams()
                .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio)
            {
                let parameters = stream.parameters().clone();
                if !matches!(
                    parameters.id(),
                    ffmpeg::codec::Id::AAC | ffmpeg::codec::Id::ALAC
                ) {
                    return Err(Error::MissingCapability(format!("original {} audio cannot currently be copied into stitched MP4; disable original audio", parameters.id().name())));
                }
                if stream.time_base().numerator() <= 0 || stream.time_base().denominator() <= 0 {
                    return Err(Error::InvalidMedia(
                        "audio stream has an invalid time base".into(),
                    ));
                }
                // SAFETY: the input owns this live stream, and only the scalar
                // disposition value is retained; parameters and tags are cloned.
                let disposition = unsafe { (*stream.as_ptr()).disposition };
                tracks.push(AudioTrack {
                    source_index: stream.index(),
                    time_base: stream.time_base(),
                    parameters,
                    metadata: stream.metadata().to_owned(),
                    disposition,
                });
            }
            if let Some(first) = chapters.first() {
                if tracks.len() != first.tracks.len()
                    || tracks
                        .iter()
                        .zip(&first.tracks)
                        .any(|(a, b)| !compatible(&a.parameters, &b.parameters))
                {
                    return Err(Error::MissingCapability("audio track configuration changes between recording chapters; disable original audio".into()));
                }
            }
            let start_micros = i64::try_from(chapter.timeline_start.as_micros())
                .map_err(|_| Error::InvalidMedia("chapter time overflow".into()))?;
            let end_micros = chapter
                .timeline_start
                .checked_add(chapter.duration)
                .and_then(|end| i64::try_from(end.as_micros()).ok())
                .ok_or_else(|| Error::InvalidMedia("chapter time overflow".into()))?;
            chapters.push(AudioChapter {
                tracks,
                video_origin_micros,
                start_micros,
                end_micros,
            });
        }
        Ok(Self { chapters })
    }

    pub fn track_count(&self) -> usize {
        self.chapters
            .first()
            .map_or(0, |chapter| chapter.tracks.len())
    }

    pub fn add_output_streams(
        &self,
        output: &mut ffmpeg::format::context::Output,
    ) -> Result<Vec<usize>> {
        let mut indices = Vec::new();
        let Some(chapter) = self.chapters.first() else {
            return Ok(indices);
        };
        for track in &chapter.tracks {
            let mut stream = output
                .add_stream(None::<ffmpeg::Codec>)
                .map_err(|error| Error::Media(format!("adding copied audio stream: {error}")))?;
            stream.set_parameters(track.parameters.clone());
            stream.set_time_base(track.time_base);
            stream.set_metadata(track.metadata.clone());
            // SAFETY: this output exclusively owns its new stream and codec
            // parameters. Source-specific codec tags must not leak into MP4.
            unsafe {
                (*(*stream.as_mut_ptr()).codecpar).codec_tag = 0;
                (*stream.as_mut_ptr()).disposition = track.disposition;
            }
            indices.push(stream.index());
        }
        Ok(indices)
    }
}

fn compatible(a: &ffmpeg::codec::Parameters, b: &ffmpeg::codec::Parameters) -> bool {
    // SAFETY: both parameter blocks and their extradata live for this borrow.
    // A malformed extradata length is rejected rather than dereferenced.
    unsafe {
        let a = &*a.as_ptr();
        let b = &*b.as_ptr();
        if a.codec_id != b.codec_id
            || a.format != b.format
            || a.sample_rate != b.sample_rate
            || a.profile != b.profile
            || a.extradata_size < 0
            || b.extradata_size < 0
            || a.extradata_size != b.extradata_size
            || ffmpeg::ffi::av_channel_layout_compare(&a.ch_layout, &b.ch_layout) != 0
        {
            return false;
        }
        if a.extradata_size == 0 {
            return true;
        }
        if a.extradata.is_null() || b.extradata.is_null() {
            return false;
        }
        std::slice::from_raw_parts(a.extradata, a.extradata_size as usize)
            == std::slice::from_raw_parts(b.extradata, b.extradata_size as usize)
    }
}

/// Rebase original packet timing using the video origin, keeping only complete
/// compressed packets inside the selected interval. No sample data is changed.
pub(super) fn prepare_packet(
    chapter: &AudioChapter,
    packet: &mut ffmpeg::Packet,
    output_time_base: ffmpeg::Rational,
    output_index: usize,
    origin_micros: i64,
    end_micros: i64,
) -> Result<bool> {
    let track = chapter
        .tracks
        .iter()
        .find(|track| track.source_index == packet.stream())
        .ok_or_else(|| {
            Error::InvalidMedia("audio packet refers to an unknown source track".into())
        })?;
    if packet.is_corrupt() {
        return Err(Error::InvalidMedia("copied audio packet is corrupt".into()));
    }
    let pts = packet
        .pts()
        .ok_or_else(|| Error::InvalidMedia("audio copy requires presentation timestamps".into()))?;
    let dts = packet
        .dts()
        .ok_or_else(|| Error::InvalidMedia("audio copy requires decoding timestamps".into()))?;
    if packet.duration() <= 0 {
        return Err(Error::InvalidMedia(
            "audio copy requires positive packet durations".into(),
        ));
    }
    let shift = chapter
        .start_micros
        .checked_sub(chapter.video_origin_micros)
        .ok_or_else(|| Error::InvalidMedia("audio timestamp overflow".into()))?;
    let packet_start = timestamp_micros(pts, track.time_base)?
        .checked_add(shift)
        .ok_or_else(|| Error::InvalidMedia("audio timestamp overflow".into()))?;
    let packet_end = timestamp_micros(
        pts.checked_add(packet.duration())
            .ok_or_else(|| Error::InvalidMedia("audio timestamp overflow".into()))?,
        track.time_base,
    )?
    .checked_add(shift)
    .ok_or_else(|| Error::InvalidMedia("audio timestamp overflow".into()))?;
    if packet_start < origin_micros.max(chapter.start_micros)
        || packet_end > end_micros.min(chapter.end_micros)
    {
        return Ok(false);
    }
    let shift = shift
        .checked_sub(origin_micros)
        .ok_or_else(|| Error::InvalidMedia("audio timestamp overflow".into()))?
        .rescale(
            ffmpeg::util::mathematics::rescale::TIME_BASE,
            output_time_base,
        );
    let rebased = |value: i64| {
        value
            .rescale(track.time_base, output_time_base)
            .checked_add(shift)
            .ok_or_else(|| Error::InvalidMedia("audio timestamp overflow".into()))
    };
    packet.set_pts(Some(rebased(pts)?));
    packet.set_dts(Some(rebased(dts)?));
    packet.set_duration(packet.duration().rescale(track.time_base, output_time_base));
    packet.set_position(-1);
    packet.set_stream(output_index);
    Ok(true)
}

fn timestamp_micros(value: i64, base: ffmpeg::Rational) -> Result<i64> {
    let ticks = i128::from(value) * i128::from(base.numerator()) * 1_000_000;
    i64::try_from(ticks / i128::from(base.denominator()))
        .map_err(|_| Error::InvalidMedia("audio timestamp overflow".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter() -> AudioChapter {
        AudioChapter {
            tracks: vec![AudioTrack {
                source_index: 2,
                time_base: (1, 48_000).into(),
                parameters: ffmpeg::codec::Parameters::new(),
                metadata: ffmpeg::Dictionary::new(),
                disposition: 0,
            }],
            video_origin_micros: 2_000_000,
            start_micros: 5_000_000,
            end_micros: 6_000_000,
        }
    }

    fn packet(pts: i64) -> ffmpeg::Packet {
        let mut packet = ffmpeg::Packet::copy(b"unchanged compressed audio bytes");
        packet.set_stream(2);
        packet.set_pts(Some(pts));
        packet.set_dts(Some(pts));
        packet.set_duration(1024);
        packet
    }

    #[test]
    fn copied_payload_and_av_offset_survive_shared_origin_and_time_base_conversion() {
        let mut packet = packet(100_800); // 100 ms after the chapter's video start.
        assert!(prepare_packet(
            &chapter(),
            &mut packet,
            (1, 96_000).into(),
            1,
            5_000_000,
            6_000_000
        )
        .unwrap());
        assert_eq!(packet.data().unwrap(), b"unchanged compressed audio bytes");
        assert_eq!(packet.pts(), Some(9600));
        assert_eq!(packet.dts(), Some(9600));
        assert_eq!(packet.duration(), 2048);
        assert_eq!(packet.stream(), 1);
    }

    #[test]
    fn cuts_only_keep_complete_packets_and_do_not_shift_audio_independently() {
        let chapter = chapter();
        let mut leading = packet(100_800);
        assert!(!prepare_packet(
            &chapter,
            &mut leading,
            (1, 48_000).into(),
            1,
            5_110_000,
            6_000_000
        )
        .unwrap());
        let mut trailing = packet(100_800);
        assert!(!prepare_packet(
            &chapter,
            &mut trailing,
            (1, 48_000).into(),
            1,
            5_000_000,
            5_110_000
        )
        .unwrap());
        let mut included = packet(101_824);
        assert!(prepare_packet(
            &chapter,
            &mut included,
            (1, 48_000).into(),
            1,
            5_110_000,
            6_000_000
        )
        .unwrap());
        assert_eq!(included.pts(), Some(544));
    }

    #[test]
    fn missing_timing_corruption_and_overflow_fail_instead_of_guessing() {
        let chapter = chapter();
        let mut no_pts = packet(100_800);
        no_pts.set_pts(None);
        let mut no_dts = packet(100_800);
        no_dts.set_dts(None);
        let mut no_duration = packet(100_800);
        no_duration.set_duration(0);
        let mut corrupt = packet(100_800);
        corrupt.set_flags(ffmpeg::packet::Flags::CORRUPT);
        for mut packet in [no_pts, no_dts, no_duration, corrupt, packet(i64::MAX)] {
            assert!(prepare_packet(
                &chapter,
                &mut packet,
                (1, 48_000).into(),
                1,
                5_000_000,
                6_000_000
            )
            .is_err());
        }
    }
}
