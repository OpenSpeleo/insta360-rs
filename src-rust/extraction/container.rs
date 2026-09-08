//! Lossless, bounded extraction of container headers and proprietary tail data.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::ComponentExtraction;
use crate::error::io_error;
use crate::{Error, Result};

const MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";
const GYRO_MAGIC: &[u8; 32] = b"9c792b1ac55c40418d36ffb0d1d16b58";
const MAX_BOXES: usize = 65_536;
const MAX_RECORDS: usize = 65_536;
const MAX_DECODE_SIZE: u64 = 8 * 1024 * 1024;
const COPY_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug)]
struct Record {
    id: u8,
    format: u8,
    offset: u64,
    size: u64,
    directory_index: Option<usize>,
}

#[derive(Debug)]
struct Tail {
    version: u32,
    start: u64,
    media_end: u64,
    valid: bool,
    framing: &'static str,
    records: Vec<Record>,
    directory: Option<(u64, u64)>,
    metadata: Option<(u64, u64)>,
    gyro: Option<(u64, u64)>,
}

struct Extractor<'a> {
    input: &'a Path,
    output: &'a Path,
    file: File,
    file_size: u64,
    files: Vec<PathBuf>,
    warnings: Vec<String>,
}

pub(super) fn extract_container(input: &Path, output_dir: &Path) -> Result<ComponentExtraction> {
    let file = File::open(input).map_err(|error| io_error(input, error))?;
    let file_size = file
        .metadata()
        .map_err(|error| io_error(input, error))?
        .len();
    let mut extractor = Extractor {
        input,
        output: output_dir,
        file,
        file_size,
        files: Vec::new(),
        warnings: Vec::new(),
    };
    let mut tail = extractor.detect_tail()?;
    let media_end = tail.as_ref().map_or(file_size, |tail| tail.media_end);
    let v3_start = tail
        .as_ref()
        .filter(|tail| tail.version == 3)
        .map(|tail| tail.start);
    let (boxes, top_level_inst) = extractor.extract_boxes(media_end, v3_start)?;
    if let Some(tail) = &mut tail {
        if tail.version == 3 && tail.valid && !top_level_inst.contains(&tail.start) {
            tail.valid = false;
            tail.records.clear();
            tail.directory = None;
            extractor.warnings.push(
                "ExtraInfo V3 candidate does not begin at a top-level inst box; only raw bytes were preserved".into(),
            );
        }
    }
    let mut records = Vec::new();
    let mut trailer = Value::Null;
    if let Some(tail) = tail {
        let raw_path = PathBuf::from("extra-info/tail.bin");
        if !extractor.files.contains(&raw_path) {
            extractor.copy_range(tail.start, file_size - tail.start, &raw_path)?;
        }
        trailer = json!({
            "version": tail.version,
            "framing": tail.framing,
            "valid": tail.valid,
            "offset": tail.start,
            "size": file_size - tail.start,
            "raw_path": raw_path,
        });
        if let Some((offset, size)) = tail.directory {
            let path = PathBuf::from("extra-info/directory.bin");
            extractor.copy_range(offset, size, &path)?;
            trailer["directory"] = json!({"offset": offset, "size": size, "raw_path": path});
        }
        for (index, record) in tail.records.iter().enumerate() {
            records.push(extractor.extract_record(index, record)?);
        }
        if let Some((offset, size)) = tail.metadata {
            let path = PathBuf::from("extra-info/metadata-v2.bin");
            extractor.copy_range(offset, size, &path)?;
            let decoded = extractor.extract_json(offset, size, "metadata/v2.json")?;
            records.push(json!({
                "region": "metadata", "encoding": "json", "offset": offset,
                "size": size, "raw_path": path, "decoded": decoded,
            }));
        }
        if let Some((offset, size)) = tail.gyro {
            let path = PathBuf::from("extra-info/gyro-v2.pb");
            extractor.copy_range(offset, size, &path)?;
            let decoded = match extractor.extract_v2_gyro(offset, size) {
                Ok(decoded) => decoded,
                Err(Error::InvalidMedia(message)) => {
                    extractor
                        .warnings
                        .push(format!("V2 gyro envelope remains opaque: {message}"));
                    Value::Null
                }
                Err(error) => return Err(error),
            };
            records.push(json!({
                "region": "gyro", "encoding": "protobuf_envelope", "offset": offset,
                "size": size, "raw_path": path, "decoded": decoded,
            }));
        }
    } else {
        extractor.warnings.push(
            "No recognized ExtraInfo terminal signature; container and media extraction remain available".into(),
        );
    }
    Ok(ComponentExtraction {
        item_count: records.len(),
        description: json!({"file_size": file_size, "boxes": boxes, "trailer": trailer, "records": records}),
        files: extractor.files,
        warnings: extractor.warnings,
    })
}

impl Extractor<'_> {
    fn read(&mut self, offset: u64, size: usize) -> Result<Vec<u8>> {
        if offset > self.file_size || size as u64 > self.file_size - offset {
            return Err(invalid("read exceeds the input file"));
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| io_error(self.input, error))?;
        let mut bytes = vec![0; size];
        self.file
            .read_exact(&mut bytes)
            .map_err(|error| io_error(self.input, error))?;
        Ok(bytes)
    }

    fn destination(&self, relative: &Path) -> Result<File> {
        let path = self.output.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| io_error(path, error))
    }

    fn copy_range(&mut self, offset: u64, size: u64, relative: &Path) -> Result<()> {
        if offset > self.file_size || size > self.file_size - offset {
            return Err(invalid("copy range exceeds the input file"));
        }
        let mut destination = self.destination(relative)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| io_error(self.input, error))?;
        let mut remaining = size;
        let mut buffer = [0; COPY_BUFFER_SIZE];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            self.file
                .read_exact(&mut buffer[..count])
                .map_err(|error| io_error(self.input, error))?;
            destination
                .write_all(&buffer[..count])
                .map_err(|error| io_error(self.output.join(relative), error))?;
            remaining -= count as u64;
        }
        self.files.push(relative.to_path_buf());
        Ok(())
    }

    fn write_bytes(&mut self, relative: &Path, bytes: &[u8]) -> Result<()> {
        let mut destination = self.destination(relative)?;
        destination
            .write_all(bytes)
            .map_err(|error| io_error(self.output.join(relative), error))?;
        self.files.push(relative.to_path_buf());
        Ok(())
    }

    fn write_json(&mut self, relative: &Path, value: &Value) -> Result<()> {
        let mut destination = self.destination(relative)?;
        serde_json::to_writer_pretty(&mut destination, value).map_err(|error| {
            Error::Media(format!(
                "could not write {}: {error}",
                self.output.join(relative).display()
            ))
        })?;
        destination
            .write_all(b"\n")
            .map_err(|error| io_error(self.output.join(relative), error))?;
        self.files.push(relative.to_path_buf());
        Ok(())
    }

    fn detect_tail(&mut self) -> Result<Option<Tail>> {
        if self.file_size < 40 {
            return Ok(None);
        }
        let footer = self.read(self.file_size - 40, 40)?;
        if &footer[8..] != MAGIC {
            return Ok(None);
        }
        let version = le_u32(&footer[4..8]);
        let mut tail = Tail {
            version,
            start: self.file_size - 40,
            media_end: self.file_size,
            valid: false,
            framing: "unsupported",
            records: Vec::new(),
            directory: None,
            metadata: None,
            gyro: None,
        };
        let result = match version {
            2 => self.parse_v2(&mut tail, u64::from(le_u32(&footer[..4]))),
            3 => self.parse_v3(&mut tail, u64::from(le_u32(&footer[..4]))),
            _ => {
                self.warnings.push(format!(
                    "Unsupported ExtraInfo version {version}; raw terminal bytes were preserved"
                ));
                return Ok(Some(tail));
            }
        };
        match result {
            Ok(()) => tail.valid = true,
            Err(Error::InvalidMedia(message)) => {
                tail.records.clear();
                tail.directory = None;
                self.warnings.push(format!(
                    "Damaged ExtraInfo V{version}: {message}; available raw bytes were preserved"
                ));
            }
            Err(error) => return Err(error),
        }
        Ok(Some(tail))
    }

    fn parse_v2(&mut self, tail: &mut Tail, metadata_size: u64) -> Result<()> {
        tail.framing = "v2";
        let metadata_start = (self.file_size - 40)
            .checked_sub(metadata_size)
            .ok_or_else(|| invalid("V2 metadata length exceeds the file"))?;
        tail.start = metadata_start;
        tail.media_end = metadata_start;
        tail.metadata = Some((metadata_start, metadata_size));
        if metadata_start >= 44 {
            let gyro_header = self.read(metadata_start - 44, 44)?;
            if &gyro_header[12..] == GYRO_MAGIC {
                // Preserve a recognized damaged header as part of the tail too.
                tail.start = metadata_start - 44;
                tail.media_end = tail.start;
                let gyro_size = u64::from(le_u32(&gyro_header[..4]));
                let gyro_start = tail.start.checked_sub(gyro_size).ok_or_else(|| {
                    invalid("V2 gyro length exceeds the bytes preceding its header")
                })?;
                tail.start = gyro_start;
                tail.media_end = gyro_start;
                tail.gyro = Some((gyro_start, gyro_size));
            }
        }
        Ok(())
    }

    fn parse_v3(&mut self, tail: &mut Tail, extra_size: u64) -> Result<()> {
        tail.framing = "v3";
        if self.file_size < 72 || extra_size < 72 {
            return Err(invalid(
                "V3 terminal header or ExtraInfo size is shorter than 72 bytes",
            ));
        }
        let total = extra_size
            .checked_add(8)
            .ok_or_else(|| invalid("V3 size overflow"))?;
        let start = self
            .file_size
            .checked_sub(total)
            .ok_or_else(|| invalid("V3 ExtraInfo size exceeds the file"))?;
        tail.start = start;
        let header = self.read(start, 8)?;
        if total > u64::from(u32::MAX)
            || &header[4..8] != b"inst"
            || u64::from(be_u32(&header[..4])) != total
        {
            return Err(invalid(
                "V3 ExtraInfo does not match the enclosing ordinary-size inst header",
            ));
        }
        let payload_start = start + 8;
        let terminal_start = self.file_size - 72;
        if terminal_start == payload_start {
            tail.framing = "v3_empty";
            return Ok(());
        }
        if terminal_start - payload_start < 6 {
            return Err(invalid("partial V3 record footer"));
        }
        let final_footer = self.read(terminal_start - 6, 6)?;
        if final_footer[0] == 0 && final_footer[1] == 0 {
            tail.framing = "v3_indexed";
            self.parse_directory(tail, payload_start, terminal_start, &final_footer)?;
        } else {
            tail.framing = "v3_sequential";
            let mut cursor = terminal_start;
            while cursor > payload_start {
                if tail.records.len() == MAX_RECORDS {
                    return Err(invalid(
                        "V3 sequential record count exceeds the extraction limit",
                    ));
                }
                let footer_start = cursor
                    .checked_sub(6)
                    .filter(|value| *value >= payload_start)
                    .ok_or_else(|| invalid("partial V3 sequential footer or leftover prefix"))?;
                let footer = self.read(footer_start, 6)?;
                let size = u64::from(le_u32(&footer[2..6]));
                let offset = footer_start
                    .checked_sub(size)
                    .filter(|value| *value >= payload_start)
                    .ok_or_else(|| invalid("V3 sequential record exceeds its region"))?;
                tail.records.push(Record {
                    id: footer[1],
                    format: footer[0],
                    offset,
                    size,
                    directory_index: None,
                });
                cursor = offset;
            }
            tail.records.reverse();
        }
        Ok(())
    }

    fn parse_directory(
        &mut self,
        tail: &mut Tail,
        payload_start: u64,
        terminal_start: u64,
        footer: &[u8],
    ) -> Result<()> {
        let directory_size = u64::from(le_u32(&footer[2..6]));
        if !directory_size.is_multiple_of(10) || directory_size / 10 > MAX_RECORDS as u64 {
            return Err(invalid(
                "V3 directory length is not a multiple of ten or exceeds the record limit",
            ));
        }
        let directory_start = (terminal_start - 6)
            .checked_sub(directory_size)
            .filter(|value| *value >= payload_start)
            .ok_or_else(|| invalid("V3 directory exceeds the record region"))?;
        for index in 0..(directory_size / 10) as usize {
            let entry = self.read(directory_start + index as u64 * 10, 10)?;
            // Indexed tails may reserve empty slots for absent record IDs.
            // These ten zero bytes describe no payload and have no footer.
            if entry.iter().all(|byte| *byte == 0) {
                continue;
            }
            if entry[0] == 0 {
                return Err(invalid(
                    "nonempty V3 directory slot uses reserved record ID zero",
                ));
            }
            let size = u64::from(le_u32(&entry[2..6]));
            let offset = payload_start
                .checked_add(u64::from(le_u32(&entry[6..10])))
                .ok_or_else(|| invalid("V3 record offset overflow"))?;
            let end = offset
                .checked_add(size)
                .and_then(|value| value.checked_add(6))
                .filter(|value| *value <= directory_start)
                .ok_or_else(|| {
                    invalid("V3 indexed record overlaps the directory or exceeds its region")
                })?;
            let record_footer = self.read(end - 6, 6)?;
            if record_footer[0] != entry[1]
                || record_footer[1] != entry[0]
                || le_u32(&record_footer[2..6]) != le_u32(&entry[2..6])
            {
                return Err(invalid(
                    "V3 record footer does not match its directory entry",
                ));
            }
            tail.records.push(Record {
                id: entry[0],
                format: entry[1],
                offset,
                size,
                directory_index: Some(index),
            });
        }
        tail.records.sort_by_key(|record| record.offset);
        for pair in tail.records.windows(2) {
            if pair[0].offset + pair[0].size + 6 > pair[1].offset {
                return Err(invalid(
                    "V3 directory contains overlapping or duplicate record ranges",
                ));
            }
        }
        tail.directory = Some((directory_start, directory_size));
        Ok(())
    }

    fn extract_boxes(&mut self, end: u64, v3_start: Option<u64>) -> Result<(Vec<Value>, Vec<u64>)> {
        let mut boxes = Vec::new();
        let mut inst_offsets = Vec::new();
        let mut cursor = 0;
        while cursor < end {
            if end - cursor < 8 || boxes.len() == MAX_BOXES {
                self.warnings.push(format!("Container scan stopped at byte {cursor}: partial box header or box count limit"));
                break;
            }
            let header = self.read(cursor, 8)?;
            let kind: [u8; 4] = header[4..8].try_into().expect("four-byte box type");
            let size_word = be_u32(&header[..4]);
            let (size, header_size) = match size_word {
                0 => (end - cursor, 8),
                1 if end - cursor >= 16 => {
                    let extended = self.read(cursor + 8, 8)?;
                    (
                        u64::from_be_bytes(extended.try_into().expect("eight-byte extended size")),
                        16,
                    )
                }
                1 => {
                    self.warnings
                        .push(format!("Truncated extended box header at byte {cursor}"));
                    break;
                }
                size => (u64::from(size), 8),
            };
            if size < header_size || size > end - cursor {
                self.warnings.push(format!(
                    "Invalid box size at byte {cursor}; unparsed bytes were preserved"
                ));
                break;
            }
            let name = safe_box_name(kind);
            // The original inst box is kept here even when its tail is damaged.
            let relative = if kind == *b"inst"
                && v3_start == Some(cursor)
                && size == self.file_size - cursor
            {
                // One artifact serves both the complete box and the raw tail.
                PathBuf::from("extra-info/tail.bin")
            } else if kind == *b"mdat" {
                PathBuf::from(format!("container/{:04}-{name}.header.bin", boxes.len()))
            } else {
                PathBuf::from(format!("container/{:04}-{name}.box", boxes.len()))
            };
            self.copy_range(
                cursor,
                if kind == *b"mdat" { header_size } else { size },
                &relative,
            )?;
            if kind == *b"inst" {
                inst_offsets.push(cursor);
            }
            boxes.push(json!({
                "type": String::from_utf8_lossy(&kind), "type_hex": hex(&kind),
                "offset": cursor, "size": size, "header_size": header_size,
                "raw_path": relative, "payload_preserved": kind != *b"mdat",
            }));
            cursor += size;
        }
        if cursor < end {
            self.copy_range(cursor, end - cursor, Path::new("container/unparsed.bin"))?;
        }
        Ok((boxes, inst_offsets))
    }

    fn extract_record(&mut self, index: usize, record: &Record) -> Result<Value> {
        let signature = self.read(record.offset, record.size.min(16) as usize)?;
        let extension = image_extension(&signature).unwrap_or(match (record.id, record.format) {
            (1, 1) => "pb",
            _ => "bin",
        });
        let raw_path = PathBuf::from(format!(
            "extra-info/records/{index:04}-id{:02x}-format{:02x}.{extension}",
            record.id, record.format
        ));
        self.copy_range(record.offset, record.size, &raw_path)?;
        let mut description = json!({
            "id": record.id, "format": record.format, "offset": record.offset, "size": record.size,
            "physical_index": index, "directory_index": record.directory_index, "raw_path": raw_path,
            "encoding": "opaque",
        });
        match (record.id, record.format) {
            (1, 1) if record.size <= MAX_DECODE_SIZE => {
                let bytes = self.read(record.offset, record.size as usize)?;
                match crate::container::parse_metadata(&bytes) {
                    Ok(metadata) => {
                        let path = PathBuf::from(format!("metadata/record-{index:04}.json"));
                        let parsed = serde_json::to_value(&metadata)
                            .map_err(|error| Error::Media(format!("could not serialize INSV metadata: {error}")))?;
                        self.write_json(&path, &parsed)?;
                        description["encoding"] = json!("protobuf_metadata");
                        description["decoded"] = json!({"path": path, "metadata": parsed});
                        let mut calibration = Vec::new();
                        for (offset_index, offset) in metadata.offsets.iter().enumerate() {
                            let form = if offset.original { "original" } else { "current" };
                            let path = PathBuf::from(format!("calibration/record-{index:04}-offset-{offset_index:03}-v{}-{form}.txt", offset.version));
                            self.write_bytes(&path, offset.value.as_bytes())?;
                            calibration.push(json!({"version": offset.version, "original": offset.original, "path": path}));
                        }
                        for (profile_index, profile) in metadata.profiles.iter().enumerate() {
                            let path = PathBuf::from(format!("calibration/record-{index:04}-profile-{profile_index:03}.pb"));
                            self.write_bytes(&path, &profile.payload)?;
                            calibration.push(json!({"name": profile.name, "path": path}));
                        }
                        description["calibration"] = json!(calibration);
                    }
                    Err(error) => self.warnings.push(format!("Metadata record {index} remains opaque: {error}")),
                }
            }
            (1, 2) => {
                description["encoding"] = json!("json_metadata");
                description["decoded"] = self.extract_json(record.offset, record.size, &format!("metadata/record-{index:04}.json"))?;
            }
            (1, 1) => self.warnings.push(format!("Metadata record {index} exceeds the {MAX_DECODE_SIZE}-byte decode limit; raw payload preserved")),
            (1, format) => self.warnings.push(format!("Metadata record {index} uses unsupported format {format}; raw payload preserved")),
            _ => {}
        }
        Ok(description)
    }

    fn extract_json(&mut self, offset: u64, size: u64, relative: &str) -> Result<Value> {
        if size > MAX_DECODE_SIZE {
            self.warnings.push(format!("JSON metadata at byte {offset} exceeds the {MAX_DECODE_SIZE}-byte decode limit; raw payload preserved"));
            return Ok(Value::Null);
        }
        let bytes = self.read(offset, size as usize)?;
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => {
                self.write_json(Path::new(relative), &value)?;
                Ok(json!({"path": relative, "metadata": value}))
            }
            Err(error) => {
                self.warnings.push(format!("JSON metadata at byte {offset} could not be decoded: {error}; raw payload preserved"));
                Ok(Value::Null)
            }
        }
    }

    fn extract_v2_gyro(&mut self, offset: u64, size: u64) -> Result<Value> {
        let fields = self.protobuf_fields(offset, size)?;
        let mut envelopes = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            if field.number != 1 || field.wire != 2 {
                continue;
            }
            let nested = self.protobuf_fields(field.offset, field.size)?;
            let mut decoded =
                json!({"fields": nested.iter().map(WireField::description).collect::<Vec<_>>()});
            let mut samples = Vec::new();
            for (sample_index, field) in nested.iter().enumerate() {
                match (field.number, field.wire) {
                    (1, 0) => decoded["time_offset"] = json!(field.scalar as i64),
                    (2, 0) => decoded["application_state"] = json!(field.scalar != 0),
                    (3, 0) => decoded["declared_sample_count"] = json!(field.scalar as i32),
                    (4, 0) => decoded["camera_family"] = json!(field.scalar),
                    (7, 2) => {
                        let path = PathBuf::from(format!(
                            "extra-info/gyro-v2-envelope-{index:03}-samples-{sample_index:03}.bin"
                        ));
                        self.copy_range(field.offset, field.size, &path)?;
                        samples.push(json!({"offset": field.offset, "size": field.size, "raw_path": path, "encoding": "opaque"}));
                    }
                    _ => {}
                }
            }
            decoded["samples"] = json!(samples);
            envelopes.push(decoded);
        }
        let value = json!({"fields": fields.iter().map(WireField::description).collect::<Vec<_>>(), "envelopes": envelopes});
        let path = Path::new("metadata/gyro-v2.json");
        self.write_json(path, &value)?;
        Ok(json!({"path": path, "envelope": value}))
    }

    fn protobuf_fields(&mut self, offset: u64, size: u64) -> Result<Vec<WireField>> {
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= self.file_size)
            .ok_or_else(|| invalid("protobuf message exceeds the file"))?;
        let mut cursor = offset;
        let mut fields = Vec::new();
        while cursor < end {
            if fields.len() == MAX_RECORDS {
                return Err(invalid("protobuf field count exceeds the extraction limit"));
            }
            let key = self.varint(&mut cursor, end)?;
            if key >> 3 == 0 || key >> 3 > (1 << 29) - 1 {
                return Err(invalid("invalid protobuf field number"));
            }
            let wire = (key & 7) as u8;
            let mut field = WireField {
                number: (key >> 3) as u32,
                wire,
                offset: cursor,
                size: 0,
                scalar: 0,
            };
            match wire {
                0 => field.scalar = self.varint(&mut cursor, end)?,
                1 | 5 => {
                    field.size = if wire == 1 { 8 } else { 4 };
                    if field.size > end - cursor {
                        return Err(invalid("truncated protobuf fixed-width field"));
                    }
                    let bytes = self.read(cursor, field.size as usize)?;
                    field.scalar = if wire == 1 {
                        u64::from_le_bytes(bytes.try_into().expect("eight bytes"))
                    } else {
                        u64::from(le_u32(&bytes))
                    };
                    cursor += field.size;
                }
                2 => {
                    field.size = self.varint(&mut cursor, end)?;
                    field.offset = cursor;
                    if field.size > end - cursor {
                        return Err(invalid("protobuf bytes exceed the enclosing message"));
                    }
                    cursor += field.size;
                }
                _ => return Err(invalid("unsupported protobuf wire type")),
            }
            fields.push(field);
        }
        Ok(fields)
    }

    fn varint(&mut self, cursor: &mut u64, end: u64) -> Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            if *cursor >= end {
                return Err(invalid("truncated protobuf varint"));
            }
            let byte = self.read(*cursor, 1)?[0];
            *cursor += 1;
            if shift == 63 && byte > 1 {
                return Err(invalid("protobuf varint overflows u64"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(invalid("protobuf varint exceeds ten bytes"))
    }
}

struct WireField {
    number: u32,
    wire: u8,
    offset: u64,
    size: u64,
    scalar: u64,
}

impl WireField {
    fn description(&self) -> Value {
        if self.wire == 2 {
            json!({"number": self.number, "wire_type": self.wire, "offset": self.offset, "size": self.size})
        } else {
            json!({"number": self.number, "wire_type": self.wire, "raw_value": self.scalar})
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::InvalidMedia(message.into())
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte little-endian integer"))
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("four-byte big-endian integer"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn safe_box_name(kind: [u8; 4]) -> String {
    if kind.iter().all(u8::is_ascii_alphanumeric) {
        String::from_utf8(kind.to_vec()).expect("ASCII box name")
    } else {
        hex(&kind)
    }
}

fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn prefix() -> Vec<u8> {
        let mut bytes = boxed(b"ftyp", b"isom\0\0\0\0isom");
        bytes.extend(boxed(b"mdat", &[0xab; 37]));
        bytes.extend(boxed(b"zZ99", b"private box"));
        bytes
    }

    fn v3(records: &[(u8, u8, &[u8])], directory_order: Option<&[usize]>) -> Vec<u8> {
        let mut payload = Vec::new();
        let mut entries = Vec::new();
        for (id, format, bytes) in records {
            let mut entry = vec![*id, *format];
            entry.extend((bytes.len() as u32).to_le_bytes());
            entry.extend((payload.len() as u32).to_le_bytes());
            entries.push(entry);
            payload.extend_from_slice(bytes);
            payload.extend([*format, *id]);
            payload.extend((bytes.len() as u32).to_le_bytes());
        }
        if let Some(order) = directory_order {
            for index in order {
                payload.extend(&entries[*index]);
            }
            payload.extend([0, 0]);
            payload.extend(((order.len() * 10) as u32).to_le_bytes());
        }
        let extra_size = (payload.len() + 72) as u32;
        payload.extend([0x55; 32]);
        payload.extend(extra_size.to_le_bytes());
        payload.extend(3_u32.to_le_bytes());
        payload.extend(MAGIC);
        let mut bytes = prefix();
        bytes.extend(boxed(b"inst", &payload));
        bytes
    }

    fn v2(metadata: &[u8], gyro: Option<&[u8]>) -> Vec<u8> {
        let mut bytes = prefix();
        if let Some(gyro) = gyro {
            bytes.extend_from_slice(gyro);
            bytes.extend((gyro.len() as u32).to_le_bytes());
            bytes.extend([0x66; 8]);
            bytes.extend(GYRO_MAGIC);
        }
        bytes.extend_from_slice(metadata);
        bytes.extend((metadata.len() as u32).to_le_bytes());
        bytes.extend(2_u32.to_le_bytes());
        bytes.extend(MAGIC);
        bytes
    }

    fn extract(bytes: &[u8]) -> (TempDir, ComponentExtraction) {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input.insv");
        let output = directory.path().join("output");
        fs::write(&input, bytes).unwrap();
        fs::create_dir(&output).unwrap();
        let result = extract_container(&input, &output).unwrap();
        for path in &result.files {
            assert!(path.is_relative());
            assert!(output.join(path).is_file(), "{}", path.display());
        }
        (directory, result)
    }

    fn artifact(directory: &TempDir, description: &Value, key: &str) -> Vec<u8> {
        fs::read(
            directory
                .path()
                .join("output")
                .join(description[key].as_str().unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn indexed_tail_preserves_unknown_repeated_records_and_original_directory_order() {
        let png = b"\x89PNG\r\n\x1a\nopaque image bytes";
        let bytes = v3(
            &[(0xfe, 8, b"first"), (2, 77, png), (0xfe, 9, b"second")],
            Some(&[2, 0, 1]),
        );
        let (directory, result) = extract(&bytes);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(result.item_count, 3);
        assert_eq!(result.description["trailer"]["framing"], "v3_indexed");
        let records = result.description["records"].as_array().unwrap();
        assert_eq!(records[0]["directory_index"], 1);
        assert_eq!(records[1]["directory_index"], 2);
        assert_eq!(records[2]["directory_index"], 0);
        assert_eq!(artifact(&directory, &records[0], "raw_path"), b"first");
        assert_eq!(artifact(&directory, &records[2], "raw_path"), b"second");
        assert_eq!(artifact(&directory, &records[1], "raw_path"), png);
        assert!(records[1]["raw_path"].as_str().unwrap().ends_with(".png"));
        assert_eq!(records[1]["encoding"], "opaque");
        assert_eq!(
            artifact(&directory, &result.description["trailer"], "raw_path"),
            &bytes[prefix().len()..]
        );
        let boxes = result.description["boxes"].as_array().unwrap();
        assert_eq!(
            artifact(&directory, &boxes[2], "raw_path"),
            boxed(b"zZ99", b"private box")
        );
        assert_eq!(artifact(&directory, &boxes[1], "raw_path").len(), 8);
        assert_eq!(boxes[1]["payload_preserved"], false);
    }

    #[test]
    fn sequential_tail_decodes_each_supported_metadata_record_and_preserves_unknown_formats() {
        let protobuf = [0x12, 2, b'X', b'5', 0x2a, 3, b'2', b'_', b'2'];
        let bytes = v3(
            &[
                (1, 1, &protobuf),
                (1, 2, br#"{"legacy":"camera"}"#),
                (1, 99, &protobuf),
            ],
            None,
        );
        let (directory, result) = extract(&bytes);
        assert_eq!(result.description["trailer"]["framing"], "v3_sequential");
        assert_eq!(result.item_count, 3);
        let records = result.description["records"].as_array().unwrap();
        assert_eq!(records[0]["decoded"]["metadata"]["camera_name"], "X5");
        assert_eq!(records[1]["decoded"]["metadata"]["legacy"], "camera");
        assert!(records[2].get("decoded").is_none());
        let calibration = &records[0]["calibration"][0];
        assert_eq!(artifact(&directory, calibration, "path"), b"2_2");
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("unsupported format 99"));
    }

    #[test]
    fn genuine_v2_json_and_gyro_wrapper_are_extracted_without_v3_assumptions() {
        let nested = [8, 42, 16, 1, 24, 3, 32, 2, 58, 3, 0xaa, 0xbb, 0xcc];
        let mut wrapper = vec![10, nested.len() as u8];
        wrapper.extend(nested);
        let bytes = v2(br#"{"camera":"Nano","opaque":[1,2]}"#, Some(&wrapper));
        let (directory, result) = extract(&bytes);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(result.item_count, 2);
        assert_eq!(result.description["trailer"]["version"], 2);
        assert_eq!(result.description["trailer"]["valid"], true);
        assert_eq!(result.description["boxes"].as_array().unwrap().len(), 3);
        let records = &result.description["records"];
        assert_eq!(records[0]["decoded"]["metadata"]["camera"], "Nano");
        assert_eq!(artifact(&directory, &records[1], "raw_path"), wrapper);
        let envelope = &records[1]["decoded"]["envelope"]["envelopes"][0];
        assert_eq!(envelope["time_offset"], 42);
        assert_eq!(envelope["declared_sample_count"], 3);
        assert_eq!(
            artifact(&directory, &envelope["samples"][0], "raw_path"),
            [0xaa, 0xbb, 0xcc]
        );
        assert_eq!(
            artifact(&directory, &result.description["trailer"], "raw_path"),
            &bytes[prefix().len()..]
        );
    }

    #[test]
    fn v2_without_gyro_and_malformed_json_keep_original_payloads() {
        for metadata in [br#"{"valid":true}"#.as_slice(), b"not JSON"] {
            let bytes = v2(metadata, None);
            let (directory, result) = extract(&bytes);
            assert_eq!(result.item_count, 1);
            assert_eq!(result.description["trailer"]["valid"], true);
            assert_eq!(
                artifact(&directory, &result.description["records"][0], "raw_path"),
                metadata
            );
            assert_eq!(result.warnings.is_empty(), metadata.starts_with(b"{"));
        }
    }

    #[test]
    fn overlapping_duplicate_directory_ranges_are_rejected_but_raw_tail_survives() {
        let bytes = v3(&[(0x7f, 2, b"private")], Some(&[0, 0]));
        let (directory, result) = extract(&bytes);
        assert_eq!(result.description["trailer"]["valid"], false);
        assert_eq!(result.item_count, 0);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("duplicate")));
        assert_eq!(
            artifact(&directory, &result.description["trailer"], "raw_path"),
            &bytes[prefix().len()..]
        );
    }

    #[test]
    fn indexed_record_cannot_overlap_directory_even_with_matching_footer() {
        let mut bytes = v3(&[(8, 8, b"")], Some(&[0]));
        let directory_start = prefix().len() + 8 + 6;
        // Make the entry point at its own first bytes. A matching footer lives
        // inside the directory, but that cannot turn directory bytes into data.
        bytes[directory_start..directory_start + 2].copy_from_slice(&[8, 8]);
        bytes[directory_start + 2..directory_start + 6].copy_from_slice(&0_u32.to_le_bytes());
        bytes[directory_start + 6..directory_start + 10].copy_from_slice(&6_u32.to_le_bytes());
        let (_, result) = extract(&bytes);
        assert_eq!(result.item_count, 0);
        assert_eq!(result.description["trailer"]["valid"], false);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("overlaps the directory")));
    }

    #[test]
    fn directory_marker_requires_both_bytes_and_empty_tails_are_valid() {
        let (_, sequential) = extract(&v3(&[(0, 3, b"opaque control extension")], None));
        assert_eq!(
            sequential.description["trailer"]["framing"],
            "v3_sequential"
        );
        assert_eq!(sequential.item_count, 1);
        for bytes in [v3(&[], Some(&[])), v3(&[], None)] {
            let (_, result) = extract(&bytes);
            assert!(result.warnings.is_empty(), "{:?}", result.warnings);
            assert_eq!(result.description["trailer"]["valid"], true);
            assert_eq!(result.item_count, 0);
        }
    }

    #[test]
    fn wrong_version_never_enters_v3_parser() {
        let mut bytes = v3(&[(1, 1, &[0x12, 2, b'X', b'5'])], None);
        let version_offset = bytes.len() - 36;
        bytes[version_offset..version_offset + 4].copy_from_slice(&259_u32.to_le_bytes());
        let (_, result) = extract(&bytes);
        assert_eq!(result.description["trailer"]["version"], 259);
        assert_eq!(result.description["trailer"]["valid"], false);
        assert_eq!(result.item_count, 0);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("Unsupported ExtraInfo version 259")));
    }

    #[test]
    fn tail_candidate_inside_media_payload_is_not_a_valid_top_level_inst() {
        let tail = v3(&[(0x7f, 1, b"private")], None);
        let bytes = boxed(b"mdat", &tail[prefix().len()..]);
        let (directory, result) = extract(&bytes);
        assert_eq!(result.description["trailer"]["valid"], false);
        assert_eq!(result.item_count, 0);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("top-level inst")));
        assert_eq!(
            artifact(&directory, &result.description["trailer"], "raw_path"),
            &bytes[8..]
        );
    }

    #[test]
    fn impossible_terminal_lengths_remain_bounded_and_preserve_available_bytes() {
        for mut bytes in [v2(b"{}", None), v3(&[(1, 2, b"{}")], None)] {
            let length_offset = bytes.len() - 40;
            bytes[length_offset..length_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let (directory, result) = extract(&bytes);
            assert_eq!(result.description["trailer"]["valid"], false);
            assert_eq!(result.item_count, 0);
            assert!(!result.warnings.is_empty());
            assert_eq!(
                artifact(&directory, &result.description["trailer"], "raw_path"),
                &bytes[length_offset..]
            );
        }
    }

    #[test]
    fn sequential_prefix_garbage_is_rejected() {
        let mut bytes = v3(&[(0x70, 0x71, b"x")], None);
        let payload_start = prefix().len() + 8;
        // Insert one unexplained byte and repair outer sizes, preserving the
        // valid last record. The backward walk must consume the entire region.
        bytes.insert(payload_start, 0xff);
        let inst_size = (bytes.len() - prefix().len()) as u32;
        bytes[prefix().len()..prefix().len() + 4].copy_from_slice(&inst_size.to_be_bytes());
        let extra_offset = bytes.len() - 40;
        bytes[extra_offset..extra_offset + 4].copy_from_slice(&(inst_size - 8).to_le_bytes());
        let (_, result) = extract(&bytes);
        assert_eq!(result.description["trailer"]["valid"], false);
        assert_eq!(result.item_count, 0);
    }

    #[test]
    fn damaged_v2_gyro_length_does_not_become_an_empty_gyro_record() {
        let mut bytes = v2(b"{}", Some(b"opaque"));
        let gyro_header = prefix().len() + b"opaque".len();
        bytes[gyro_header..gyro_header + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let (_, result) = extract(&bytes);
        assert_eq!(result.description["trailer"]["valid"], false);
        assert_eq!(result.item_count, 1);
        assert_eq!(result.description["records"][0]["region"], "metadata");
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("V2 gyro length")));
    }

    #[test]
    fn incomplete_tail_and_unrecognized_box_types_are_preserved_without_path_injection() {
        let mut bytes = prefix();
        bytes.extend(boxed(b"../\\", b"unknown"));
        bytes.extend(b"broken!");
        let (directory, result) = extract(&bytes);
        let boxes = result.description["boxes"].as_array().unwrap();
        assert_eq!(boxes[3]["type_hex"], "2e2e2f5c");
        assert_eq!(
            artifact(&directory, &boxes[3], "raw_path"),
            boxed(b"../\\", b"unknown")
        );
        assert_eq!(
            fs::read(directory.path().join("output/container/unparsed.bin")).unwrap(),
            b"broken!"
        );
        assert!(result.description["trailer"].is_null());
    }

    #[test]
    fn extended_boxes_are_preserved_and_copy_spans_multiple_buffers() {
        let mut bytes = prefix();
        let payload = vec![0xad; COPY_BUFFER_SIZE * 2 + 7];
        let mut extended = 1_u32.to_be_bytes().to_vec();
        extended.extend(b"uuid");
        extended.extend(((payload.len() + 16) as u64).to_be_bytes());
        extended.extend(&payload);
        bytes.extend(&extended);
        let (directory, result) = extract(&bytes);
        assert_eq!(
            artifact(&directory, &result.description["boxes"][3], "raw_path"),
            extended
        );
        assert_eq!(result.description["boxes"][3]["header_size"], 16);
    }
}
