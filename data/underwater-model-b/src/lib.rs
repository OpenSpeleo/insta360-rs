//! Byte-preserving Insta360 Studio underwater restoration resources.

#![no_std]
#![forbid(unsafe_code)]

/// Source provenance and integrity descriptors.
pub const MANIFEST: &str = include_str!("../model-bundle.json");

/// Immutable original payloads, indexed by stable resource paths.
pub const PAYLOADS: &[(&str, &[u8])] = &[(
    "underwater/model197.ins.part1",
    include_bytes!("../assets/underwater/model197.ins.part1"),
)];
