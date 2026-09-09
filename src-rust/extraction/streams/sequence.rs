//! One continuous packet-copy destination per original camera/audio stream.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use ffmpeg::util::mathematics::rescale::Rescale;

use super::*;

pub(in crate::extraction) struct SequenceInput<'a> {
    pub copies: &'a mut SequenceCopies,
    pub chapter_index: usize,
    pub input_index: usize,
    pub timeline_start: Duration,
    pub duration: Duration,
    pub output_dir: &'a Path,
    pub camera_order: Option<bool>,
    pub expected_videos_per_file: usize,
}

pub(in crate::extraction) struct SequenceCopies {
    tracks: BTreeMap<(usize, usize), TrackCopy>,
    warnings: Vec<String>,
    video_count: usize,
    audio_count: usize,
    chapter_count: usize,
}

struct TrackCopy {
    signature: Value,
    muxer: Option<Muxer>,
    time_base: ffmpeg::Rational,
    shift: i64,
    origin: i64,
    duration: i64,
    audio: bool,
    last_dts: Option<i64>,
    packet_count: u64,
    skipped_packets: u64,
    chapters: BTreeSet<usize>,
    parts: Vec<Value>,
}

impl SequenceCopies {
    pub(in crate::extraction) fn new() -> Self {
        Self {
            tracks: BTreeMap::new(),
            warnings: Vec::new(),
            video_count: 0,
            audio_count: 0,
            chapter_count: 0,
        }
    }

    pub(in crate::extraction) fn finish(
        mut self,
        output_dir: &Path,
    ) -> Result<ComponentExtraction> {
        let mut files = Vec::new();
        let mut descriptions = Vec::new();
        for ((input_index, stream_index), mut track) in self.tracks {
            if track.chapters.len() != self.chapter_count {
                disable(
                    &mut track,
                    output_dir,
                    &mut self.warnings,
                    "stream is absent from one or more chapters",
                )?;
            }
            let mut media_file = None;
            if let Some(mut muxer) = track.muxer.take() {
                match muxer
                    .context
                    .write_trailer()
                    .and_then(|()| muxer.check_io())
                {
                    Ok(()) => {
                        media_file = Some(muxer.relative.clone());
                        files.push(muxer.relative.clone());
                    }
                    Err(error) => {
                        self.warnings.push(format!("Continuous stream {input_index}:{stream_index} could not finalize: {error}; raw chapters are preserved."));
                        muxer.discard(output_dir)?;
                    }
                }
            }
            descriptions.push(json!({
                "input_index": input_index, "stream_index": stream_index,
                "media_file": media_file, "packet_count": track.packet_count,
                "boundary_packets_omitted": track.skipped_packets,
                "parts": track.parts,
            }));
        }
        Ok(ComponentExtraction {
            item_count: descriptions.len(),
            description: json!(descriptions),
            files,
            warnings: self.warnings,
        })
    }
}

impl SequenceInput<'_> {
    pub(in crate::extraction) fn begin(
        &mut self,
        source: &ffmpeg::format::context::Input,
    ) -> Result<()> {
        self.copies.chapter_count = self.copies.chapter_count.max(self.chapter_index + 1);
        if source
            .streams()
            .filter(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)
            .count()
            != self.expected_videos_per_file
        {
            // Additional proxy streams have no proven lens role. Keep neutral
            // stream names while archiving every component without assumptions.
            self.camera_order = None;
        }
        let origin_stream = source
            .streams()
            .find(|s| s.parameters().medium() == ffmpeg::media::Type::Video)
            .or_else(|| {
                source
                    .streams()
                    .find(|s| s.parameters().medium() == ffmpeg::media::Type::Audio)
            });
        let Some(origin_stream) = origin_stream else {
            return Ok(());
        };
        let origin = timestamp(origin_stream.start_time());
        let origin_tb = origin_stream.time_base();
        for stream in source.streams() {
            let medium = stream.parameters().medium();
            if !matches!(
                medium,
                ffmpeg::media::Type::Video | ffmpeg::media::Type::Audio
            ) {
                continue;
            }
            let key = (self.input_index, stream.index());
            let signature = codec_signature(&stream)?;
            if self.chapter_index == 0 {
                let stem = if medium == ffmpeg::media::Type::Video {
                    let ordinal = self.copies.video_count;
                    self.copies.video_count += 1;
                    match (self.camera_order, ordinal) {
                        (Some(reverse), 0 | 1) => format!(
                            "camera_{}",
                            if (ordinal == 0) != reverse { "A" } else { "B" }
                        ),
                        _ => format!("video_{:03}", ordinal + 1),
                    }
                } else {
                    self.copies.audio_count += 1;
                    format!("audio_{:03}", self.copies.audio_count)
                };
                let muxer = Muxer::try_new_named(
                    source,
                    &stream,
                    self.output_dir,
                    Path::new(""),
                    &stem,
                    true,
                    &mut self.copies.warnings,
                )?;
                self.copies.tracks.insert(
                    key,
                    TrackCopy {
                        signature: signature.clone(),
                        muxer,
                        time_base: stream.time_base(),
                        shift: 0,
                        origin: 0,
                        duration: 0,
                        audio: medium == ffmpeg::media::Type::Audio,
                        last_dts: None,
                        packet_count: 0,
                        skipped_packets: 0,
                        chapters: BTreeSet::new(),
                        parts: Vec::new(),
                    },
                );
            }
            let Some(track) = self.copies.tracks.get_mut(&key) else {
                self.copies.warnings.push(format!(
                    "Chapter {} adds stream {}:{}; only its original archive is available.",
                    self.chapter_index + 1,
                    self.input_index,
                    stream.index()
                ));
                continue;
            };
            track.chapters.insert(self.chapter_index);
            if track.signature != signature {
                disable(
                    track,
                    self.output_dir,
                    &mut self.copies.warnings,
                    "codec configuration changes between chapters",
                )?;
            }
            track.time_base = stream.time_base();
            let Some(origin) = origin else {
                disable(
                    track,
                    self.output_dir,
                    &mut self.copies.warnings,
                    "source has no video/audio time origin",
                )?;
                continue;
            };
            let Some(muxer) = track.muxer.as_ref() else {
                continue;
            };
            validate_time_base(track.time_base)?;
            validate_time_base(origin_tb)?;
            validate_time_base(muxer.time_base)?;
            track.origin = origin.rescale(origin_tb, muxer.time_base);
            track.shift = duration_ticks(self.timeline_start, muxer.time_base)?
                .checked_sub(track.origin)
                .ok_or_else(|| {
                    Error::InvalidMedia("continuous stream timestamp overflow".into())
                })?;
            track.duration = duration_ticks(self.duration, muxer.time_base)?;
            track.parts.push(json!({
                "chapter_index": self.chapter_index,
                "timeline_start_ns": self.timeline_start.as_nanos().to_string(),
                "duration_ns": self.duration.as_nanos().to_string(),
                "source_origin": origin, "source_origin_time_base": rational(origin_tb),
                "output_time_base": rational(muxer.time_base), "timestamp_shift": track.shift,
            }));
        }
        Ok(())
    }

    pub(in crate::extraction) fn write_packet(
        &mut self,
        packet: &mut ffmpeg::Packet,
    ) -> Result<bool> {
        let key = (self.input_index, packet.stream());
        let Some(track) = self.copies.tracks.get_mut(&key) else {
            return Ok(false);
        };
        let Some(muxer) = track.muxer.as_mut() else {
            return Ok(false);
        };
        packet.rescale_ts(track.time_base, muxer.time_base);
        let (Some(pts), Some(dts)) = (packet.pts(), packet.dts()) else {
            disable(
                track,
                self.output_dir,
                &mut self.copies.warnings,
                "packet timestamps are missing",
            )?;
            return Ok(false);
        };
        let local = pts
            .checked_sub(track.origin)
            .ok_or_else(|| Error::InvalidMedia("packet timestamp overflow".into()))?;
        // Original bytes/indexes always retain these boundary packets. Only the
        // convenience audio copy omits repeated priming or audio beyond video.
        if track.audio && ((self.chapter_index > 0 && local < 0) || local >= track.duration) {
            track.skipped_packets += 1;
            return Ok(false);
        }
        let pts = pts
            .checked_add(track.shift)
            .ok_or_else(|| Error::InvalidMedia("packet PTS overflow".into()))?;
        let dts = dts
            .checked_add(track.shift)
            .ok_or_else(|| Error::InvalidMedia("packet DTS overflow".into()))?;
        if track.last_dts.is_some_and(|previous| dts <= previous) {
            disable(
                track,
                self.output_dir,
                &mut self.copies.warnings,
                "chapter packets do not have increasing decode timestamps",
            )?;
            return Ok(false);
        }
        packet.set_pts(Some(pts));
        packet.set_dts(Some(dts));
        packet.set_stream(0);
        packet.set_position(-1);
        if let Err(error) = packet.write_interleaved(&mut muxer.context) {
            disable(
                track,
                self.output_dir,
                &mut self.copies.warnings,
                &format!("muxing failed: {error}"),
            )?;
            return Ok(false);
        }
        track.last_dts = Some(dts);
        track.packet_count += 1;
        Ok(true)
    }
}

fn disable(
    track: &mut TrackCopy,
    output: &Path,
    warnings: &mut Vec<String>,
    reason: &str,
) -> Result<()> {
    if let Some(muxer) = track.muxer.take() {
        warnings.push(format!("Continuous {} unavailable: {reason}; every original packet is preserved in its chapter archive.", muxer.relative.display()));
        muxer.discard(output)?;
    }
    Ok(())
}

fn validate_time_base(time_base: ffmpeg::Rational) -> Result<()> {
    if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
        Err(Error::InvalidMedia(
            "continuous stream requires positive time bases".into(),
        ))
    } else {
        Ok(())
    }
}

fn duration_ticks(duration: Duration, time_base: ffmpeg::Rational) -> Result<i64> {
    let nanos = i64::try_from(duration.as_nanos())
        .map_err(|_| Error::InvalidMedia("recording duration exceeds timestamp range".into()))?;
    Ok(nanos.rescale((1, 1_000_000_000), time_base))
}

fn codec_signature(stream: &ffmpeg::Stream<'_>) -> Result<Value> {
    let parameters = stream.parameters();
    // SAFETY: only read scalar fields and a bounded extradata slice while the
    // live input owns its codec parameters and channel layout.
    unsafe {
        let p = &*parameters.as_ptr();
        let size = usize::try_from(p.extradata_size)
            .map_err(|_| Error::InvalidMedia("negative extradata size".into()))?;
        Ok(json!({
            "codec": p.codec_id as i32, "format": p.format,
            "width": p.width, "height": p.height, "profile": p.profile,
            "sample_rate": p.sample_rate, "channels": channel_layout(&p.ch_layout)?,
            "bits_per_coded_sample": p.bits_per_coded_sample,
            "bits_per_raw_sample": p.bits_per_raw_sample,
            "color_range": p.color_range as i32, "color_primaries": p.color_primaries as i32,
            "color_transfer": p.color_trc as i32, "color_space": p.color_space as i32,
            "extradata": hex(checked_bytes(p.extradata, size, MAX_PACKET_BYTES)?),
        }))
    }
}
