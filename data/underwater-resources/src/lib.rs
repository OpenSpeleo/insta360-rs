//! Byte-preserving Insta360 Studio underwater restoration resources.

#![no_std]
#![forbid(unsafe_code)]

/// Source provenance and integrity descriptors.
pub const MANIFEST: &str = include_str!("../model-bundle.json");

/// Immutable original payloads, indexed by stable resource paths.
pub const PAYLOADS: &[(&str, &[u8])] = &[
    (
        "underwater/model198.ins",
        include_bytes!("../assets/underwater/model198.ins"),
    ),
    (
        "underwater/underwater.ilut",
        include_bytes!("../assets/underwater/underwater.ilut"),
    ),
    (
        "underwater/style-database.bin",
        include_bytes!("../assets/underwater/style-database.bin"),
    ),
    (
        "underwater/styles/result.json",
        include_bytes!("../assets/underwater/styles/result.json"),
    ),
    (
        "underwater/styles/style0.png",
        include_bytes!("../assets/underwater/styles/style0.png"),
    ),
    (
        "underwater/styles/style1.png",
        include_bytes!("../assets/underwater/styles/style1.png"),
    ),
    (
        "underwater/styles/style2.png",
        include_bytes!("../assets/underwater/styles/style2.png"),
    ),
    (
        "underwater/styles/style3.png",
        include_bytes!("../assets/underwater/styles/style3.png"),
    ),
];
