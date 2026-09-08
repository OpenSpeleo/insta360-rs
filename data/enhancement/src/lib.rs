//! Original Insta360 enhancement resource bytes for `insta360-rs`.
//!
//! Data availability does not imply an implemented or qualified inference pipeline.

#![no_std]
#![forbid(unsafe_code)]

/// Source provenance and integrity descriptors for this data crate.
pub const MANIFEST: &str = include_str!("../model-bundle.json");

/// Original payloads indexed by the stable paths used by `insta360-rs`.
pub const PAYLOADS: &[(&str, &[u8])] = &[
    (
        "models/colorplus_model.ins",
        include_bytes!("../assets/models/colorplus_model.ins"),
    ),
    (
        "models/deflicker_86ccba0d.ins",
        include_bytes!("../assets/models/deflicker_86ccba0d.ins"),
    ),
    (
        "models/defringe_air_hr_dynamic_6fbc2886.ins",
        include_bytes!("../assets/models/defringe_air_hr_dynamic_6fbc2886.ins"),
    ),
    (
        "models/defringe_hr_dynamic_7b56e80f.ins",
        include_bytes!("../assets/models/defringe_hr_dynamic_7b56e80f.ins"),
    ),
    (
        "models/jpg_denoise_9d006262.ins",
        include_bytes!("../assets/models/jpg_denoise_9d006262.ins"),
    ),
];
