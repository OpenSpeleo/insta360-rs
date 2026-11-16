//! Recording-wide metadata beside continuous stream copies, retaining original records.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::archive::ArtifactReferences;
use super::{ComponentExtraction, ProgressTracker};
use crate::container::{InsvMetadata, VideoPtsMapType};
use crate::error::io_error;
use crate::{Error, RecordingSequence, Result};

struct Artifact {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl Artifact {
    fn new(root: &Path, relative: &str) -> Result<Self> {
        let path = root.join(relative);
        Ok(Self {
            writer: BufWriter::new(File::create(&path).map_err(|error| io_error(&path, error))?),
            path,
        })
    }

    fn line(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.writer, value)
            .map_err(|error| Error::Media(format!("writing unified metadata: {error}")))?;
        self.writer
            .write_all(b"\n")
            .map_err(|error| io_error(&self.path, error))
    }

    fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .map_err(|error| io_error(&self.path, error))
    }
}

pub(super) struct SequenceMetadata {
    root: PathBuf,
    records: Artifact,
    motion: Artifact,
    exposure: Artifact,
    record_count: usize,
    motion_count: u64,
    exposure_count: u64,
    settings: Vec<Value>,
    common: Option<Map<String, Value>>,
    warnings: Vec<String>,
}

impl SequenceMetadata {
    pub(super) fn new(output_dir: &Path) -> Result<Self> {
        let directory = output_dir.join("metadata/metadata");
        fs::create_dir_all(&directory).map_err(|error| io_error(&directory, error))?;
        Ok(Self {
            root: output_dir.to_owned(),
            records: Artifact::new(output_dir, "metadata/extra-info/records.jsonl")?,
            motion: Artifact::new(output_dir, "metadata/metadata/motion.jsonl")?,
            exposure: Artifact::new(output_dir, "metadata/metadata/exposure.jsonl")?,
            record_count: 0,
            motion_count: 0,
            exposure_count: 0,
            settings: Vec::new(),
            common: None,
            warnings: Vec::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn append(
        &mut self,
        chapter_index: usize,
        input_index: usize,
        timeline_start: Duration,
        duration: Duration,
        source_root: &Path,
        references: &ArtifactReferences,
        container: &Value,
        media: &Value,
        metadata: &InsvMetadata,
        progress: &mut ProgressTracker<'_>,
    ) -> Result<()> {
        progress.check()?;
        let parsed = serde_json::to_value(metadata)
            .map_err(|error| Error::Media(format!("serializing recording metadata: {error}")))?;
        if let Some(fields) = parsed.as_object() {
            match &mut self.common {
                Some(common) => common.retain(|key, value| fields.get(key) == Some(value)),
                None => self.common = Some(fields.clone()),
            }
        }
        let mut decoded = Vec::new();
        let empty = Vec::new();
        let records = container["records"].as_array().unwrap_or(&empty);
        let explicit_time_map = records
            .iter()
            .any(|record| matches!(record["id"].as_u64(), Some(6 | 128)));
        for (record_index, record) in records.iter().enumerate() {
            progress.check()?;
            if let Some(value) = record.pointer("/decoded/metadata") {
                decoded.push(json!({"record_index": record_index, "metadata": value}));
            }
            let Some(relative) = record["raw_path"].as_str() else {
                continue;
            };
            let path = source_root.join(relative);
            let size = fs::metadata(&path)
                .map_err(|error| io_error(&path, error))?
                .len();
            let mut record = record.clone();
            references.rewrite(&mut record);
            self.records.line(&json!({
                "chapter_index": chapter_index, "input_index": input_index,
                "record_index": record_index, "record": record,
            }))?;
            self.record_count += 1;
            let Some(id @ (3 | 4 | 12)) = record["id"].as_u64() else {
                if record["region"].as_str() == Some("gyro") {
                    self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: opaque gyro envelope is consolidated in metadata/extra-info/payload.bin; sensor samples and video-clock mapping are unavailable."));
                }
                continue;
            };
            if record["format"].as_u64() != Some(0) {
                self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: unsupported telemetry format; complete record remains in metadata/extra-info/payload.bin."));
                continue;
            }
            self.telemetry(
                &path,
                size,
                chapter_index,
                input_index,
                record_index,
                id,
                timeline_start,
                metadata,
                explicit_time_map,
                progress,
            )?;
        }
        let mut settings = json!({
            "chapter_index": chapter_index, "input_index": input_index,
            "timeline_start_ns": timeline_start.as_nanos().to_string(),
            "duration_ns": duration.as_nanos().to_string(), "metadata": parsed,
            "decoded_records": decoded, "media": media,
        });
        references.rewrite(&mut settings);
        self.settings.push(settings);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn telemetry(
        &mut self,
        path: &Path,
        size: u64,
        chapter_index: usize,
        input_index: usize,
        record_index: usize,
        id: u64,
        timeline_start: Duration,
        metadata: &InsvMetadata,
        explicit_time_map: bool,
        progress: &mut ProgressTracker<'_>,
    ) -> Result<()> {
        let Some(raw) = metadata.is_raw_gyro else {
            self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: telemetry clock/format is unknown; complete record remains in metadata/extra-info/payload.bin."));
            return Ok(());
        };
        let sample_size = if id == 3 {
            if raw {
                20
            } else {
                56
            }
        } else {
            16
        };
        if !size.is_multiple_of(sample_size as u64) {
            self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: truncated telemetry samples; complete record remains in metadata/extra-info/payload.bin."));
            return Ok(());
        }
        let multiplier = if raw { 1_000_i128 } else { 1_000_000_i128 };
        let mapping = !explicit_time_map
            && metadata.first_frame_timestamp.is_some()
            && metadata.video_pts_map_type == Some(VideoPtsMapType::DecoderWithFirstFrameTimestamp)
            && !metadata.timelapse_interval.is_some_and(|value| value > 0.0)
            && !metadata
                .timelapse_interval_ms
                .is_some_and(|value| value > 0);
        let adjustment = metadata.gyro_timestamp_adjust_ms.unwrap_or(0.0) * 1_000_000.0;
        let adjustment = if id != 3 {
            Some(0_i128)
        } else if adjustment.is_finite()
            && adjustment >= i64::MIN as f64
            && adjustment < i64::MAX as f64
        {
            Some(adjustment.round() as i128)
        } else {
            None
        };
        let mapping = mapping && adjustment.is_some();
        if !mapping {
            self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: video-clock mapping is unavailable or unsupported; unified telemetry retains camera timestamps with null timeline_timestamp_ns."));
        }
        let mut normalized = metadata.clone();
        normalized.gyro_timestamp_adjust_ms = Some(0.0);
        normalized.video_pts_map_type = Some(VideoPtsMapType::DecoderWithFirstFrameTimestamp);
        let mut reader = BufReader::new(File::open(path).map_err(|error| io_error(path, error))?);
        let mut sample = [0_u8; 56];
        let mut decode_warning = false;
        let mut previous_timestamp = None;
        let mut ordering_warning = false;
        for sample_index in 0..size / sample_size as u64 {
            progress.check()?;
            reader
                .read_exact(&mut sample[..sample_size])
                .map_err(|error| io_error(path, error))?;
            let timestamp =
                i64::from_le_bytes(sample[..8].try_into().expect("eight-byte timestamp"));
            let camera_ns = i128::from(timestamp) * multiplier;
            if previous_timestamp.is_some_and(|previous| timestamp <= previous) && !ordering_warning
            {
                self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: telemetry timestamps are not strictly increasing; original sample order is retained."));
                ordering_warning = true;
            }
            previous_timestamp = Some(timestamp);
            let timeline_ns = if mapping {
                Some(
                    (timeline_start.as_nanos() as i128 + camera_ns
                        - i128::from(metadata.first_frame_timestamp.expect("validated anchor"))
                            * multiplier
                        - adjustment.expect("validated adjustment"))
                    .to_string(),
                )
            } else {
                None
            };
            let mut row = json!({
                "chapter_index": chapter_index, "input_index": input_index,
                "record_index": record_index, "sample_index": sample_index,
                "source_timestamp": timestamp.to_string(),
                "source_timestamp_unit": if raw { "microseconds" } else { "milliseconds" },
                "camera_timestamp_ns": camera_ns.to_string(), "timeline_timestamp_ns": timeline_ns,
            });
            let result = if id == 3 {
                normalized.first_frame_timestamp = Some(timestamp);
                crate::motion::decode_motion_record(&sample[..sample_size], &normalized).map(
                    |samples| {
                        if let Some(value) = samples.first() {
                            row["acceleration_g"] = json!(value.acceleration);
                            row["angular_velocity_rad_s"] = json!(value.angular_velocity);
                        }
                    },
                )
            } else {
                crate::telemetry::decode_camera_exposure_record(&sample[..sample_size], metadata)
                    .map(|samples| {
                        if let Some(value) = samples.first() {
                            row["shutter_ns"] = json!(value.shutter_speed.as_nanos().to_string());
                        }
                    })
            };
            if let Err(error) = result {
                row["decode_error"] = json!(error.to_string());
                if !decode_warning {
                    self.warnings.push(format!("Chapter {chapter_index} input {input_index} record {record_index}: some telemetry values could not be decoded; raw payload and sample errors are preserved."));
                    decode_warning = true;
                }
            }
            if id == 3 {
                self.motion.line(&row)?;
                self.motion_count += 1;
            } else {
                row["exposure_record_id"] = json!(id);
                self.exposure.line(&row)?;
                self.exposure_count += 1;
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        sequence: &RecordingSequence,
        continuous: &Value,
    ) -> Result<ComponentExtraction> {
        self.records.flush()?;
        self.motion.flush()?;
        self.exposure.flush()?;
        let mut common = self.common.unwrap_or_default();
        for key in [
            "first_frame_timestamp",
            "recording_group",
            "file_split_type",
            "gyro_sample_count",
            "exposure_sample_count",
        ] {
            common.remove(key);
        }
        for settings in &mut self.settings {
            if let Some(fields) = settings["metadata"].as_object_mut() {
                fields.retain(|key, _| !common.contains_key(key));
            }
        }
        let description = json!({
            "schema_version": 2,
            "timeline": {"duration_ns": sequence.duration.as_nanos().to_string(), "complete": sequence.complete,
                "origin": "start of continuous camera videos", "timestamp_unit": "nanoseconds"},
            "common": common, "settings": self.settings,
            "records": {"index": "metadata/extra-info/records.jsonl", "count": self.record_count},
            "telemetry": {
                "motion": {"path": "metadata/metadata/motion.jsonl", "samples": self.motion_count},
                "exposure": {"path": "metadata/metadata/exposure.jsonl", "samples": self.exposure_count},
                "ordering": "source chapter, input, record and sample order; overlapping and paired observations retain provenance",
                "unmapped_time": "null timeline_timestamp_ns means the camera clock cannot be safely mapped to video time"
            },
            "continuous_streams": continuous, "warnings": self.warnings,
        });
        let mut output = Artifact::new(&self.root, "metadata/metadata/recording.json")?;
        output.line(&description)?;
        output.flush()?;
        Ok(ComponentExtraction {
            description: json!({"path": "metadata/metadata/recording.json", "records": self.record_count,
                "motion_samples": self.motion_count, "exposure_samples": self.exposure_count}),
            files: [
                "metadata/metadata/recording.json",
                "metadata/extra-info/records.jsonl",
                "metadata/metadata/motion.jsonl",
                "metadata/metadata/exposure.jsonl",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
            warnings: self.warnings,
            item_count: self.record_count,
        })
    }
}
