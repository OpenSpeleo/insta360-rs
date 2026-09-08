# Literal SDK and Studio copy inventory

## Scope

This inventory records original files copied byte-for-byte from the licensed
Insta360 SDK and installed Insta360 Studio into `data/core/assets/` and
`data/enhancement/assets/`. Reimplemented behavior, inferred formats, constants,
and static-analysis evidence are outside this inventory.

## Copied resources

The two data crates include **41 original files totaling 21,814,090 bytes**
(about 20.8 MiB). The complete per-file inventory is
[`src-rust/assets/model-bundle.json`](../src-rust/assets/model-bundle.json),
including logical provider path, role, source release, original source path,
byte length, and source/stored SHA-256 digests. Vendor source paths are relative
to the supplied distribution root. Paths beginning with `Insta360 Studio.app/`
are relative to `/Applications/`.

| Source release         | Original resources                                                            | Files |
| ---------------------- | ----------------------------------------------------------------------------- | ----: |
| MediaSDK 3.1.5, Linux  | AI stitch, ColorPlus, deflicker, two defringe, and JPEG denoise `.ins` models |     6 |
| MediaSDK 3.1.5, Linux  | Camera accessory and cooling-shell OpenCV XML models                          |    10 |
| CameraSDK 2.1.8, Linux | `camera_conf_Insta360_*.json` camera configuration files                      |    10 |
| iOS SDK 1.10.4         | `INSCoreMedia.framework/model_info.json` catalog                              |     1 |
| iOS SDK 1.10.4         | Complete `model_export_v22_d7c49187.mlmodelc` CoreML/Espresso model           |     8 |
| Insta360 Studio 5.9.10 | Ace Pro 2, Luna, and X5 I-Log-to-Rec.709 `.cube` LUTs                         |     3 |
| Insta360 Studio 5.9.10 | `data/sharpen_param.json`                                                     |     1 |
| Insta360 Studio 5.9.10 | `data/0e38bc62.xml` and `data/616ea2f7.xml` accessory models                  |     2 |

Studio resources were copied from `/Applications/Insta360 Studio.app/Contents/`.
The three LUT filenames are `AcePro2_I-Log_To_Rec.709_V1.0.cube`,
`Luna_I-Log_to_Rec709.cube`, and `X5_I-Log_To_Rec.709_V1.0.cube`, originally
under `data/i_log/`.

All copied files retain their original bytes and line endings. Linux SDK XML
copies were selected to avoid duplicate Windows CRLF versions. Identical SDK
platform copies and Studio XMLs already present in the SDK are stored once;
Studio's `761dd84e.xml.xml` matches the SDK's `761dd84e.xml` after line-ending
normalization. The manifest records `identity` normalization for every stored
payload, and each source digest equals its stored digest.

No vendor source, shader, header, executable, or runtime library was copied. The
CoreML resources are compiled model data. SDK sample media, Studio UI resources,
and Studio's older or redundant compiled model variants are outside this bundle.

## Verification and licensing

Every copied payload was compared against its original source bytes. Bundled
provider tests verify all manifest lengths and hashes, parse the SVM XMLs, and
require the complete eight-member seam model. Each data crate includes its own
subset of this manifest and its `assets/**` directory. Tests require the two
subset inventories to match the library manifest exactly. `BundledAssetProvider`
serves their embedded bytes directly at runtime. See [packaging](packaging.md)
for the split.

The original resources retain their vendor licensing. The project's Apache-2.0
license covers project-authored code and does not relicense these copied assets.
See [`NOTICE.md`](../NOTICE.md).
