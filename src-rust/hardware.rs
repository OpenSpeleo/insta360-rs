//! Runtime codec capabilities shared by media hosts and paired preview decoders.

use ffmpeg_next as ffmpeg;

/// Whether this host supports VideoToolbox hardware decoding for the codec.
///
/// `codec_tag` is FFmpeg's little-endian container tag (used for ProRes variants).
/// Unknown codecs and non-macOS hosts return false. A true result is a preflight,
/// not a guarantee that a particular stream or available resources can decode;
/// callers must retain their software fallback.
pub fn videotoolbox_decode_supported(codec: ffmpeg::codec::Id, codec_tag: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        videotoolbox_supported(codec, codec_tag, |codec_type| {
            // SAFETY: this read-only API takes a codec value, not a pointer.
            unsafe { VTIsHardwareDecodeSupported(codec_type) != 0 }
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (codec, codec_tag);
        false
    }
}

#[cfg(target_os = "macos")]
#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    // Apple's Boolean is unsigned char, and CMVideoCodecType is UInt32.
    fn VTIsHardwareDecodeSupported(codec_type: u32) -> u8;
}

#[cfg(any(target_os = "macos", test))]
fn videotoolbox_supported(
    codec: ffmpeg::codec::Id,
    codec_tag: u32,
    query: impl FnOnce(u32) -> bool,
) -> bool {
    use ffmpeg::codec::Id;
    // Use codec IDs rather than container aliases such as avc3/hev1. FFmpeg's
    // ProRes tags are little-endian; Apple's codec constants are big-endian.
    let tag = match codec {
        Id::H263 => *b"h263",
        Id::H264 => *b"avc1",
        Id::HEVC => *b"hvc1",
        Id::MPEG1VIDEO => *b"mp1v",
        Id::MPEG2VIDEO => *b"mp2v",
        Id::MPEG4 => *b"mp4v",
        Id::VP9 => *b"vp09",
        Id::AV1 => *b"av01",
        Id::PRORES => match &codec_tag.to_le_bytes() {
            tag @ (b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x") => *tag,
            _ => return false,
        },
        Id::PRORES_RAW => match &codec_tag.to_le_bytes() {
            tag @ (b"aprn" | b"aprh") => *tag,
            _ => return false,
        },
        _ => return false,
    };
    query(u32::from_be_bytes(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn videotoolbox_checks_runtime_support_with_canonical_codec_types() {
        use ffmpeg::codec::Id;
        assert!(!videotoolbox_supported(Id::MPEG4, 0, |codec_type| {
            assert_eq!(codec_type, u32::from_be_bytes(*b"mp4v"));
            false
        }));
        for (codec, alias, canonical) in [
            (Id::H264, *b"avc3", *b"avc1"),
            (Id::HEVC, *b"hev1", *b"hvc1"),
            (Id::H263, *b"h263", *b"h263"),
            (Id::MPEG1VIDEO, *b"mp1v", *b"mp1v"),
            (Id::MPEG2VIDEO, *b"mp2v", *b"mp2v"),
            (Id::VP9, *b"vp09", *b"vp09"),
            (Id::AV1, *b"av01", *b"av01"),
        ] {
            for supported in [false, true] {
                assert_eq!(
                    videotoolbox_supported(codec, u32::from_le_bytes(alias), |codec_type| {
                        assert_eq!(codec_type, u32::from_be_bytes(canonical));
                        supported
                    }),
                    supported
                );
            }
        }
        assert!(!videotoolbox_supported(Id::FFV1, 0, |_| {
            panic!("unknown codecs must not be probed")
        }));
    }

    #[test]
    fn videotoolbox_validates_and_converts_prores_container_tags() {
        use ffmpeg::codec::Id;
        for (codec, tags) in [
            (
                Id::PRORES,
                [*b"apco", *b"apcs", *b"apcn", *b"apch", *b"ap4h", *b"ap4x"].as_slice(),
            ),
            (Id::PRORES_RAW, [*b"aprn", *b"aprh"].as_slice()),
        ] {
            for tag in tags {
                assert!(videotoolbox_supported(
                    codec,
                    u32::from_le_bytes(*tag),
                    |codec_type| {
                        assert_eq!(codec_type, u32::from_be_bytes(*tag));
                        true
                    }
                ));
            }
            assert!(!videotoolbox_supported(codec, 0, |_| {
                panic!("invalid ProRes tags must not be probed")
            }));
        }
    }
}
