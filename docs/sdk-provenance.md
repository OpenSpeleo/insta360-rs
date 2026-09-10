# Literal SDK and Studio copy inventory

## Scope

This inventory records original files copied byte-for-byte from the licensed
Insta360 SDK and installed Insta360 Studio into `data/*/assets/`. The split
model is stored as contiguous original-byte parts so each publishable crate
remains below its archive budget. Reimplemented behavior, inferred formats,
constants, and static-analysis evidence are outside this inventory.

## Copied resources

The five data crates include **51 stored payloads from 50 original files,
totaling 46,131,982 bytes** (about 44 MiB). The complete per-file inventory is
[`src-rust/assets/model-bundle.json`](../src-rust/assets/model-bundle.json),
including logical provider path, role, source release, original source path,
byte length, and source/stored SHA-256 digests. Vendor source paths are relative
to the supplied distribution root. Paths beginning with `Insta360 Studio.app/`
identify the application bundle.

| Source release         | Original resources                                                                                                                      | Files |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ----: |
| MediaSDK 3.1.5, Linux  | AI stitch, ColorPlus, deflicker, two defringe, and JPEG denoise `.ins` models                                                           |     6 |
| MediaSDK 3.1.5, Linux  | Camera accessory and cooling-shell OpenCV XML models                                                                                    |    10 |
| CameraSDK 2.1.8, Linux | `camera_conf_Insta360_*.json` camera configuration files                                                                                |    10 |
| iOS SDK 1.10.4         | `INSCoreMedia.framework/model_info.json` catalog                                                                                        |     1 |
| iOS SDK 1.10.4         | Complete `model_export_v22_d7c49187.mlmodelc` CoreML/Espresso model                                                                     |     8 |
| Insta360 Studio 5.9.10 | Ace Pro 2, Luna, and X5 I-Log-to-Rec.709 `.cube` LUTs                                                                                   |     3 |
| Insta360 Studio 5.9.10 | `data/sharpen_param.json`                                                                                                               |     1 |
| Insta360 Studio 5.9.10 | Two underwater neural models (one split into two original-byte parts), legacy ILUT, vector database, style manifest and four style PNGs |     9 |
| Insta360 Studio 5.9.10 | `data/0e38bc62.xml` and `data/616ea2f7.xml` accessory models                                                                            |     2 |

Studio resources were copied from `Insta360 Studio.app/Contents/`. The three LUT
filenames are `AcePro2_I-Log_To_Rec.709_V1.0.cube`, `Luna_I-Log_to_Rec709.cube`,
and `X5_I-Log_To_Rec.709_V1.0.cube`, originally under `data/i_log/`.

All copied files retain their original bytes and line endings. Linux SDK XML
copies were selected to avoid duplicate Windows CRLF versions. Identical SDK
platform copies and Studio XMLs already present in the SDK are stored once;
Studio's `761dd84e.xml.xml` matches the SDK's `761dd84e.xml` after line-ending
normalization. Whole-file payloads use `identity` normalization and equal
source/stored hashes. The 15,009,214-byte neural preset model uses two
contiguous original-byte ranges (0–8,000,000 and 8,000,000–15,009,214); both
entries retain the whole-source hash. Tests verify individual parts, ordered
contiguous coverage and the reassembled original SHA-256. Model decryption
occurs only in memory at explicit AI startup.

No vendor source, shader, header, executable, or runtime library was copied. The
CoreML resources are compiled model data. SDK sample media, Studio UI resources,
and Studio's older or redundant compiled model variants are outside this bundle.

## Verification and licensing

Every copied payload was compared against its original source bytes. Bundled
provider tests verify all manifest lengths and hashes, parse the SVM XMLs, and
require complete eight-member seam and nine-member underwater AI groups. Each
data crate includes its own subset of this manifest and its `assets/**`
directory. Tests require the five subset inventories to match the library
manifest exactly. `BundledAssetProvider` serves their embedded bytes directly at
runtime. See [packaging](packaging.md) for the split.

The original resources retain their vendor licensing. The project's Apache-2.0
license covers project-authored code and does not relicense these copied assets.
See [`NOTICE.md`](../NOTICE.md).
