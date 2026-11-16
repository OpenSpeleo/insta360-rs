//! Recording-wide indexed bundles of original artifacts, without per-part folders.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{ComponentExtraction, ProgressTracker};
use crate::error::io_error;
use crate::{Error, Result};

const CATEGORIES: [&str; 5] = [
    "calibration",
    "container",
    "extra-info",
    "metadata",
    "streams",
];
const COPY_BUFFER_SIZE: usize = 64 * 1024;

struct Bundle {
    payload_path: String,
    index_path: String,
    payload: BufWriter<File>,
    index: BufWriter<File>,
    size: u64,
    count: usize,
}

/// Resolves original artifact paths to slices of the published category bundles.
pub(super) struct ArtifactReferences(BTreeMap<String, Value>);

impl ArtifactReferences {
    /// Replaces references in descriptions, preserving non-path metadata values.
    pub(super) fn rewrite(&self, value: &mut Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.rewrite(value);
                }
            }
            Value::Object(values) => {
                for (key, value) in values.iter_mut() {
                    // These subtrees contain source data, not extractor-owned
                    // references, even when a tag happens to look like a path.
                    if matches!(key.as_str(), "metadata" | "tags") {
                        continue;
                    }
                    if matches!(
                        key.as_str(),
                        "path"
                            | "raw_path"
                            | "packet_payload"
                            | "packet_index"
                            | "side_data_payload"
                            | "extradata"
                    ) {
                        if let Some(reference) = value.as_str().and_then(|path| self.0.get(path)) {
                            *value = reference.clone();
                            continue;
                        }
                    }
                    self.rewrite(value);
                }
            }
            _ => {}
        }
    }
}

/// Preserves each original file as an indexed byte range within its category.
pub(super) struct SequenceArchive {
    root: PathBuf,
    bundles: BTreeMap<&'static str, Bundle>,
}

impl SequenceArchive {
    /// Creates the complete category layout, including categories without artifacts.
    pub(super) fn new(root: &Path) -> Result<Self> {
        let mut bundles = BTreeMap::new();
        for category in CATEGORIES {
            let directory = PathBuf::from("metadata").join(category);
            let absolute = root.join(&directory);
            fs::create_dir_all(&absolute).map_err(|error| io_error(&absolute, error))?;
            // Forward slashes keep exported references portable when the bundle
            // moves between Windows and POSIX hosts.
            let payload_path = format!("metadata/{category}/payload.bin");
            let index_path = format!("metadata/{category}/artifacts.jsonl");
            let payload = create_file(&root.join(&payload_path))?;
            let index = create_file(&root.join(&index_path))?;
            bundles.insert(
                category,
                Bundle {
                    payload_path,
                    index_path,
                    payload: BufWriter::new(payload),
                    index: BufWriter::new(index),
                    size: 0,
                    count: 0,
                },
            );
        }
        Ok(Self {
            root: root.to_owned(),
            bundles,
        })
    }

    /// Copies bounded chunks and returns references relative to the export root.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn append(
        &mut self,
        source_root: &Path,
        chapter_index: usize,
        input_index: usize,
        timeline_start: Duration,
        duration: Duration,
        files: &[PathBuf],
        progress: &mut ProgressTracker<'_>,
    ) -> Result<ArtifactReferences> {
        progress.check()?;
        let mut references = BTreeMap::new();
        let mut buffer = [0_u8; COPY_BUFFER_SIZE];
        for relative in files {
            progress.check()?;
            let key = relative.to_str().ok_or_else(|| {
                Error::InvalidMedia("extracted artifact path is not UTF-8".into())
            })?;
            if references.contains_key(key) {
                continue;
            }
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(Error::InvalidMedia(
                    "extracted artifact path is not relative to its input".into(),
                ));
            }
            let category = relative.components().next().and_then(|component| {
                if let Component::Normal(name) = component {
                    name.to_str()
                } else {
                    None
                }
            });
            let bundle = category
                .and_then(|category| self.bundles.get_mut(category))
                .ok_or_else(|| Error::InvalidMedia("unknown extracted artifact category".into()))?;
            let source = source_root.join(relative);
            let file = File::open(&source).map_err(|error| io_error(&source, error))?;
            let mut reader = BufReader::new(file);
            let offset = bundle.size;
            loop {
                progress.check()?;
                let count = reader
                    .read(&mut buffer)
                    .map_err(|error| io_error(&source, error))?;
                if count == 0 {
                    break;
                }
                let size = bundle
                    .size
                    .checked_add(count as u64)
                    .ok_or_else(|| Error::InvalidMedia("artifact bundle size overflow".into()))?;
                bundle
                    .payload
                    .write_all(&buffer[..count])
                    .map_err(|error| io_error(self.root.join(&bundle.payload_path), error))?;
                bundle.size = size;
                progress.copied(count)?;
            }
            let reference = json!({
                "path": bundle.payload_path,
                "offset": offset,
                "size": bundle.size - offset,
            });
            serde_json::to_writer(
                &mut bundle.index,
                &json!({
                    "original_path": relative,
                    "chapter_index": chapter_index,
                    "input_index": input_index,
                    "timeline_start_ns": timeline_start.as_nanos().to_string(),
                    "duration_ns": duration.as_nanos().to_string(),
                    "slice": reference,
                }),
            )
            .map_err(|error| Error::Media(format!("writing artifact index: {error}")))?;
            bundle
                .index
                .write_all(b"\n")
                .map_err(|error| io_error(self.root.join(&bundle.index_path), error))?;
            bundle.count = bundle
                .count
                .checked_add(1)
                .ok_or_else(|| Error::InvalidMedia("artifact count overflow".into()))?;
            references.insert(key.to_owned(), reference);
        }
        Ok(ArtifactReferences(references))
    }

    /// Flushes every category before its containing staging directory is published.
    pub(super) fn finish(self) -> Result<ComponentExtraction> {
        let mut description = Map::new();
        let mut files = Vec::with_capacity(CATEGORIES.len() * 2);
        let mut count = 0_usize;
        for (category, mut bundle) in self.bundles {
            bundle
                .payload
                .flush()
                .map_err(|error| io_error(self.root.join(&bundle.payload_path), error))?;
            bundle
                .index
                .flush()
                .map_err(|error| io_error(self.root.join(&bundle.index_path), error))?;
            count = count
                .checked_add(bundle.count)
                .ok_or_else(|| Error::InvalidMedia("artifact count overflow".into()))?;
            description.insert(
                category.to_owned(),
                json!({
                    "payload": bundle.payload_path,
                    "index": bundle.index_path,
                    "artifacts": bundle.count,
                    "bytes": bundle.size,
                }),
            );
            files.push(PathBuf::from(bundle.payload_path));
            files.push(PathBuf::from(bundle.index_path));
        }
        Ok(ComponentExtraction {
            description: json!({
                "schema_version": 1,
                "format": "indexed original artifact byte ranges; payloads are not standalone source containers",
                "references": "slice paths are relative to the export root; offset and size are byte ranges in that payload",
                "original_json_paths": "resolve original paths inside archived JSON using chapter_index, input_index and original_path in the category indexes",
                "packet_offsets": "packet and side-data offsets are relative to their corresponding original artifact slice, not the category payload",
                "categories": description,
            }),
            files,
            warnings: Vec::new(),
            item_count: count,
        })
    }
}

fn create_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error(path, error))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use tempfile::tempdir;

    fn artifact(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("artifact directory")).expect("directory");
        fs::write(path, bytes).expect("artifact");
        PathBuf::from(name)
    }

    fn recover(root: &Path, reference: &Value) -> Vec<u8> {
        let bytes =
            fs::read(root.join(reference["path"].as_str().expect("path"))).expect("bundle payload");
        let offset = reference["offset"].as_u64().expect("offset") as usize;
        let size = reference["size"].as_u64().expect("size") as usize;
        bytes[offset..offset + size].to_vec()
    }

    #[test]
    fn combines_categories_and_inputs_without_losing_original_bytes_or_timing() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("output");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let first_packet = vec![0x5a; COPY_BUFFER_SIZE * 2 + 7];
        let first_files = vec![
            artifact(&first, "streams/000/packets.bin", &first_packet),
            artifact(&first, "calibration/profile.pb", b"original profile"),
            artifact(&first, "container/moov.box", b"original container"),
            artifact(&first, "extra-info/tail.bin", b"unknown proprietary bytes"),
            artifact(&first, "metadata/decoded.json", br#"{"field":42}"#),
        ];
        let second_files = vec![
            artifact(&second, "streams/000/packets.bin", b"next packets"),
            artifact(&second, "calibration/profile.pb", b"changed profile"),
        ];
        let cancelled = AtomicBool::new(false);
        let mut callback = |_| {};
        let mut progress = ProgressTracker::new(&cancelled, &mut callback, 2, 0);
        let mut archive = SequenceArchive::new(&root).expect("archive");
        let first_refs = archive
            .append(
                &first,
                0,
                0,
                Duration::ZERO,
                Duration::from_secs(3),
                &first_files,
                &mut progress,
            )
            .expect("first append");
        let second_refs = archive
            .append(
                &second,
                1,
                1,
                Duration::from_secs(3),
                Duration::from_secs(5),
                &second_files,
                &mut progress,
            )
            .expect("second append");
        let component = archive.finish().expect("finish");
        assert_eq!(component.item_count, 7);
        assert_eq!(component.files.len(), 10);
        for (source, files, references) in [
            (&first, &first_files, &first_refs),
            (&second, &second_files, &second_refs),
        ] {
            for file in files {
                let reference = &references.0[file.to_str().expect("relative path")];
                assert_eq!(
                    recover(&root, reference),
                    fs::read(source.join(file)).expect("original")
                );
            }
        }
        let rows = fs::read_to_string(root.join("metadata/streams/artifacts.jsonl"))
            .expect("stream index")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("index row"))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["chapter_index"], 1);
        assert_eq!(rows[1]["input_index"], 1);
        assert_eq!(rows[1]["timeline_start_ns"], "3000000000");
        assert_eq!(rows[1]["duration_ns"], "5000000000");
        assert_eq!(rows[1]["slice"]["offset"], first_packet.len());
        assert!(!root.join("parts").exists());
    }

    #[test]
    fn rewrites_nested_references_and_keeps_unrelated_values() {
        let references = ArtifactReferences(BTreeMap::from([(
            "streams/000/packets.bin".to_owned(),
            json!({"path": "metadata/streams/payload.bin", "offset": 12, "size": 5}),
        )]));
        let mut description = json!({
            "streams": [{"packet_payload": "streams/000/packets.bin", "pts": -3}],
            "unknown": "unchanged", "enabled": true, "missing": null,
            "name": "streams/000/packets.bin",
            "tags": {"path": "streams/000/packets.bin"},
            "decoded": {"metadata": {"raw_path": "streams/000/packets.bin"}},
        });
        references.rewrite(&mut description);
        assert_eq!(
            description["streams"][0]["packet_payload"],
            references.0["streams/000/packets.bin"]
        );
        assert_eq!(description["streams"][0]["pts"], -3);
        assert_eq!(description["unknown"], "unchanged");
        assert_eq!(description["enabled"], true);
        assert!(description["missing"].is_null());
        assert_eq!(description["name"], "streams/000/packets.bin");
        assert_eq!(description["tags"]["path"], "streams/000/packets.bin");
        assert_eq!(
            description["decoded"]["metadata"]["raw_path"],
            "streams/000/packets.bin"
        );
    }

    #[test]
    fn creates_empty_categories_refuses_overwrites_and_honors_cancellation() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("output");
        let mut archive = SequenceArchive::new(&root).expect("archive");
        assert!(SequenceArchive::new(&root).is_err());
        let cancelled = AtomicBool::new(true);
        let mut callback = |_| {};
        let mut progress = ProgressTracker::new(&cancelled, &mut callback, 1, 0);
        assert!(matches!(
            archive.append(
                temp.path(),
                0,
                0,
                Duration::ZERO,
                Duration::ZERO,
                &[],
                &mut progress
            ),
            Err(Error::Cancelled)
        ));
        let component = archive.finish().expect("finish");
        assert_eq!(component.item_count, 0);
        for category in CATEGORIES {
            assert!(root.join("metadata").join(category).is_dir());
        }
        for file in component.files {
            assert!(fs::read(root.join(file)).expect("empty bundle").is_empty());
        }
    }
}
