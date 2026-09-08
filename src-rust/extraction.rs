//! Lossless encoded-stream and metadata extraction without decoding or stitching.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::io_error;
use crate::{Error, InputSet, Result};

mod container;
mod streams;

/// Completed extraction. Files are published only after the entire operation succeeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionReport {
    /// Absolute path to the completed output directory.
    pub output_dir: PathBuf,
    /// Absolute path to the JSON manifest describing all inputs and artifacts.
    pub manifest_path: PathBuf,
    /// Number of original files processed, including explicitly paired lenses.
    pub input_count: usize,
    /// Number of demuxed streams, including audio and data streams.
    pub stream_count: usize,
    /// Number of individually extracted ExtraInfo records.
    pub record_count: usize,
    /// Absolute paths to all generated files, including the manifest.
    pub files: Vec<PathBuf>,
    /// Unsupported interpretations or playable copies; raw artifacts remain available.
    pub warnings: Vec<String>,
}

pub(super) struct ComponentExtraction {
    pub description: Value,
    pub files: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub item_count: usize,
}

/// Extracts every encoded stream and available metadata into a target folder.
///
/// Video and audio are copied without decoding, stitching, or changing their
/// bit depth. Every stream also has raw packets and a timing index, including
/// streams that cannot be remuxed into a playable file. Container metadata and
/// ExtraInfo records are preserved independently of their typed interpretation.
///
/// The target must be absent or an empty, non-symlink directory. Work is staged
/// in an owned sibling directory; failure removes only that staging directory.
/// Each input gets its own `input-00`, `input-01` directory. Paths within the
/// manifest's component descriptions are relative to that input directory.
/// This synchronous operation requires the `media` feature and no encoder,
/// camera model, calibration, stabilization, or GPU configuration.
pub fn extract(inputs: &InputSet, output_dir: impl AsRef<Path>) -> Result<ExtractionReport> {
    let output = prepare_output(output_dir.as_ref())?;
    let staging = StagingDirectory::create(&output)?;
    let mut descriptions = Vec::with_capacity(inputs.paths().len());
    let mut relative_files = Vec::new();
    let mut warnings = Vec::new();
    let mut stream_count = 0;
    let mut record_count = 0;

    for (index, path) in inputs.paths().iter().enumerate() {
        let source = fs::canonicalize(path).map_err(|error| io_error(path, error))?;
        let before = fs::metadata(&source).map_err(|error| io_error(&source, error))?;
        let directory = PathBuf::from(format!("input-{index:02}"));
        let destination = staging.path.join(&directory);
        fs::create_dir(&destination).map_err(|error| io_error(&destination, error))?;

        let container = container::extract_container(&source, &destination)?;
        let media = streams::extract_streams(&source, &destination)?;
        let after = fs::metadata(&source).map_err(|error| io_error(&source, error))?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(Error::InvalidMedia(format!(
                "input changed during extraction: {}",
                source.display()
            )));
        }

        stream_count += media.item_count;
        record_count += container.item_count;
        for component in [&container, &media] {
            relative_files.extend(component.files.iter().map(|file| directory.join(file)));
            warnings.extend(
                component
                    .warnings
                    .iter()
                    .map(|warning| format!("input-{index:02}: {warning}")),
            );
        }
        descriptions.push(json!({
            "source": source,
            "size": before.len(),
            "directory": directory,
            "container": container.description,
            "media": media.description,
        }));
    }

    let manifest = json!({
        "schema_version": 1,
        "inputs": descriptions,
        "warnings": warnings,
        "preservation": {
            "encoded_packets": "original demuxed payloads with packet boundaries and timing",
            "playable_copies": "additional stream copies when a compatible muxer is available",
            "container": "original non-mdat boxes and ExtraInfo bytes",
            "scope": "component extraction; not a byte-for-byte backup of unused mdat space",
        },
    });
    let manifest_path = staging.path.join("manifest.json");
    let file = File::create(&manifest_path).map_err(|error| io_error(&manifest_path, error))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &manifest)
        .map_err(|error| Error::Media(format!("serializing extraction manifest: {error}")))?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|error| io_error(&manifest_path, error))?;
    drop(writer);
    relative_files.push(PathBuf::from("manifest.json"));
    relative_files.sort();
    staging.publish(&output)?;

    Ok(ExtractionReport {
        manifest_path: output.join("manifest.json"),
        files: relative_files
            .iter()
            .map(|file| output.join(file))
            .collect(),
        output_dir: output,
        input_count: inputs.paths().len(),
        stream_count,
        record_count,
        warnings,
    })
}

fn validate_empty_target(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::InvalidMedia(format!(
                    "extraction target must be an absent or empty directory: {}",
                    path.display()
                )));
            }
            if fs::read_dir(path)
                .map_err(|error| io_error(path, error))?
                .next()
                .transpose()
                .map_err(|error| io_error(path, error))?
                .is_some()
            {
                return Err(Error::InvalidMedia(format!(
                    "extraction target is not empty: {}",
                    path.display()
                )));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(path, error)),
    }
}

fn prepare_output(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::InvalidMedia("extraction target must name a directory".into()))?;
    validate_empty_target(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    Ok(fs::canonicalize(parent)
        .map_err(|error| io_error(parent, error))?
        .join(name))
}

struct StagingDirectory {
    path: PathBuf,
    published: bool,
}

impl StagingDirectory {
    fn create(output: &Path) -> Result<Self> {
        static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);
        let parent = output.parent().ok_or_else(|| {
            Error::InvalidMedia("extraction target has no parent directory".into())
        })?;
        for _ in 0..128 {
            let id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".insta360-extract-{}-{id}.part",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        published: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io_error(&path, error)),
            }
        }
        Err(Error::Media(
            "could not allocate an extraction staging directory".into(),
        ))
    }

    fn publish(mut self, output: &Path) -> Result<()> {
        if validate_empty_target(output)? {
            // remove_dir succeeds only while the target is still empty.
            fs::remove_dir(output).map_err(|error| io_error(output, error))?;
        }
        fs::rename(&self.path, output).map_err(|error| io_error(output, error))?;
        self.published = true;
        Ok(())
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn refuses_nonempty_targets_and_cleans_failed_attempts() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("invalid.insv");
        fs::write(&source, b"not a movie").expect("input");
        let inputs = InputSet::new(vec![source]).expect("inputs");
        let output = temp.path().join("output");
        fs::create_dir(&output).expect("output");
        let existing = output.join("keep.txt");
        fs::write(&existing, b"keep me").expect("existing file");
        assert!(extract(&inputs, &output).is_err());
        assert_eq!(fs::read(&existing).expect("existing remains"), b"keep me");
        fs::remove_file(&existing).expect("empty output");
        assert!(extract(&inputs, &output).is_err());
        assert!(output.is_dir());
        assert_eq!(fs::read_dir(&output).expect("output contents").count(), 0);
        assert_eq!(fs::read_dir(temp.path()).expect("no staging").count(), 2);
    }

    #[test]
    fn publishes_only_complete_staging_and_preserves_new_target_contents() {
        let temp = tempdir().expect("tempdir");
        let output = temp.path().join("output");
        let staging = StagingDirectory::create(&output).expect("staging");
        fs::write(staging.path.join("done"), b"complete").expect("artifact");
        assert!(!output.exists());
        staging.publish(&output).expect("publish");
        assert_eq!(
            fs::read(output.join("done")).expect("artifact"),
            b"complete"
        );
        let second = StagingDirectory::create(&output).expect("another staging");
        let second_path = second.path.clone();
        assert!(second.publish(&output).is_err());
        assert!(!second_path.exists());
        assert!(output.join("done").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_targets_including_dangling_links() {
        let temp = tempdir().expect("tempdir");
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("dangling link");
        assert!(prepare_output(&link).is_err());
        fs::create_dir(&real).expect("real");
        assert!(prepare_output(&link).is_err());
    }
}
