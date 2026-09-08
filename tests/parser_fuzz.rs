use std::io::Cursor;

use insta360_rs::InsvReader;

#[test]
fn bounded_parser_rejects_deterministic_fuzz_corpus_without_panicking() {
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    for length in (0..=4_096).step_by(17) {
        let mut data = vec![0_u8; length];
        for byte in &mut data {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        if length >= 8 {
            data[..4].copy_from_slice(&(length as u32).to_be_bytes());
            data[4..8].copy_from_slice(b"ftyp");
        }
        let mut reader = InsvReader::new(Cursor::new(data)).expect("cursor is seekable");
        let _ = reader.inspect();
    }
}
