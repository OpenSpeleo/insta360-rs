# insta360-rs-data-core

Original Insta360 SDK and Studio data used by
[insta360-rs](https://github.com/OpenSpeleo/insta360-rs). This package contains
36 files: camera configurations, three I-Log LUTs, accessory/cooling-shell
classifiers, stitching models, the model catalog, and sharpening settings.

This dependency-free `no_std` crate exposes immutable `PAYLOADS` and a
`MANIFEST` with source provenance, byte lengths, and SHA-256 digests.
Applications normally use `insta360_rs::assets::BundledAssetProvider` for
validated access. Model bytes alone do not implement image preprocessing,
inference, or postprocessing.

The two data crates keep each published archive below 10,000,000 bytes while
preserving all original resources. Their payloads are embedded directly in the
compiled library. The library pins their versions so its manifest matches the
embedded bytes.

Project-authored Rust code uses Apache-2.0. Original vendor data retains its
vendor licensing; see [LICENSE.md](LICENSE.md), [NOTICE.md](NOTICE.md), and
[model-bundle.json](model-bundle.json).
