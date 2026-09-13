//! Original licensed Insta360 Studio video seam resources.
#![no_std]

/// Original-source provenance and integrity metadata.
pub const MANIFEST: &str = include_str!("../model-bundle.json");

/// Byte-preserving licensed resources indexed by provider path.
pub const PAYLOADS: &[(&str, &[u8])] = &[
    (
        "models/ai-seam-studio-video-213/model.ins",
        include_bytes!("../assets/models/ai-seam-studio-video-213/model.ins"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/analytics/coremldata.bin",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/analytics/coremldata.bin"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/coremldata.bin",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/coremldata.bin"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/ins_metadata.json",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/ins_metadata.json"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/ins_model.mil",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/ins_model.mil"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/metadata.json",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/metadata.json"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/model.mil",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/model.mil"),
    ),
    (
        "models/ai-seam-studio-video-213/coreml/weights/weight.bin",
        include_bytes!("../assets/models/ai-seam-studio-video-213/coreml/weights/weight.bin"),
    ),
];
