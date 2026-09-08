//! Bundled and application-supplied, integrity-checked assets for optional vendor-derived features.
//!
//! The core stitch path must not depend on these assets. In particular, per-recording
//! lens calibration remains authoritative. [`BundledAssetProvider`] supplies the licensed
//! Insta360 and Studio resources embedded through two data crates; applications can also supply
//! their own bundles. Asset availability does not imply a qualified inference pipeline.

mod bundled;

pub use bundled::{BundledAssetProvider, BUNDLED_MANIFEST};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Manifest schema understood by this crate.
pub const MODEL_BUNDLE_SCHEMA_VERSION: u32 = 2;

pub type AssetResult<T> = std::result::Result<T, AssetError>;

/// Controls whether an optional licensed asset is used and how absence is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetPolicy {
    /// The asset must be present and pass all integrity checks.
    Required,
    /// Use the asset when present; an absent asset cleanly disables the feature.
    Automatic,
    /// Do not ask the provider for the asset.
    Disabled,
}

/// Semantic role of a payload in a model bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    AiStitch,
    ColorLut,
    Defringe,
    Deflicker,
    ImageDenoise,
    RawDenoise,
    AccessoryDetectorSvm,
    CoolingShellDetectorSvm,
    CameraConfig,
    ModelCatalog,
    RegressionFixture,
    Other,
}

/// Whether an asset has passed the complete camera-specific qualification gate.
///
/// Imported vendor data defaults to [`Self::Unqualified`]. Possessing and parsing a
/// model is not evidence that its preprocessing, execution, and output have been
/// qualified for a camera/lens/platform combination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetQualification {
    /// The payload may be inspected or converted, but not selected as a runtime capability.
    #[default]
    Unqualified,
    /// The application has qualified the complete algorithm for the declared dimensions.
    Qualified,
}

/// Version bounds are opaque vendor version strings, not SemVer promises.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<String>,
}

/// Declares where an asset has been qualified for use.
///
/// Qualification is default-deny. Once explicitly qualified, empty dimension lists
/// are wildcards. Target triples are strings so manifests can describe platforms
/// newer than this crate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetCompatibility {
    /// Explicit capability gate. Missing fields deserialize as `Unqualified`.
    #[serde(default)]
    pub qualification: AssetQualification,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub camera_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lens_types: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_triples: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_firmware: Option<VersionBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sdk: Option<VersionBounds>,
}

impl AssetCompatibility {
    /// Checks the exact camera, lens, and Rust target dimensions of an asset request.
    ///
    /// Empty manifest lists are wildcards. Firmware and vendor-release bounds remain audit
    /// metadata because vendor version strings are explicitly opaque rather than a
    /// SemVer contract; applications must qualify those bounds for their release.
    pub fn matches_context(&self, context: AssetContext<'_>) -> bool {
        self.qualification == AssetQualification::Qualified
            && (self.camera_models.is_empty()
                || self
                    .camera_models
                    .iter()
                    .any(|camera| camera.eq_ignore_ascii_case(context.camera_model)))
            && (self.lens_types.is_empty() || self.lens_types.contains(&context.lens_type))
            && (self.target_triples.is_empty()
                || self
                    .target_triples
                    .iter()
                    .any(|target| target == context.target_triple))
    }
}

/// Exact runtime dimensions used to select a qualified optional asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetContext<'a> {
    /// Canonical camera family, for example `X5`.
    pub camera_model: &'a str,
    /// Encoded lens identifier resolved from the current recording offset.
    pub lens_type: u32,
    /// Rust target triple of the consuming application.
    pub target_triple: &'a str,
}

/// Audit information retained when a licensed payload is copied into a bundle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// SHA-256 of the exact source bytes before any importer normalization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    /// Optional version of the tool that produced the stored payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importer_version: Option<String>,
    /// Non-secret application audit identifier. This must never contain a license key or token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_reference: Option<String>,
    /// Canonicalization applied before the descriptor digest was calculated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalization: Option<String>,
}

/// One payload declared by a [`ModelBundle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetDescriptor {
    /// Stable logical identifier used by code, independent of the source filename.
    pub id: String,
    pub kind: AssetKind,
    /// Forward-slash-separated path relative to the provider root.
    pub path: String,
    /// Hexadecimal SHA-256 digest of the exact stored bytes.
    pub sha256: String,
    /// Exact stored length, checked before the digest.
    pub byte_length: u64,
    #[serde(default)]
    pub compatibility: AssetCompatibility,
    #[serde(default)]
    pub provenance: AssetProvenance,
}

/// A complete logical model assembled from multiple verified payload descriptors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetGroupDescriptor {
    /// Stable logical identifier for the complete model.
    pub id: String,
    /// Semantic role of the complete model.
    pub kind: AssetKind,
    /// Ordered member asset identifiers required to consume the model.
    pub members: Vec<String>,
    /// Qualification applies to the complete model, never to a lone constituent.
    #[serde(default)]
    pub compatibility: AssetCompatibility,
}

/// Versioned description of a bundled or application-supplied licensed asset collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBundle {
    pub schema_version: u32,
    pub bundle_id: String,
    /// Opaque application-controlled bundle revision.
    pub bundle_version: String,
    pub assets: Vec<AssetDescriptor>,
    /// Logical models whose constituent assets must all be present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<AssetGroupDescriptor>,
}

impl ModelBundle {
    /// Validate structure and all content identifiers before any payload is loaded.
    pub fn validate(&self) -> AssetResult<()> {
        if self.schema_version != MODEL_BUNDLE_SCHEMA_VERSION {
            return Err(AssetError::UnsupportedManifestVersion {
                found: self.schema_version,
                supported: MODEL_BUNDLE_SCHEMA_VERSION,
            });
        }
        if self.bundle_id.trim().is_empty() {
            return Err(AssetError::InvalidManifest(
                "bundle_id must not be empty".to_owned(),
            ));
        }
        if self.bundle_version.trim().is_empty() {
            return Err(AssetError::InvalidManifest(
                "bundle_version must not be empty".to_owned(),
            ));
        }

        let mut ids = HashSet::with_capacity(self.assets.len());
        let mut paths = HashSet::with_capacity(self.assets.len());
        for asset in &self.assets {
            if asset.id.trim().is_empty() {
                return Err(AssetError::InvalidManifest(
                    "asset id must not be empty".to_owned(),
                ));
            }
            if !ids.insert(asset.id.as_str()) {
                return Err(AssetError::DuplicateAssetId(asset.id.clone()));
            }
            validate_relative_path(&asset.path)?;
            if !paths.insert(asset.path.as_str()) {
                return Err(AssetError::DuplicateAssetPath(asset.path.clone()));
            }
            Sha256Digest::from_hex(&asset.sha256)?;
            if let Some(source_sha256) = &asset.provenance.source_sha256 {
                Sha256Digest::from_hex(source_sha256)?;
            }
            for (name, value) in [
                ("source_path", &asset.provenance.source_path),
                ("importer_version", &asset.provenance.importer_version),
                ("audit_reference", &asset.provenance.audit_reference),
                ("normalization", &asset.provenance.normalization),
            ] {
                if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
                    return Err(AssetError::InvalidManifest(format!(
                        "asset {:?} provenance {name} must not be empty when supplied",
                        asset.id
                    )));
                }
            }
        }

        let mut group_ids = HashSet::with_capacity(self.groups.len());
        for group in &self.groups {
            if group.id.trim().is_empty() {
                return Err(AssetError::InvalidManifest(
                    "asset group id must not be empty".to_owned(),
                ));
            }
            if !group_ids.insert(group.id.as_str()) {
                return Err(AssetError::DuplicateAssetGroupId(group.id.clone()));
            }
            if group.members.is_empty() {
                return Err(AssetError::InvalidManifest(format!(
                    "asset group {:?} must contain at least one member",
                    group.id
                )));
            }
            let mut members = HashSet::with_capacity(group.members.len());
            for member in &group.members {
                if !members.insert(member.as_str()) {
                    return Err(AssetError::DuplicateAssetGroupMember {
                        group: group.id.clone(),
                        member: member.clone(),
                    });
                }
                let Some(member_asset) = self.asset(member) else {
                    return Err(AssetError::MissingAssetGroupMember {
                        group: group.id.clone(),
                        member: member.clone(),
                    });
                };
                if member_asset.kind != group.kind {
                    return Err(AssetError::AssetGroupKindMismatch {
                        group: group.id.clone(),
                        member: member.clone(),
                        group_kind: group.kind,
                        member_kind: member_asset.kind,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn asset(&self, id: &str) -> Option<&AssetDescriptor> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    /// Looks up a complete logical model by its stable identifier.
    pub fn group(&self, id: &str) -> Option<&AssetGroupDescriptor> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// Load a declared payload according to `policy` and verify its size and digest.
    ///
    /// `Automatic` tolerates absence, not corruption. A present but altered asset is
    /// always an error, preventing a silent quality or provenance downgrade.
    pub fn load_verified(
        &self,
        provider: &dyn AssetProvider,
        id: &str,
        policy: AssetPolicy,
    ) -> AssetResult<Option<VerifiedAsset>> {
        self.validate()?;
        let descriptor = self
            .asset(id)
            .ok_or_else(|| AssetError::UndeclaredAsset(id.to_owned()))?;

        if policy == AssetPolicy::Disabled {
            return Ok(None);
        }

        let Some(bytes) = provider.load(&descriptor.path)? else {
            return match policy {
                AssetPolicy::Required => Err(AssetError::MissingRequiredAsset {
                    id: descriptor.id.clone(),
                    path: descriptor.path.clone(),
                }),
                AssetPolicy::Automatic => Ok(None),
                AssetPolicy::Disabled => unreachable!("handled above"),
            };
        };

        let actual_length = bytes.len() as u64;
        if actual_length != descriptor.byte_length {
            return Err(AssetError::LengthMismatch {
                id: descriptor.id.clone(),
                expected: descriptor.byte_length,
                actual: actual_length,
            });
        }

        let expected = Sha256Digest::from_hex(&descriptor.sha256)?;
        let actual = sha256(&bytes);
        if actual != expected {
            return Err(AssetError::DigestMismatch {
                id: descriptor.id.clone(),
                expected,
                actual,
            });
        }

        Ok(Some(VerifiedAsset {
            descriptor: descriptor.clone(),
            digest: actual,
            bytes,
        }))
    }

    /// Selects by qualified camera/lens/target dimensions, then loads and verifies bytes.
    ///
    /// `Automatic` treats an incompatible asset like an unavailable optional
    /// capability. `Required` reports the incompatibility without touching the provider.
    pub fn load_verified_for(
        &self,
        provider: &dyn AssetProvider,
        id: &str,
        policy: AssetPolicy,
        context: AssetContext<'_>,
    ) -> AssetResult<Option<VerifiedAsset>> {
        self.validate()?;
        let descriptor = self
            .asset(id)
            .ok_or_else(|| AssetError::UndeclaredAsset(id.to_owned()))?;
        if policy == AssetPolicy::Disabled {
            return Ok(None);
        }
        if !descriptor.compatibility.matches_context(context) {
            return match policy {
                AssetPolicy::Required => Err(AssetError::IncompatibleAsset {
                    id: descriptor.id.clone(),
                    camera_model: context.camera_model.to_owned(),
                    lens_type: context.lens_type,
                    target_triple: context.target_triple.to_owned(),
                }),
                AssetPolicy::Automatic | AssetPolicy::Disabled => Ok(None),
            };
        }
        self.load_verified(provider, id, policy)
    }

    /// Loads a qualified logical model and returns it only when every member verifies.
    ///
    /// `Automatic` returns `None` when the group is unqualified, incompatible, or any
    /// member is absent. A corrupt member remains an error. `Required` reports each of
    /// those conditions as a typed error. No partial group is ever returned.
    pub fn load_verified_group_for(
        &self,
        provider: &dyn AssetProvider,
        id: &str,
        policy: AssetPolicy,
        context: AssetContext<'_>,
    ) -> AssetResult<Option<VerifiedAssetGroup>> {
        self.validate()?;
        let descriptor = self
            .group(id)
            .ok_or_else(|| AssetError::UndeclaredAssetGroup(id.to_owned()))?;
        if policy == AssetPolicy::Disabled {
            return Ok(None);
        }
        if !descriptor.compatibility.matches_context(context) {
            return match policy {
                AssetPolicy::Required => Err(AssetError::IncompatibleAssetGroup {
                    id: descriptor.id.clone(),
                    camera_model: context.camera_model.to_owned(),
                    lens_type: context.lens_type,
                    target_triple: context.target_triple.to_owned(),
                }),
                AssetPolicy::Automatic | AssetPolicy::Disabled => Ok(None),
            };
        }

        let mut assets = Vec::with_capacity(descriptor.members.len());
        for member in &descriptor.members {
            let Some(asset) = self.load_verified(provider, member, policy)? else {
                return Ok(None);
            };
            assets.push(asset);
        }
        Ok(Some(VerifiedAssetGroup {
            descriptor: descriptor.clone(),
            assets,
        }))
    }
}

/// Bytes that matched both the declared length and SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAsset {
    pub descriptor: AssetDescriptor,
    pub digest: Sha256Digest,
    pub bytes: Vec<u8>,
}

/// Complete logical model whose member payloads all passed length and digest checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAssetGroup {
    /// Manifest description of the logical model.
    pub descriptor: AssetGroupDescriptor,
    /// Verified payloads in the descriptor's declared member order.
    pub assets: Vec<VerifiedAsset>,
}

/// Source of licensed asset bytes, embedded or application-owned.
pub trait AssetProvider: Send + Sync {
    /// Return `Ok(None)` only when the relative path is absent.
    fn load(&self, relative_path: &str) -> AssetResult<Option<Vec<u8>>>;
}

/// Reads a bundle from an existing directory while preventing path and symlink escape.
#[derive(Debug, Clone)]
pub struct DirectoryAssetProvider {
    root: PathBuf,
}

impl DirectoryAssetProvider {
    pub fn new(root: impl AsRef<Path>) -> AssetResult<Self> {
        let supplied = root.as_ref();
        let root = fs::canonicalize(supplied).map_err(|source| AssetError::Io {
            path: supplied.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(AssetError::ProviderRootNotDirectory(root));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl AssetProvider for DirectoryAssetProvider {
    fn load(&self, relative_path: &str) -> AssetResult<Option<Vec<u8>>> {
        validate_relative_path(relative_path)?;
        let requested = self.root.join(relative_path);
        let canonical = match fs::canonicalize(&requested) {
            Ok(path) => path,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(AssetError::Io {
                    path: requested,
                    source,
                });
            }
        };
        if !canonical.starts_with(&self.root) {
            return Err(AssetError::AssetPathEscapesRoot(relative_path.to_owned()));
        }
        if !canonical.is_file() {
            return Err(AssetError::ProviderPathNotFile(canonical));
        }
        fs::read(&canonical)
            .map(Some)
            .map_err(|source| AssetError::Io {
                path: canonical,
                source,
            })
    }
}

/// Small provider useful for callers that already manage payload storage or downloads.
#[derive(Debug, Clone, Default)]
pub struct InMemoryAssetProvider {
    assets: HashMap<String, Vec<u8>>,
}

impl InMemoryAssetProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        relative_path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> AssetResult<Option<Vec<u8>>> {
        let relative_path = relative_path.into();
        validate_relative_path(&relative_path)?;
        Ok(self.assets.insert(relative_path, bytes.into()))
    }
}

impl AssetProvider for InMemoryAssetProvider {
    fn load(&self, relative_path: &str) -> AssetResult<Option<Vec<u8>>> {
        validate_relative_path(relative_path)?;
        Ok(self.assets.get(relative_path).cloned())
    }
}

fn validate_relative_path(path: &str) -> AssetResult<()> {
    let invalid_component = path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..");
    let looks_like_drive_path = path.as_bytes().get(1) == Some(&b':');
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || invalid_component
        || looks_like_drive_path
        || Path::new(path).is_absolute()
    {
        return Err(AssetError::InvalidAssetPath(path.to_owned()));
    }
    Ok(())
}

/// A SHA-256 digest used for content-addressed asset verification.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub fn from_hex(hex: &str) -> AssetResult<Self> {
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(AssetError::InvalidSha256(hex.to_owned()));
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex_nibble(pair[0])
                .ok_or_else(|| AssetError::InvalidSha256(hex.to_owned()))?;
            let low = decode_hex_nibble(pair[1])
                .ok_or_else(|| AssetError::InvalidSha256(hex.to_owned()))?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
        output
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Sha256Digest")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Calculate SHA-256 without requiring a cryptography runtime or platform API.
pub fn sha256(bytes: &[u8]) -> Sha256Digest {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut state = INITIAL;
    let mut chunks = bytes.chunks_exact(64);
    for chunk in &mut chunks {
        compress_sha256(&mut state, chunk.try_into().expect("64-byte chunk"));
    }

    let remainder = chunks.remainder();
    let padded_length = if remainder.len() < 56 { 64 } else { 128 };
    let mut padding = [0_u8; 128];
    padding[..remainder.len()].copy_from_slice(remainder);
    padding[remainder.len()] = 0x80;
    let bit_length = (bytes.len() as u64).wrapping_mul(8);
    padding[padded_length - 8..padded_length].copy_from_slice(&bit_length.to_be_bytes());
    for chunk in padding[..padded_length].chunks_exact(64) {
        compress_sha256(&mut state, chunk.try_into().expect("64-byte chunk"));
    }

    let mut output = [0_u8; 32];
    for (word, destination) in state.iter().zip(output.chunks_exact_mut(4)) {
        destination.copy_from_slice(&word.to_be_bytes());
    }
    Sha256Digest(output)
}

fn compress_sha256(state: &mut [u32; 8], block: &[u8; 64]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut schedule = [0_u32; 64];
    for (destination, source) in schedule[..16].iter_mut().zip(block.chunks_exact(4)) {
        *destination = u32::from_be_bytes(source.try_into().expect("4-byte word"));
    }
    for index in 16..64 {
        let s0 = schedule[index - 15].rotate_right(7)
            ^ schedule[index - 15].rotate_right(18)
            ^ (schedule[index - 15] >> 3);
        let s1 = schedule[index - 2].rotate_right(17)
            ^ schedule[index - 2].rotate_right(19)
            ^ (schedule[index - 2] >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(sum1)
            .wrapping_add(choice)
            .wrapping_add(K[index])
            .wrapping_add(schedule[index]);
        let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = sum0.wrapping_add(majority);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Portable representation of the linear OpenCV SVM payloads shipped by the vendor.
///
/// This is intentionally only the trained-model boundary. The vendor does not document
/// the image crop, channel order, scaling, normalization, or flattening that produces
/// its feature vector. Callers must not claim accessory classification until that
/// preprocessing has been independently qualified.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCvLinearSvm {
    pub format: u32,
    pub svm_type: String,
    pub variable_count: usize,
    pub support_vectors: Vec<Vec<f32>>,
    pub decision_function: LinearSvmDecisionFunction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinearSvmDecisionFunction {
    pub support_vector_count: usize,
    pub rho: f64,
    pub alpha: Vec<f64>,
}

impl OpenCvLinearSvm {
    /// Parse the narrow OpenCV XML schema used by the licensed vendor resources.
    /// Indexed classifier decision functions are rejected because this schema
    /// pairs each alpha directly with the corresponding stored support vector.
    pub fn parse_xml(bytes: &[u8]) -> AssetResult<Self> {
        let xml = std::str::from_utf8(bytes).map_err(AssetError::InvalidSvmUtf8)?;
        let storage = xml_tag(xml, "opencv_storage")?;
        let svm = xml_tag(storage, "opencv_ml_svm")?;
        let format = parse_integer::<u32>(xml_tag(svm, "format")?, "format")?;
        if format != 3 {
            return Err(AssetError::UnsupportedSvmFormat(format));
        }
        let svm_type = xml_tag(svm, "svmType")?.trim().to_owned();
        if svm_type.is_empty() {
            return Err(invalid_svm("svmType must not be empty"));
        }
        let kernel = xml_tag(svm, "kernel")?;
        let kernel_type = xml_tag(kernel, "type")?.trim();
        if kernel_type != "LINEAR" {
            return Err(AssetError::UnsupportedSvmKernel(kernel_type.to_owned()));
        }

        let variable_count = parse_integer::<usize>(xml_tag(svm, "var_count")?, "var_count")?;
        if variable_count == 0 {
            return Err(invalid_svm("var_count must be positive"));
        }
        let declared_sv_total = parse_integer::<usize>(xml_tag(svm, "sv_total")?, "sv_total")?;
        let vectors_xml = xml_tag(svm, "support_vectors")?;
        let vector_elements = xml_elements(vectors_xml, "_")?;
        let mut support_vectors = Vec::with_capacity(vector_elements.len());
        for element in vector_elements {
            let values = parse_f32_list(element, "support vector")?;
            if values.len() != variable_count {
                return Err(invalid_svm(format!(
                    "support vector has {} values, expected {variable_count}",
                    values.len()
                )));
            }
            support_vectors.push(values);
        }
        if support_vectors.len() != declared_sv_total {
            return Err(invalid_svm(format!(
                "sv_total is {declared_sv_total}, but {} support vectors were found",
                support_vectors.len()
            )));
        }

        let functions_xml = xml_tag(svm, "decision_functions")?;
        let function_elements = xml_elements(functions_xml, "_")?;
        if function_elements.len() != 1 {
            return Err(invalid_svm(format!(
                "expected one decision function, found {}",
                function_elements.len()
            )));
        }
        let function = function_elements[0];
        if function.match_indices("<index").any(|(offset, opening)| {
            function
                .as_bytes()
                .get(offset + opening.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>'))
        }) {
            return Err(invalid_svm(
                "indexed decision functions are unsupported by the SDK-shaped SVM reader",
            ));
        }
        let support_vector_count =
            parse_integer::<usize>(xml_tag(function, "sv_count")?, "sv_count")?;
        let rho = parse_f64(xml_tag(function, "rho")?, "rho")?;
        let alpha = parse_f64_list(xml_tag(function, "alpha")?, "alpha")?;
        if support_vector_count != support_vectors.len() || alpha.len() != support_vector_count {
            return Err(invalid_svm(format!(
                "decision function references {support_vector_count} vectors with {} alphas, but {} vectors exist",
                alpha.len(),
                support_vectors.len()
            )));
        }

        Ok(Self {
            format,
            svm_type,
            variable_count,
            support_vectors,
            decision_function: LinearSvmDecisionFunction {
                support_vector_count,
                rho,
                alpha,
            },
        })
    }

    /// Evaluates the trained linear decision function for an already-preprocessed feature vector.
    ///
    /// This is the portable mathematical tail of OpenCV's linear SVM/SVR prediction:
    /// `sum(alpha[i] * dot(support_vector[i], features)) - rho`. It deliberately
    /// does not extract image features; callers must supply a vector produced by a
    /// separately qualified camera-specific preprocessing pipeline.
    /// Public model fields are rechecked for consistent dimensions; nonfinite
    /// features or results are rejected rather than returned as predictions.
    pub fn decision_value(&self, features: &[f32]) -> AssetResult<f64> {
        if features.len() != self.variable_count {
            return Err(AssetError::InvalidSvmInput {
                expected: self.variable_count,
                actual: features.len(),
            });
        }

        if self.variable_count == 0
            || self.support_vectors.is_empty()
            || self.decision_function.support_vector_count != self.support_vectors.len()
            || self.decision_function.alpha.len() != self.support_vectors.len()
            || self
                .support_vectors
                .iter()
                .any(|vector| vector.len() != self.variable_count)
        {
            return Err(invalid_svm("inconsistent decision-function dimensions"));
        }
        if !features.iter().all(|value| value.is_finite()) {
            return Err(invalid_svm("feature vector contains a non-finite value"));
        }

        let weighted_sum = self
            .support_vectors
            .iter()
            .zip(&self.decision_function.alpha)
            .map(|(support_vector, alpha)| {
                let dot_product = support_vector
                    .iter()
                    .zip(features)
                    .map(|(left, right)| f64::from(*left) * f64::from(*right))
                    .sum::<f64>();
                *alpha * dot_product
            })
            .sum::<f64>();
        let decision = weighted_sum - self.decision_function.rho;
        if !decision.is_finite() {
            return Err(invalid_svm("decision function produced a non-finite value"));
        }
        Ok(decision)
    }
}

fn xml_tag<'a>(source: &'a str, tag: &str) -> AssetResult<&'a str> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let start = source
        .find(&opening)
        .ok_or_else(|| invalid_svm(format!("missing <{tag}>")))?
        + opening.len();
    let end = source[start..]
        .find(&closing)
        .map(|offset| start + offset)
        .ok_or_else(|| invalid_svm(format!("missing </{tag}>")))?;
    Ok(&source[start..end])
}

fn xml_elements<'a>(source: &'a str, tag: &str) -> AssetResult<Vec<&'a str>> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let mut remaining = source;
    let mut elements = Vec::new();
    while let Some(offset) = remaining.find(&opening) {
        let content_start = offset + opening.len();
        let content_end = remaining[content_start..]
            .find(&closing)
            .map(|end| content_start + end)
            .ok_or_else(|| invalid_svm(format!("missing </{tag}>")))?;
        elements.push(&remaining[content_start..content_end]);
        remaining = &remaining[content_end + closing.len()..];
    }
    if elements.is_empty() {
        return Err(invalid_svm(format!("missing <{tag}> elements")));
    }
    Ok(elements)
}

fn parse_integer<T>(source: &str, field: &str) -> AssetResult<T>
where
    T: std::str::FromStr,
{
    source
        .trim()
        .parse()
        .map_err(|_| invalid_svm(format!("invalid {field}")))
}

fn parse_f64(source: &str, field: &str) -> AssetResult<f64> {
    let value = source
        .trim()
        .parse::<f64>()
        .map_err(|_| invalid_svm(format!("invalid {field}")))?;
    if !value.is_finite() {
        return Err(invalid_svm(format!("non-finite {field}")));
    }
    Ok(value)
}

fn parse_f32_list(source: &str, field: &str) -> AssetResult<Vec<f32>> {
    source
        .split_ascii_whitespace()
        .map(|token| {
            let value = token
                .parse::<f32>()
                .map_err(|_| invalid_svm(format!("invalid {field} value {token:?}")))?;
            if !value.is_finite() {
                return Err(invalid_svm(format!("non-finite {field} value")));
            }
            Ok(value)
        })
        .collect()
}

fn parse_f64_list(source: &str, field: &str) -> AssetResult<Vec<f64>> {
    source
        .split_ascii_whitespace()
        .map(|token| parse_f64(token, field))
        .collect()
}

fn invalid_svm(message: impl Into<String>) -> AssetError {
    AssetError::InvalidSvm(message.into())
}

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("unsupported asset manifest schema {found}; supported schema is {supported}")]
    UnsupportedManifestVersion { found: u32, supported: u32 },
    #[error("invalid asset manifest: {0}")]
    InvalidManifest(String),
    #[error("duplicate asset id {0:?}")]
    DuplicateAssetId(String),
    #[error("duplicate asset path {0:?}")]
    DuplicateAssetPath(String),
    #[error("duplicate asset group id {0:?}")]
    DuplicateAssetGroupId(String),
    #[error("asset group {group:?} contains duplicate member {member:?}")]
    DuplicateAssetGroupMember { group: String, member: String },
    #[error("asset group {group:?} references undeclared member {member:?}")]
    MissingAssetGroupMember { group: String, member: String },
    #[error(
        "asset group {group:?} has kind {group_kind:?}, but member {member:?} has kind {member_kind:?}"
    )]
    AssetGroupKindMismatch {
        group: String,
        member: String,
        group_kind: AssetKind,
        member_kind: AssetKind,
    },
    #[error("asset {0:?} is not declared by the manifest")]
    UndeclaredAsset(String),
    #[error("asset group {0:?} is not declared by the manifest")]
    UndeclaredAssetGroup(String),
    #[error("invalid relative asset path {0:?}")]
    InvalidAssetPath(String),
    #[error("asset path escapes the provider root: {0:?}")]
    AssetPathEscapesRoot(String),
    #[error("asset provider root is not a directory: {0}")]
    ProviderRootNotDirectory(PathBuf),
    #[error("asset provider path is not a file: {0}")]
    ProviderPathNotFile(PathBuf),
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("required asset {id:?} is missing at {path:?}")]
    MissingRequiredAsset { id: String, path: String },
    #[error(
        "asset {id:?} is not qualified for camera {camera_model}, lens {lens_type}, target {target_triple}"
    )]
    IncompatibleAsset {
        id: String,
        camera_model: String,
        lens_type: u32,
        target_triple: String,
    },
    #[error(
        "asset group {id:?} is not qualified for camera {camera_model}, lens {lens_type}, target {target_triple}"
    )]
    IncompatibleAssetGroup {
        id: String,
        camera_model: String,
        lens_type: u32,
        target_triple: String,
    },
    #[error("asset {id:?} length mismatch: expected {expected}, got {actual}")]
    LengthMismatch {
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("invalid SHA-256 digest {0:?}")]
    InvalidSha256(String),
    #[error("asset {id:?} SHA-256 mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        id: String,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("OpenCV SVM XML is not UTF-8: {0}")]
    InvalidSvmUtf8(#[source] std::str::Utf8Error),
    #[error("unsupported OpenCV SVM format {0}")]
    UnsupportedSvmFormat(u32),
    #[error("unsupported OpenCV SVM kernel {0:?}; only LINEAR is portable here")]
    UnsupportedSvmKernel(String),
    #[error("invalid OpenCV linear SVM: {0}")]
    InvalidSvm(String),
    #[error("OpenCV linear SVM expected {expected} input features, got {actual}")]
    InvalidSvmInput { expected: usize, actual: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn descriptor(bytes: &[u8]) -> AssetDescriptor {
        AssetDescriptor {
            id: "ai-flow-v22".to_owned(),
            kind: AssetKind::AiStitch,
            path: "models/ai-flow-v22.bin".to_owned(),
            sha256: sha256(bytes).to_hex(),
            byte_length: bytes.len() as u64,
            compatibility: AssetCompatibility {
                qualification: AssetQualification::Qualified,
                camera_models: vec!["X5".to_owned()],
                lens_types: vec![113],
                ..AssetCompatibility::default()
            },
            provenance: AssetProvenance::default(),
        }
    }

    fn bundle(bytes: &[u8]) -> ModelBundle {
        ModelBundle {
            schema_version: MODEL_BUNDLE_SCHEMA_VERSION,
            bundle_id: "licensed-test-assets".to_owned(),
            bundle_version: "2026-09-08.1".to_owned(),
            assets: vec![descriptor(bytes)],
            groups: Vec::new(),
        }
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256(b"").to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").to_hex(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            Sha256Digest::from_hex(
                "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
            )
            .unwrap(),
            sha256(b"abc")
        );
    }

    #[test]
    fn manifest_round_trips_and_validates() {
        let original = bundle(b"licensed payload");
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ModelBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
        decoded.validate().unwrap();

        let mut duplicate = decoded;
        duplicate.assets.push(duplicate.assets[0].clone());
        assert!(matches!(
            duplicate.validate(),
            Err(AssetError::DuplicateAssetId(_))
        ));

        let mut duplicate_path = bundle(b"licensed payload");
        duplicate_path.assets.push(AssetDescriptor {
            id: "second-id".to_owned(),
            ..duplicate_path.assets[0].clone()
        });
        assert!(matches!(
            duplicate_path.validate(),
            Err(AssetError::DuplicateAssetPath(_))
        ));
    }

    #[test]
    fn in_memory_provider_obeys_policy_and_verifies_integrity() {
        let bytes = b"licensed payload";
        let bundle = bundle(bytes);
        let mut provider = InMemoryAssetProvider::new();

        assert!(bundle
            .load_verified(&provider, "ai-flow-v22", AssetPolicy::Automatic)
            .unwrap()
            .is_none());
        assert!(matches!(
            bundle.load_verified(&provider, "ai-flow-v22", AssetPolicy::Required),
            Err(AssetError::MissingRequiredAsset { .. })
        ));

        provider
            .insert("models/ai-flow-v22.bin", &bytes[..])
            .unwrap();
        let verified = bundle
            .load_verified(&provider, "ai-flow-v22", AssetPolicy::Required)
            .unwrap()
            .unwrap();
        assert_eq!(verified.bytes, bytes);
        assert_eq!(verified.digest, sha256(bytes));

        provider
            .insert("models/ai-flow-v22.bin", &b"licensed payloae"[..])
            .unwrap();
        assert!(matches!(
            bundle.load_verified(&provider, "ai-flow-v22", AssetPolicy::Automatic),
            Err(AssetError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn disabled_policy_does_not_touch_provider() {
        struct CountingProvider(AtomicUsize);
        impl AssetProvider for CountingProvider {
            fn load(&self, _relative_path: &str) -> AssetResult<Option<Vec<u8>>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Some(Vec::new()))
            }
        }

        let provider = CountingProvider(AtomicUsize::new(0));
        assert!(bundle(b"payload")
            .load_verified(&provider, "ai-flow-v22", AssetPolicy::Disabled)
            .unwrap()
            .is_none());
        assert_eq!(provider.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn compatibility_is_enforced_before_loading() {
        let bytes = b"payload";
        let bundle = bundle(bytes);
        let mut provider = InMemoryAssetProvider::new();
        provider
            .insert("models/ai-flow-v22.bin", &bytes[..])
            .unwrap();
        let qualified = AssetContext {
            camera_model: "x5",
            lens_type: 113,
            target_triple: "aarch64-apple-darwin",
        };
        assert!(bundle
            .load_verified_for(&provider, "ai-flow-v22", AssetPolicy::Required, qualified)
            .unwrap()
            .is_some());

        let incompatible = AssetContext {
            lens_type: 117,
            ..qualified
        };
        assert!(bundle
            .load_verified_for(
                &provider,
                "ai-flow-v22",
                AssetPolicy::Automatic,
                incompatible,
            )
            .unwrap()
            .is_none());
        assert!(matches!(
            bundle.load_verified_for(
                &provider,
                "ai-flow-v22",
                AssetPolicy::Required,
                incompatible,
            ),
            Err(AssetError::IncompatibleAsset { lens_type: 117, .. })
        ));
    }

    #[test]
    fn unqualified_compatibility_is_default_deny() {
        let bytes = b"payload";
        let mut bundle = bundle(bytes);
        bundle.assets[0].compatibility = AssetCompatibility {
            camera_models: vec!["X5".to_owned()],
            lens_types: vec![113],
            ..AssetCompatibility::default()
        };
        let mut provider = InMemoryAssetProvider::new();
        provider
            .insert("models/ai-flow-v22.bin", &bytes[..])
            .unwrap();
        let context = AssetContext {
            camera_model: "X5",
            lens_type: 113,
            target_triple: "aarch64-apple-darwin",
        };

        assert!(bundle
            .load_verified_for(&provider, "ai-flow-v22", AssetPolicy::Automatic, context)
            .unwrap()
            .is_none());
        assert!(matches!(
            bundle.load_verified_for(&provider, "ai-flow-v22", AssetPolicy::Required, context),
            Err(AssetError::IncompatibleAsset { .. })
        ));
    }

    #[test]
    fn asset_groups_require_unique_declared_members() {
        let bytes = b"payload";
        let mut bundle = bundle(bytes);
        bundle.groups.push(AssetGroupDescriptor {
            id: "complete-model".to_owned(),
            kind: AssetKind::AiStitch,
            members: vec!["ai-flow-v22".to_owned()],
            compatibility: AssetCompatibility {
                qualification: AssetQualification::Qualified,
                camera_models: vec!["X5".to_owned()],
                lens_types: vec![113],
                ..AssetCompatibility::default()
            },
        });
        bundle.validate().unwrap();
        assert_eq!(bundle.group("complete-model"), bundle.groups.first());

        let context = AssetContext {
            camera_model: "X5",
            lens_type: 113,
            target_triple: "aarch64-apple-darwin",
        };
        let mut provider = InMemoryAssetProvider::new();
        assert!(bundle
            .load_verified_group_for(&provider, "complete-model", AssetPolicy::Automatic, context,)
            .unwrap()
            .is_none());
        provider
            .insert("models/ai-flow-v22.bin", &bytes[..])
            .unwrap();
        let verified = bundle
            .load_verified_group_for(&provider, "complete-model", AssetPolicy::Required, context)
            .unwrap()
            .unwrap();
        assert_eq!(verified.assets.len(), 1);
        assert_eq!(verified.assets[0].bytes, bytes);

        let mut wrong_kind = bundle.clone();
        wrong_kind.assets[0].kind = AssetKind::ColorLut;
        assert!(matches!(
            wrong_kind.validate(),
            Err(AssetError::AssetGroupKindMismatch { .. })
        ));

        bundle.groups[0].members.push("missing".to_owned());
        assert!(matches!(
            bundle.validate(),
            Err(AssetError::MissingAssetGroupMember { ref member, .. }) if member == "missing"
        ));
    }

    #[test]
    fn directory_provider_reads_only_relative_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("models")).unwrap();
        fs::write(directory.path().join("models/model.bin"), b"model").unwrap();
        let provider = DirectoryAssetProvider::new(directory.path()).unwrap();

        assert_eq!(
            provider.load("models/model.bin").unwrap(),
            Some(b"model".to_vec())
        );
        assert_eq!(provider.load("models/missing.bin").unwrap(), None);
        assert!(matches!(
            provider.load("../outside.bin"),
            Err(AssetError::InvalidAssetPath(_))
        ));
        assert!(matches!(
            provider.load("C:\\outside.bin"),
            Err(AssetError::InvalidAssetPath(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_provider_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("model.bin"), b"outside").unwrap();
        symlink(
            outside.path().join("model.bin"),
            directory.path().join("model.bin"),
        )
        .unwrap();
        let provider = DirectoryAssetProvider::new(directory.path()).unwrap();

        assert!(matches!(
            provider.load("model.bin"),
            Err(AssetError::AssetPathEscapesRoot(_))
        ));
    }

    #[test]
    fn parses_sdk_shaped_opencv_linear_svm() {
        let xml = br#"<?xml version="1.0"?>
<opencv_storage>
<opencv_ml_svm>
  <format>3</format>
  <svmType>EPS_SVR</svmType>
  <kernel><type>LINEAR</type></kernel>
  <var_count>3</var_count>
  <sv_total>1</sv_total>
  <support_vectors><_>1.25 -2.5 3.75</_></support_vectors>
  <decision_functions><_>
    <sv_count>1</sv_count>
    <rho>-1.0227784203256246e-01</rho>
    <alpha>1.</alpha>
  </_></decision_functions>
</opencv_ml_svm>
</opencv_storage>"#;

        let svm = OpenCvLinearSvm::parse_xml(xml).unwrap();
        assert_eq!(svm.format, 3);
        assert_eq!(svm.svm_type, "EPS_SVR");
        assert_eq!(svm.variable_count, 3);
        assert_eq!(svm.support_vectors, vec![vec![1.25, -2.5, 3.75]]);
        assert_eq!(svm.decision_function.support_vector_count, 1);
        assert_eq!(svm.decision_function.alpha, vec![1.0]);
        assert!((svm.decision_function.rho + 0.10227784203256246).abs() < 1e-15);
        let decision = svm
            .decision_value(&[2.0, 3.0, -4.0])
            .expect("matching feature vector");
        let expected = 1.25 * 2.0 + -2.5 * 3.0 + 3.75 * -4.0 + 0.10227784203256246;
        assert!((decision - expected).abs() < 1e-12);
        assert!(matches!(
            svm.decision_value(&[1.0, 2.0]),
            Err(AssetError::InvalidSvmInput {
                expected: 3,
                actual: 2
            })
        ));
    }

    #[test]
    fn svm_parser_rejects_unknown_preconditions_and_bad_dimensions() {
        let non_linear = br#"<opencv_storage><opencv_ml_svm>
<format>3</format><svmType>EPS_SVR</svmType><kernel><type>RBF</type></kernel>
</opencv_ml_svm></opencv_storage>"#;
        assert!(matches!(
            OpenCvLinearSvm::parse_xml(non_linear),
            Err(AssetError::UnsupportedSvmKernel(kernel)) if kernel == "RBF"
        ));

        let bad_dimensions = br#"<opencv_storage><opencv_ml_svm>
<format>3</format><svmType>EPS_SVR</svmType><kernel><type>LINEAR</type></kernel>
<var_count>2</var_count><sv_total>1</sv_total>
<support_vectors><_>1 2 3</_></support_vectors>
<decision_functions><_><sv_count>1</sv_count><rho>0</rho><alpha>1</alpha></_></decision_functions>
</opencv_ml_svm></opencv_storage>"#;
        assert!(matches!(
            OpenCvLinearSvm::parse_xml(bad_dimensions),
            Err(AssetError::InvalidSvm(_))
        ));
    }

    #[test]
    fn svm_rejects_indexed_decisions_instead_of_ignoring_vector_order() {
        let indexed = r#"<opencv_storage><opencv_ml_svm>
<format>3</format><svmType>C_SVC</svmType><kernel><type>LINEAR</type></kernel>
<var_count>1</var_count><sv_total>2</sv_total>
<support_vectors><_>2</_><_>7</_></support_vectors>
<decision_functions><_><sv_count>2</sv_count><rho>0</rho><alpha>3 5</alpha><index>1 0</index></_></decision_functions>
</opencv_ml_svm></opencv_storage>"#;
        // Ignoring the index produces 41 instead of 31 for the input [1].
        for element in [
            "<index>1 0</index>",
            "<index >1 0</index>",
            "<index\n>1 0</index>",
            "<index source=\"classifier\">1 0</index>",
            "<index/>",
        ] {
            let xml = indexed.replace("<index>1 0</index>", element);
            assert!(
                matches!(OpenCvLinearSvm::parse_xml(xml.as_bytes()),
                Err(AssetError::InvalidSvm(message)) if message.contains("indexed")),
                "unsupported index element was ignored: {element}"
            );
        }
    }

    #[test]
    fn svm_predictions_validate_mutable_model_shapes_and_nonfinite_data() {
        let model = OpenCvLinearSvm {
            format: 3,
            svm_type: "EPS_SVR".into(),
            variable_count: 2,
            support_vectors: vec![vec![2.0, 3.0]],
            decision_function: LinearSvmDecisionFunction {
                support_vector_count: 1,
                rho: 1.0,
                alpha: vec![1.0],
            },
        };
        assert_eq!(model.decision_value(&[5.0, 7.0]).unwrap(), 30.0);
        for input in [
            [f32::NAN, 0.0],
            [f32::INFINITY, 1.0],
            [0.0, f32::NEG_INFINITY],
        ] {
            assert!(model.decision_value(&input).is_err());
        }
        for mutation in 0..6 {
            let mut changed = model.clone();
            match mutation {
                0 => {
                    changed.support_vectors[0].pop();
                }
                1 => changed.decision_function.alpha.clear(),
                2 => changed.decision_function.support_vector_count = 0,
                3 => changed.support_vectors[0][0] = f32::INFINITY,
                4 => changed.decision_function.rho = f64::NAN,
                _ => changed.decision_function.alpha[0] = f64::MAX,
            }
            assert!(changed.decision_value(&[5.0, 7.0]).is_err());
        }
    }
}
