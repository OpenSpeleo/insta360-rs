# insta360-rs-data-underwater-model-a

Original Insta360 Studio 5.9.10 data for the optional underwater restoration
pipeline in `insta360-rs`. The dependency-free `no_std` crate exposes immutable
`PAYLOADS` and a source provenance `MANIFEST`.

Model 197 is split at byte 8,000,000 across the model-a and model-b crates to
keep each compressed archive below 10,000,000 bytes. Concatenating the chunks
restores the original file byte for byte. The parent library verifies both chunk
digests and the complete original digest before decoding it.

Data availability alone does not qualify an inference or restoration pipeline.
Project-authored Rust uses Apache-2.0; original vendor data retains its
licensing. See [LICENSE.md](LICENSE.md), [NOTICE.md](NOTICE.md), and
[model-bundle.json](model-bundle.json).
