//! Packet-preserving extraction and optional single-track remuxing.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use ffmpeg::codec::packet::Ref;
use ffmpeg_next as ffmpeg;
use serde_json::{json, Value};

use super::ComponentExtraction;
use crate::{Error, Result};

const MAX_STREAMS: usize = 256;
const MAX_CHAPTERS: usize = 10_000;
const MAX_SIDE_DATA: usize = 1024;
const MAX_PACKET_BYTES: usize = 256 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_METADATA_ENTRIES: usize = 100_000;

/// Demux once, preserving every packet before passing it to a convenience muxer.
pub(super) fn extract_streams(input: &Path, output_dir: &Path) -> Result<ComponentExtraction> {
    ffmpeg::init().map_err(|error| media_error("initializing stream extraction", error))?;
    let file = File::open(input).map_err(|error| crate::error::io_error(input, error))?;
    let io = ffmpeg::format::context::StreamIo::from_read_seek(file)
        .map_err(|error| media_error("opening input stream", error))?;
    let mut options = ffmpeg::Dictionary::new();
    options.set("max_streams", &MAX_STREAMS.to_string());
    options.set("probesize", "33554432");
    options.set("analyzeduration", "10000000");
    options.set("enable_drefs", "0");
    options.set("protocol_whitelist", "file");
    let mut source = ffmpeg::format::input_from_stream(io, None, Some(options))
        .map_err(|error| media_error("opening media for extraction", error))?;
    if source.nb_streams() as usize > MAX_STREAMS || source.nb_chapters() as usize > MAX_CHAPTERS {
        return Err(Error::InvalidMedia(
            "too many media streams or chapters".into(),
        ));
    }

    let mut metadata_budget = MAX_METADATA_BYTES;
    let tags = metadata(source.metadata(), &mut metadata_budget)?;
    let chapters = source
        .chapters()
        .map(|chapter| {
            Ok(json!({
                "id": chapter.id(), "time_base": rational(chapter.time_base()),
                "start": chapter.start(), "end": chapter.end(),
                "tags": metadata(chapter.metadata(), &mut metadata_budget)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut streams = Vec::with_capacity(source.nb_streams() as usize);
    let mut warnings = Vec::new();
    for stream in source.streams() {
        if stream.index() != streams.len() {
            return Err(Error::InvalidMedia(
                "non-contiguous media stream indices".into(),
            ));
        }
        streams.push(StreamOutput::new(
            &source,
            &stream,
            output_dir,
            &mut metadata_budget,
            &mut warnings,
        )?);
    }

    let mut sequence = 0_u64;
    loop {
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut source) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => {
                // SAFETY: the live input owns its AVIOContext. FFmpeg can report EOF
                // after an I/O failure, so inspect its recorded error as well.
                let io_error = unsafe {
                    let io = (*source.as_ptr()).pb;
                    if io.is_null() {
                        0
                    } else {
                        (*io).error
                    }
                };
                if io_error < 0 && io_error != ffmpeg::ffi::AVERROR_EOF {
                    return Err(media_error(
                        "reading media packets",
                        ffmpeg::Error::from(io_error),
                    ));
                }
                break;
            }
            Err(error) => return Err(media_error("reading media packets", error)),
        }
        let stream = streams.get_mut(packet.stream()).ok_or_else(|| {
            Error::InvalidMedia("packet refers to an unknown media stream".into())
        })?;
        stream.write_packet(&mut packet, sequence, output_dir, &mut warnings)?;
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Error::InvalidMedia("media packet counter overflow".into()))?;
    }

    let mut files = Vec::new();
    let mut descriptions = Vec::with_capacity(streams.len());
    for stream in streams {
        let (description, stream_files) = stream.finish(output_dir, &mut warnings)?;
        descriptions.push(description);
        files.extend(stream_files);
    }
    Ok(ComponentExtraction {
        description: json!({
            "format": source.format().name(),
            "duration_microseconds": timestamp(source.duration()),
            "bit_rate": source.bit_rate(),
            "tags": tags, "chapters": chapters,
            "packet_count": sequence, "streams": descriptions,
        }),
        item_count: descriptions.len(),
        files,
        warnings,
    })
}

struct StreamOutput {
    description: Value,
    index: usize,
    time_base: ffmpeg::Rational,
    payload: OutputFile,
    packets: OutputFile,
    side_data: OutputFile,
    packet_count: u64,
    corrupt: bool,
    files: Vec<PathBuf>,
    muxer: Option<Muxer>,
}

impl StreamOutput {
    fn new(
        source: &ffmpeg::format::context::Input,
        stream: &ffmpeg::Stream<'_>,
        output_dir: &Path,
        metadata_budget: &mut usize,
        warnings: &mut Vec<String>,
    ) -> Result<Self> {
        let index = stream.index();
        let directory = PathBuf::from(format!("streams/{index:03}"));
        let absolute = output_dir.join(&directory);
        std::fs::create_dir_all(&absolute)
            .map_err(|error| crate::error::io_error(&absolute, error))?;
        let payload = OutputFile::new(output_dir, directory.join("packets.bin"))?;
        let packets = OutputFile::new(output_dir, directory.join("packets.jsonl"))?;
        let mut side_data = OutputFile::new(output_dir, directory.join("side_data.bin"))?;
        let mut extradata = OutputFile::new(output_dir, directory.join("extradata.bin"))?;
        let parameters = stream.parameters();
        // SAFETY: the parameters borrow their live stream; scalar fields and the
        // checked extradata slice are only read during this borrow.
        let codec = unsafe {
            let parameters = &*parameters.as_ptr();
            let size = usize::try_from(parameters.extradata_size)
                .map_err(|_| Error::InvalidMedia("negative codec extradata size".into()))?;
            extradata.append(checked_bytes(parameters.extradata, size, MAX_PACKET_BYTES)?)?;
            json!({
                "id": parameters.codec_id as i32, "name": stream.parameters().id().name(),
                "tag": parameters.codec_tag, "format": parameters.format,
                "bit_rate": parameters.bit_rate, "profile": parameters.profile,
                "level": parameters.level,
                "width": parameters.width, "height": parameters.height,
                "sample_aspect_ratio": {"numerator": parameters.sample_aspect_ratio.num, "denominator": parameters.sample_aspect_ratio.den},
                "field_order": parameters.field_order as i32,
                "color_range": parameters.color_range as i32,
                "color_primaries": parameters.color_primaries as i32,
                "color_transfer": parameters.color_trc as i32,
                "color_space": parameters.color_space as i32,
                "chroma_location": parameters.chroma_location as i32,
                "video_delay": parameters.video_delay,
                "sample_rate": parameters.sample_rate,
                "channels": parameters.ch_layout.nb_channels,
                "channel_layout": channel_layout(&parameters.ch_layout)?,
                "bits_per_coded_sample": parameters.bits_per_coded_sample,
                "bits_per_raw_sample": parameters.bits_per_raw_sample,
                "block_align": parameters.block_align, "frame_size": parameters.frame_size,
                "initial_padding": parameters.initial_padding,
                "trailing_padding": parameters.trailing_padding,
                "seek_preroll": parameters.seek_preroll,
            })
        };
        extradata.finish()?;
        let stream_side_data = write_side_data(stream.side_data(), &mut side_data)?;
        // SAFETY: the stream remains alive, and the raw disposition preserves
        // bits that the wrapper's bitflags implementation would truncate.
        let disposition = unsafe { (*stream.as_ptr()).disposition };
        let description = json!({
            "index": index, "id": stream.id(),
            "type": medium_name(parameters.medium()), "codec": codec,
            "time_base": rational(stream.time_base()),
            "start_time": timestamp(stream.start_time()), "duration": timestamp(stream.duration()),
            "declared_frame_count": stream.frames(),
            "frame_rate": rational(stream.rate()), "average_frame_rate": rational(stream.avg_frame_rate()),
            "disposition": disposition, "tags": metadata(stream.metadata(), metadata_budget)?,
            "packet_payload": payload.relative, "packet_index": packets.relative,
            "side_data_payload": side_data.relative, "extradata": extradata.relative,
            "side_data": stream_side_data,
        });
        let files = vec![
            payload.relative.clone(),
            packets.relative.clone(),
            side_data.relative.clone(),
            extradata.relative,
        ];
        let muxer = Muxer::try_new(source, stream, output_dir, &directory, warnings)?;
        Ok(Self {
            description,
            index,
            time_base: stream.time_base(),
            payload,
            packets,
            side_data,
            packet_count: 0,
            corrupt: false,
            files,
            muxer,
        })
    }

    fn write_packet(
        &mut self,
        packet: &mut ffmpeg::Packet,
        sequence: u64,
        output_dir: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        if packet.size() > MAX_PACKET_BYTES {
            return Err(Error::InvalidMedia(
                "media packet exceeds extraction size limit".into(),
            ));
        }
        let offset = self.payload.offset;
        let bytes = packet.data().unwrap_or_default();
        if bytes.len() != packet.size() {
            return Err(Error::InvalidMedia(
                "media packet has no payload buffer".into(),
            ));
        }
        self.payload.append(bytes)?;
        let side_data = write_side_data(packet.side_data(), &mut self.side_data)?;
        // SAFETY: the packet owns its AVPacket; reading the raw bits avoids loss
        // of flags not represented by ffmpeg-next's bitflags.
        let flags = unsafe { (*packet.as_ptr()).flags };
        self.packets.json_line(&json!({
            "sequence": sequence, "packet": self.packet_count,
            "offset": offset, "size": bytes.len(),
            "pts": packet.pts(), "dts": packet.dts(), "duration": packet.duration(),
            "time_base": rational(self.time_base), "source_position": packet.position(),
            "flags": flags, "key_frame": packet.is_key(), "corrupt": packet.is_corrupt(),
            "side_data": side_data,
        }))?;
        self.packet_count = self
            .packet_count
            .checked_add(1)
            .ok_or_else(|| Error::InvalidMedia("media packet counter overflow".into()))?;
        self.corrupt |= packet.is_corrupt();
        if let Some(muxer) = self.muxer.as_mut() {
            packet.rescale_ts(self.time_base, muxer.time_base);
            packet.set_position(-1);
            packet.set_stream(0);
            if let Err(error) = packet.write_interleaved(&mut muxer.context) {
                warnings.push(format!(
                    "Stream {} could not be remuxed: {error}; original packets are preserved.",
                    self.index
                ));
                if let Some(muxer) = self.muxer.take() {
                    muxer.discard(output_dir)?;
                }
            }
        }
        Ok(())
    }

    fn finish(
        mut self,
        output_dir: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<(Value, Vec<PathBuf>)> {
        self.payload.finish()?;
        self.packets.finish()?;
        self.side_data.finish()?;
        if self.corrupt {
            warnings.push(format!(
                "Stream {} contains packets marked corrupt; their original bytes are preserved.",
                self.index
            ));
        }
        if let Some(mut muxer) = self.muxer.take() {
            match muxer
                .context
                .write_trailer()
                .and_then(|()| muxer.check_io())
            {
                Ok(()) => {
                    self.description["media_file"] = json!(muxer.relative);
                    self.files.push(muxer.relative.clone());
                }
                Err(error) => {
                    warnings.push(format!("Stream {} could not finish remuxing: {error}; original packets are preserved.", self.index));
                    muxer.discard(output_dir)?;
                }
            }
        }
        self.description["packet_count"] = json!(self.packet_count);
        self.description["packet_bytes"] = json!(self.payload.offset);
        let mut info = OutputFile::new(
            output_dir,
            PathBuf::from(format!("streams/{:03}/metadata.json", self.index)),
        )?;
        info.json_line(&self.description)?;
        info.finish()?;
        self.files.push(info.relative);
        Ok((self.description, self.files))
    }
}

struct Muxer {
    context: ffmpeg::format::context::Output,
    time_base: ffmpeg::Rational,
    relative: PathBuf,
}

impl Muxer {
    fn try_new(
        source: &ffmpeg::format::context::Input,
        stream: &ffmpeg::Stream<'_>,
        output_dir: &Path,
        directory: &Path,
        warnings: &mut Vec<String>,
    ) -> Result<Option<Self>> {
        let parameters = stream.parameters();
        let candidates: &[(&str, &str)] = match (parameters.medium(), parameters.id()) {
            (
                ffmpeg::media::Type::Video,
                ffmpeg::codec::Id::H264
                | ffmpeg::codec::Id::HEVC
                | ffmpeg::codec::Id::MPEG4
                | ffmpeg::codec::Id::AV1,
            ) => &[("mp4", "mp4"), ("matroska", "mkv")],
            (ffmpeg::media::Type::Video, _) => &[("matroska", "mkv")],
            (ffmpeg::media::Type::Audio, ffmpeg::codec::Id::AAC | ffmpeg::codec::Id::ALAC) => {
                &[("mp4", "m4a"), ("matroska", "mka")]
            }
            (ffmpeg::media::Type::Audio, _) => &[("matroska", "mka")],
            _ => return Ok(None),
        };
        let mut failures = Vec::new();
        for &(format, extension) in candidates {
            let relative = directory.join(format!("media.{extension}"));
            let absolute = output_dir.join(&relative);
            let file = create_file(&absolute)?;
            let attempt = (|| {
                let io = ffmpeg::format::context::StreamIo::from_write_seek(file)?;
                let mut context = ffmpeg::format::output_to_stream(io, None, Some(format))?;
                {
                    let mut output = context.add_stream(None::<ffmpeg::Codec>)?;
                    // SAFETY: both live streams own their parameters/dictionaries.
                    // The FFI copies allocations; no source pointers are retained.
                    unsafe {
                        let parameters = (*output.as_mut_ptr()).codecpar;
                        let status = ffmpeg::ffi::avcodec_parameters_copy(
                            parameters,
                            stream.parameters().as_ptr(),
                        );
                        if status < 0 {
                            return Err(ffmpeg::Error::from(status));
                        }
                        (*parameters).codec_tag = 0;
                        (*output.as_mut_ptr()).disposition = (*stream.as_ptr()).disposition;
                        let status = ffmpeg::ffi::av_dict_copy(
                            &mut (*output.as_mut_ptr()).metadata,
                            stream.metadata().as_ptr(),
                            0,
                        );
                        if status < 0 {
                            return Err(ffmpeg::Error::from(status));
                        }
                    }
                    output.set_time_base(stream.time_base());
                    output.set_rate(stream.rate());
                    output.set_avg_frame_rate(stream.avg_frame_rate());
                }
                // SAFETY: av_dict_copy owns the destination allocation and leaves
                // the input's metadata unchanged.
                unsafe {
                    let status = ffmpeg::ffi::av_dict_copy(
                        &mut (*context.as_mut_ptr()).metadata,
                        source.metadata().as_ptr(),
                        0,
                    );
                    if status < 0 {
                        return Err(ffmpeg::Error::from(status));
                    }
                }
                let mut options = ffmpeg::Dictionary::new();
                if format == "mp4" {
                    // Fragmenting bounds sample-table memory for long recordings.
                    options.set("movflags", "frag_keyframe+empty_moov+default_base_moof");
                    options.set("frag_duration", "1000000");
                    options.set("write_tmcd", "0");
                } else {
                    // Avoid accumulating a whole-file cue index in memory.
                    options.set("live", "1");
                    options.set("cluster_time_limit", "1000");
                }
                context.write_header_with(options)?;
                let time_base = context
                    .stream(0)
                    .ok_or(ffmpeg::Error::StreamNotFound)?
                    .time_base();
                Ok(Self {
                    context,
                    time_base,
                    relative: relative.clone(),
                })
            })();
            match attempt {
                Ok(muxer) => return Ok(Some(muxer)),
                Err(error) => {
                    std::fs::remove_file(&absolute)
                        .map_err(|error| crate::error::io_error(&absolute, error))?;
                    failures.push(format!("{format}: {error}"));
                }
            }
        }
        warnings.push(format!(
            "Stream {} ({}) has no compatible copy muxer ({}); original packets are preserved.",
            stream.index(),
            parameters.id().name(),
            failures.join(", ")
        ));
        Ok(None)
    }

    fn check_io(&mut self) -> std::result::Result<(), ffmpeg::Error> {
        // SAFETY: the output owns a writable AVIOContext. Explicit flushing is
        // required because its destructor cannot return final write failures.
        unsafe {
            let io = (*self.context.as_mut_ptr()).pb;
            ffmpeg::ffi::avio_flush(io);
            if (*io).error < 0 {
                Err(ffmpeg::Error::from((*io).error))
            } else {
                Ok(())
            }
        }
    }

    fn discard(self, output_dir: &Path) -> Result<()> {
        let absolute = output_dir.join(&self.relative);
        drop(self);
        std::fs::remove_file(&absolute).map_err(|error| crate::error::io_error(&absolute, error))
    }
}

struct OutputFile {
    relative: PathBuf,
    absolute: PathBuf,
    writer: BufWriter<File>,
    offset: u64,
}

impl OutputFile {
    fn new(output_dir: &Path, relative: PathBuf) -> Result<Self> {
        let absolute = output_dir.join(&relative);
        let writer = BufWriter::new(create_file(&absolute)?);
        Ok(Self {
            relative,
            absolute,
            writer,
            offset: 0,
        })
    }

    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .map_err(|error| crate::error::io_error(&self.absolute, error))?;
        self.offset = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::InvalidMedia("extracted stream offset overflow".into()))?;
        Ok(())
    }

    fn json_line(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.writer, value).map_err(|error| {
            Error::Media(format!("writing {}: {error}", self.absolute.display()))
        })?;
        self.append(b"\n")
    }

    fn finish(&mut self) -> Result<()> {
        self.writer
            .flush()
            .map_err(|error| crate::error::io_error(&self.absolute, error))
    }
}

fn create_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| crate::error::io_error(path, error))
}

fn write_side_data<'a>(
    entries: impl ExactSizeIterator<Item = ffmpeg::codec::packet::SideData<'a>>,
    output: &mut OutputFile,
) -> Result<Vec<Value>> {
    if entries.len() > MAX_SIDE_DATA {
        return Err(Error::InvalidMedia(
            "too many stream or packet side-data entries".into(),
        ));
    }
    entries
        .map(|entry| {
            // SAFETY: the iterator borrows a live FFmpeg stream/packet. Bounds and
            // nullness are checked before reading the side-data allocation.
            unsafe {
                let entry = &*entry.as_ptr();
                let bytes = checked_bytes(entry.data, entry.size, MAX_PACKET_BYTES)?;
                let offset = output.offset;
                output.append(bytes)?;
                let name = ffmpeg::ffi::av_packet_side_data_name(entry.type_);
                Ok(json!({ "type": entry.type_ as i32,
                "name": if name.is_null() { None } else { CStr::from_ptr(name).to_str().ok() },
                "offset": offset, "size": bytes.len(), "file": output.relative }))
            }
        })
        .collect()
}

/// The caller must supply an allocation borrowed from a live FFmpeg object.
unsafe fn checked_bytes<'a>(pointer: *const u8, size: usize, maximum: usize) -> Result<&'a [u8]> {
    if size > maximum || (size != 0 && pointer.is_null()) {
        return Err(Error::InvalidMedia(
            "invalid or oversized FFmpeg byte buffer".into(),
        ));
    }
    if size == 0 {
        Ok(&[])
    } else {
        // SAFETY: the caller guarantees the allocation's lifetime and FFmpeg
        // supplies its length. Nullness and a conservative size bound are checked.
        Ok(unsafe { std::slice::from_raw_parts(pointer, size) })
    }
}

fn metadata(dictionary: ffmpeg::DictionaryRef<'_>, remaining: &mut usize) -> Result<Value> {
    let mut entries = Vec::new();
    let mut cursor = std::ptr::null_mut();
    loop {
        // SAFETY: the dictionary owns terminated strings; av_dict_get returns a
        // borrowed entry. Avoid the wrapper, which assumes metadata is UTF-8.
        unsafe {
            cursor = ffmpeg::ffi::av_dict_get(
                dictionary.as_ptr(),
                c"".as_ptr(),
                cursor,
                ffmpeg::ffi::AV_DICT_IGNORE_SUFFIX,
            );
            if cursor.is_null() {
                break;
            }
            if entries.len() >= MAX_METADATA_ENTRIES {
                return Err(Error::InvalidMedia(
                    "too many media metadata entries".into(),
                ));
            }
            let mut read_string = |pointer: *const std::ffi::c_char| -> Result<Value> {
                if pointer.is_null() {
                    return Err(Error::InvalidMedia("null media metadata string".into()));
                }
                let mut length = 0;
                while length < *remaining && *pointer.add(length) != 0 {
                    length += 1;
                }
                if length == *remaining {
                    return Err(Error::InvalidMedia(
                        "media metadata exceeds extraction size limit".into(),
                    ));
                }
                *remaining -= length + 1;
                let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), length);
                Ok(match std::str::from_utf8(bytes) {
                    Ok(text) => json!({"text": text}),
                    Err(_) => json!({"bytes_hex": hex(bytes)}),
                })
            };
            entries.push(
                json!({"key": read_string((*cursor).key)?, "value": read_string((*cursor).value)?}),
            );
        }
    }
    Ok(Value::Array(entries))
}

fn channel_layout(layout: &ffmpeg::ffi::AVChannelLayout) -> Result<String> {
    let mut buffer = [0_u8; 4096];
    if !(0..=1024).contains(&layout.nb_channels) {
        return Err(Error::InvalidMedia("invalid audio channel count".into()));
    }
    // SAFETY: the layout belongs to live codec parameters and FFmpeg receives
    // the exact writable buffer size. It returns the required string length.
    let length = unsafe {
        ffmpeg::ffi::av_channel_layout_describe(layout, buffer.as_mut_ptr().cast(), buffer.len())
    };
    if length < 0 || length as usize >= buffer.len() {
        return Err(Error::InvalidMedia(
            "invalid or oversized audio channel layout".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&buffer[..length as usize]).into_owned())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

fn medium_name(medium: ffmpeg::media::Type) -> &'static str {
    match medium {
        ffmpeg::media::Type::Video => "video",
        ffmpeg::media::Type::Audio => "audio",
        ffmpeg::media::Type::Subtitle => "subtitle",
        ffmpeg::media::Type::Data => "data",
        ffmpeg::media::Type::Attachment => "attachment",
        _ => "unknown",
    }
}

fn rational(value: ffmpeg::Rational) -> Value {
    json!({"numerator": value.numerator(), "denominator": value.denominator()})
}

fn timestamp(value: i64) -> Option<i64> {
    (value != ffmpeg::ffi::AV_NOPTS_VALUE).then_some(value)
}

fn media_error(operation: &str, error: ffmpeg::Error) -> Error {
    Error::Media(format!("{operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg::codec::packet::Mut;

    #[test]
    fn preserves_packet_bytes_timestamps_unknown_flags_and_side_data() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        std::fs::create_dir_all(base.join("streams/000")).unwrap();
        let payload = OutputFile::new(base, "streams/000/packets.bin".into()).unwrap();
        let packets = OutputFile::new(base, "streams/000/packets.jsonl".into()).unwrap();
        let side_data = OutputFile::new(base, "streams/000/side_data.bin".into()).unwrap();
        let mut output = StreamOutput {
            description: json!({}),
            index: 0,
            time_base: ffmpeg::Rational(1, 48_000),
            files: vec![
                payload.relative.clone(),
                packets.relative.clone(),
                side_data.relative.clone(),
            ],
            payload,
            packets,
            side_data,
            packet_count: 0,
            corrupt: false,
            muxer: None,
        };
        let mut first = ffmpeg::Packet::copy(&[0, 0xff, 1, 2]);
        first.set_pts(None);
        first.set_dts(Some(-1024));
        first.set_duration(1024);
        first.set_position(777);
        // SAFETY: the packet is exclusively borrowed and owns both allocations.
        unsafe {
            (*first.as_mut_ptr()).flags = 0x4001;
            let data = ffmpeg::ffi::av_packet_new_side_data(
                first.as_mut_ptr(),
                ffmpeg::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
                3,
            );
            assert!(!data.is_null());
            std::ptr::copy_nonoverlapping([9_u8, 8, 7].as_ptr(), data, 3);
        }
        let mut warnings = Vec::new();
        output
            .write_packet(&mut first, 7, base, &mut warnings)
            .unwrap();
        let mut second = ffmpeg::Packet::copy(&[3, 4]);
        second.set_pts(Some(0));
        second.set_dts(Some(0));
        output
            .write_packet(&mut second, 9, base, &mut warnings)
            .unwrap();
        let (description, files) = output.finish(base, &mut warnings).unwrap();
        assert!(warnings.is_empty());
        assert!(files
            .iter()
            .all(|path| path.is_relative() && base.join(path).is_file()));
        assert_eq!(
            std::fs::read(base.join("streams/000/packets.bin")).unwrap(),
            [0, 0xff, 1, 2, 3, 4]
        );
        assert_eq!(
            std::fs::read(base.join("streams/000/side_data.bin")).unwrap(),
            [9, 8, 7]
        );
        let lines: Vec<Value> = std::fs::read_to_string(base.join("streams/000/packets.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["sequence"], 7);
        assert_eq!(lines[0]["pts"], Value::Null);
        assert_eq!(lines[0]["dts"], -1024);
        assert_eq!(lines[0]["duration"], 1024);
        assert_eq!(lines[0]["source_position"], 777);
        assert_eq!(lines[0]["flags"], 0x4001);
        assert_eq!(
            lines[0]["time_base"],
            json!({"numerator": 1, "denominator": 48_000})
        );
        assert_eq!(lines[0]["side_data"][0]["offset"], 0);
        assert_eq!(lines[0]["side_data"][0]["size"], 3);
        assert_eq!(lines[1]["offset"], 4);
        assert_eq!(description["packet_count"], 2);
        assert_eq!(description["packet_bytes"], 6);
    }

    #[test]
    fn preserves_non_utf8_metadata_and_enforces_shared_budget() {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: both byte arrays contain a terminating NUL. av_dict_set copies
        // them, and Dictionary owns and frees the resulting dictionary.
        let dictionary = unsafe {
            assert_eq!(
                ffmpeg::ffi::av_dict_set(
                    &mut pointer,
                    c"title".as_ptr(),
                    [0xff_u8, 0].as_ptr().cast(),
                    0
                ),
                0
            );
            ffmpeg::Dictionary::own(pointer)
        };
        let borrowed = || unsafe { ffmpeg::DictionaryRef::wrap(dictionary.as_ptr()) };
        let result = metadata(borrowed(), &mut 8).unwrap();
        assert_eq!(
            result,
            json!([{"key": {"text": "title"}, "value": {"bytes_hex": "ff"}}])
        );
        assert!(metadata(borrowed(), &mut 7).is_err());
    }

    #[test]
    fn rejects_invalid_buffers_without_dereferencing_them() {
        // SAFETY: the invalid lengths/pointers must be rejected before a read.
        unsafe {
            assert!(checked_bytes(std::ptr::null(), 1, MAX_PACKET_BYTES).is_err());
            assert!(
                checked_bytes(std::ptr::null(), MAX_PACKET_BYTES + 1, MAX_PACKET_BYTES).is_err()
            );
            assert!(checked_bytes(std::ptr::null(), 0, MAX_PACKET_BYTES)
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn extracts_pcm_audio_to_playable_track_with_identical_samples() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("audio.insv");
        let samples = [0_u8, 0, 1, 0, 0xff, 0x7f, 0, 0x80].repeat(100);
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36_u32 + samples.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8000_u32.to_le_bytes());
        wav.extend_from_slice(&16000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        wav.extend_from_slice(&samples);
        std::fs::write(&source, wav).unwrap();
        let target = directory.path().join("output");
        std::fs::create_dir(&target).unwrap();
        let result = extract_streams(&source, &target).unwrap();
        assert_eq!(result.item_count, 1);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(
            std::fs::read(target.join("streams/000/packets.bin")).unwrap(),
            samples
        );
        let media = target.join(
            result.description["streams"][0]["media_file"]
                .as_str()
                .unwrap(),
        );
        let mut remuxed = ffmpeg::format::input(&media).unwrap();
        let mut remuxed_bytes = Vec::new();
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut remuxed) {
                Ok(()) => remuxed_bytes.extend_from_slice(packet.data().unwrap_or_default()),
                Err(ffmpeg::Error::Eof) => break,
                Err(error) => panic!("reading extracted track: {error}"),
            }
        }
        assert_eq!(remuxed_bytes, samples);
    }

    #[test]
    fn preserves_subtitle_attachment_and_chapter_metadata() {
        ffmpeg::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mkv");
        let attachment = b"opaque font attachment";
        {
            let mut context = ffmpeg::format::output(&source).unwrap();
            let mut subtitle = context.add_stream(None::<ffmpeg::Codec>).unwrap();
            let mut parameters = ffmpeg::codec::Parameters::new();
            parameters.set_medium(ffmpeg::media::Type::Subtitle);
            parameters.set_id(ffmpeg::codec::Id::SUBRIP);
            subtitle.set_parameters(parameters);
            subtitle.set_time_base((1, 1000));
            let mut font = context.add_stream(None::<ffmpeg::Codec>).unwrap();
            let mut parameters = ffmpeg::codec::Parameters::new();
            parameters.set_medium(ffmpeg::media::Type::Attachment);
            parameters.set_id(ffmpeg::codec::Id::TTF);
            // SAFETY: Parameters owns extradata and frees it with av_free. Its
            // required trailing padding is allocated and zero initialized here.
            unsafe {
                let parameter = &mut *parameters.as_mut_ptr();
                parameter.extradata = ffmpeg::ffi::av_mallocz(
                    attachment.len() + ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
                )
                .cast();
                assert!(!parameter.extradata.is_null());
                std::ptr::copy_nonoverlapping(
                    attachment.as_ptr(),
                    parameter.extradata,
                    attachment.len(),
                );
                parameter.extradata_size = attachment.len() as i32;
            }
            font.set_parameters(parameters);
            let mut tags = ffmpeg::Dictionary::new();
            tags.set("filename", "../../untrusted.ttf");
            tags.set("mimetype", "application/x-truetype-font");
            font.set_metadata(tags);
            context
                .add_chapter(7, (1, 1000), 0, 1000, "Opening")
                .unwrap();
            context.write_header().unwrap();
            let mut packet = ffmpeg::Packet::copy(b"Extract this subtitle");
            packet.set_stream(0);
            packet.set_pts(Some(0));
            packet.set_dts(Some(0));
            packet.set_duration(1000);
            packet.write_interleaved(&mut context).unwrap();
            context.write_trailer().unwrap();
        }
        let target = directory.path().join("output");
        std::fs::create_dir(&target).unwrap();
        let extracted = extract_streams(&source, &target).unwrap();
        assert_eq!(extracted.item_count, 2);
        assert!(extracted.warnings.is_empty(), "{:?}", extracted.warnings);
        assert_eq!(extracted.description["streams"][0]["type"], "subtitle");
        assert_eq!(extracted.description["streams"][1]["type"], "attachment");
        assert_eq!(
            std::fs::read(target.join("streams/000/packets.bin")).unwrap(),
            b"Extract this subtitle"
        );
        assert_eq!(
            std::fs::read(target.join("streams/001/extradata.bin")).unwrap(),
            attachment
        );
        assert_eq!(
            extracted.description["chapters"][0]["tags"][0]["value"]["text"],
            "Opening"
        );
        assert!(extracted.description["streams"][1]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag["value"]["text"] == "../../untrusted.ttf"));
        assert!(!directory.path().join("untrusted.ttf").exists());
        assert!(extracted
            .files
            .iter()
            .all(|path| path.is_relative() && target.join(path).is_file()));
    }

    #[test]
    fn unsupported_playable_copy_preserves_raw_video_with_warning() {
        ffmpeg::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.nut");
        let pixels = [255_u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        {
            let mut context = ffmpeg::format::output(&source).unwrap();
            let mut video = context.add_stream(None::<ffmpeg::Codec>).unwrap();
            let mut parameters = ffmpeg::codec::Parameters::new();
            parameters.set_medium(ffmpeg::media::Type::Video);
            parameters.set_id(ffmpeg::codec::Id::RAWVIDEO);
            // SAFETY: parameters exclusively owns this live codec parameter object.
            unsafe {
                let parameter = &mut *parameters.as_mut_ptr();
                parameter.width = 2;
                parameter.height = 2;
                parameter.format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_RGB24 as i32;
            }
            video.set_parameters(parameters);
            video.set_time_base((1, 25));
            context.write_header().unwrap();
            let mut packet = ffmpeg::Packet::copy(&pixels);
            packet.set_stream(0);
            packet.set_pts(Some(0));
            packet.set_dts(Some(0));
            packet.set_duration(1);
            packet.set_flags(ffmpeg::codec::packet::Flags::KEY);
            packet.write_interleaved(&mut context).unwrap();
            context.write_trailer().unwrap();
        }
        let target = directory.path().join("output");
        std::fs::create_dir(&target).unwrap();
        let extracted = extract_streams(&source, &target).unwrap();
        assert_eq!(extracted.item_count, 1);
        assert!(extracted
            .warnings
            .iter()
            .any(|warning| warning.contains("no compatible copy muxer")));
        assert!(extracted.description["streams"][0]
            .get("media_file")
            .is_none());
        assert_eq!(
            std::fs::read(target.join("streams/000/packets.bin")).unwrap(),
            pixels
        );
        assert!(!target.join("streams/000/media.mkv").exists());
    }
}
