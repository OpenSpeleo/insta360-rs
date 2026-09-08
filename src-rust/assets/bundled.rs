//! Direct access to the checked-in Insta360 and Studio payloads.

use super::{validate_relative_path, AssetError, AssetProvider, AssetResult, ModelBundle};

/// Manifest for the original resources embedded through the two data dependencies.
pub const BUNDLED_MANIFEST: &str = include_str!("model-bundle.json");

/// Loads licensed resources embedded at compile time, without filesystem access.
#[derive(Debug, Clone, Copy, Default)]
pub struct BundledAssetProvider;

impl BundledAssetProvider {
    /// Reads and validates the bundled descriptors; payloads are verified when loaded.
    ///
    /// ```
    /// use insta360_rs::assets::{AssetPolicy, BundledAssetProvider};
    ///
    /// let bundle = BundledAssetProvider::manifest()?;
    /// let model = bundle.load_verified(
    ///     &BundledAssetProvider,
    ///     "camera-accessory-svm-0db3a7a0-xml",
    ///     AssetPolicy::Required,
    /// )?;
    /// assert!(model.is_some());
    /// # Ok::<(), insta360_rs::assets::AssetError>(())
    /// ```
    pub fn manifest() -> AssetResult<ModelBundle> {
        let bundle: ModelBundle = serde_json::from_str(BUNDLED_MANIFEST)
            .map_err(|error| AssetError::InvalidManifest(error.to_string()))?;
        bundle.validate()?;
        Ok(bundle)
    }
}

impl AssetProvider for BundledAssetProvider {
    fn load(&self, relative_path: &str) -> AssetResult<Option<Vec<u8>>> {
        validate_relative_path(relative_path)?;
        Ok(payloads()
            .find(|(path, _)| *path == relative_path)
            .map(|(_, bytes)| bytes.to_vec()))
    }
}

fn payloads() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    insta360_rs_data_core::PAYLOADS
        .iter()
        .chain(insta360_rs_data_enhancement::PAYLOADS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{AssetKind, AssetPolicy, AssetQualification, OpenCvLinearSvm};

    #[test]
    fn every_bundled_payload_matches_manifest_and_source_digest() {
        let bundle = BundledAssetProvider::manifest().unwrap();
        let paths: std::collections::HashSet<_> = payloads().map(|(path, _)| *path).collect();
        assert_eq!(
            paths.len(),
            payloads().count(),
            "duplicate data-crate paths"
        );
        assert_eq!(bundle.assets.len(), paths.len());
        for descriptor in &bundle.assets {
            let asset = bundle
                .load_verified(&BundledAssetProvider, &descriptor.id, AssetPolicy::Required)
                .unwrap()
                .unwrap();
            assert_eq!(
                descriptor.provenance.source_sha256.as_deref(),
                Some(descriptor.sha256.as_str()),
                "{} must preserve the original source bytes",
                descriptor.path,
            );
            assert_eq!(
                descriptor.compatibility.qualification,
                AssetQualification::Unqualified,
            );
            if matches!(
                descriptor.kind,
                AssetKind::AccessoryDetectorSvm | AssetKind::CoolingShellDetectorSvm
            ) {
                OpenCvLinearSvm::parse_xml(&asset.bytes).unwrap();
            }
        }
        let group = bundle.group("ai-seam-coreml-v22").unwrap();
        assert_eq!(group.members.len(), 8);
        assert_eq!(
            group.compatibility.qualification,
            AssetQualification::Unqualified
        );
    }

    #[test]
    fn data_crate_manifests_match_the_public_bundle() {
        let bundle = BundledAssetProvider::manifest().unwrap();
        let mut assets = Vec::new();
        let mut groups = Vec::new();
        for (manifest, payloads) in [
            (
                insta360_rs_data_core::MANIFEST,
                insta360_rs_data_core::PAYLOADS,
            ),
            (
                insta360_rs_data_enhancement::MANIFEST,
                insta360_rs_data_enhancement::PAYLOADS,
            ),
        ] {
            let data: ModelBundle = serde_json::from_str(manifest).unwrap();
            data.validate().unwrap();
            assert_eq!(data.bundle_version, bundle.bundle_version);
            assert_eq!(data.assets.len(), payloads.len());
            for descriptor in &data.assets {
                assert!(payloads.iter().any(|(path, _)| *path == descriptor.path));
            }
            assets.extend(data.assets);
            groups.extend(data.groups);
        }
        assets.sort_by(|a, b| a.id.cmp(&b.id));
        let mut expected = bundle.assets;
        expected.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(assets, expected);
        assert_eq!(groups, bundle.groups);
    }

    #[test]
    fn bundled_provider_rejects_invalid_paths_and_handles_missing_assets() {
        assert!(BundledAssetProvider
            .load("models/missing.ins")
            .unwrap()
            .is_none());
        for path in [
            "../model-bundle.json",
            "/models/ai_stitcher.ins",
            "models/../ai_stitcher.ins",
        ] {
            assert!(matches!(
                BundledAssetProvider.load(path),
                Err(AssetError::InvalidAssetPath(_))
            ));
        }
    }
}
