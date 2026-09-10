//! Verified model wrapper decoding and the private MNN ownership boundary.
use crate::{assets::sha256, Error, Result};
use aes::{
    cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit},
    Aes256,
};
use std::{
    ffi::{c_char, c_int, c_void, CStr},
    ptr::NonNull,
};

const WRAPPER_KEY: [u8; 32] = [
    57, 98, 117, 97, 113, 109, 112, 99, 48, 116, 51, 98, 48, 53, 106, 119, 52, 121, 120, 117, 106,
    50, 118, 103, 115, 105, 107, 118, 106, 118, 97, 50,
];
const MODEL197_DECODED: &str = "67bd7fa68fdc488b9cd139a850ced8b59107745023f110896f6859f193a98736";
const MODEL198_DECODED: &str = "e1b7c4189166c1bf4b451146c87d295d12850fc069f01976b5ef9aa077d19bad";

// The V1 wrapper encrypts first and last 2000 bytes using AES-256 ECB;
// byte zero is padding length and the final byte selects key zero. Only
// the two verified complete models are accepted, before native parsing.
pub(super) fn decode_model(original: &[u8], id: u32) -> Result<Vec<u8>> {
    let (length, original_hash, decoded_hash) = match id {
        197 => (
            15_009_214,
            "53a24a86a41673adfbb56709cda53ec16584801d8918ad5ee0306f5027a92d5e",
            MODEL197_DECODED,
        ),
        198 => (
            7_574_990,
            "0d9998552303ed52e7da489e126d6ac0884e7990fda05c14ae0e7ebb62afaf52",
            MODEL198_DECODED,
        ),
        _ => {
            return Err(Error::InvalidMedia(
                "unknown underwater model identity".into(),
            ))
        }
    };
    if original.len() != length
        || sha256(original).to_hex() != original_hash
        || original[0] != 0
        || original[length - 1] != 0
    {
        return Err(Error::InvalidMedia(format!(
            "underwater model {id} original resource identity mismatch"
        )));
    }
    let mut decoded = original[1..length - 1].to_vec();
    let cipher = Aes256::new(GenericArray::from_slice(&WRAPPER_KEY));
    let end = decoded.len();
    for range in [0..2000, end - 2000..end] {
        for block in decoded[range].chunks_exact_mut(16) {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
    }
    if sha256(&decoded).to_hex() != decoded_hash {
        return Err(Error::InvalidMedia(format!(
            "underwater model {id} decoded resource identity mismatch"
        )));
    }
    Ok(decoded)
}

unsafe extern "C" {
    fn insta360_mnn_create(
        bytes: *const u8,
        length: usize,
        id: c_int,
        error: *mut c_char,
        capacity: usize,
    ) -> *mut c_void;
    fn insta360_mnn_run(
        session: *mut c_void,
        first: *const f32,
        first_length: usize,
        second: *const f32,
        second_length: usize,
        third: *const f32,
        third_length: usize,
        output: *mut f32,
        output_length: usize,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    fn insta360_mnn_destroy(session: *mut c_void);
}

pub(super) struct Model {
    native: NonNull<c_void>,
    id: u32,
}
// A session is owned by one job. MNN CPU sessions may move between threads;
// inference and buffer access require &mut self and are never concurrent.
unsafe impl Send for Model {}
impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MnnModel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
fn failure(bytes: &[c_char; 512]) -> Error {
    // SAFETY: the buffer is initialized to zero and the C shim always terminates
    // messages within capacity, including catch-all exception paths.
    let message = unsafe { CStr::from_ptr(bytes.as_ptr()) }.to_string_lossy();
    Error::Media(format!("underwater MNN: {message}"))
}
impl Model {
    pub(super) fn open(original: &[u8], id: u32) -> Result<Self> {
        let decoded = decode_model(original, id)?;
        let mut error = [0; 512];
        // SAFETY: byte slice remains live throughout creation; the independently
        // built shim copies model data and retains no Rust buffer pointers.
        let native = unsafe {
            insta360_mnn_create(
                decoded.as_ptr(),
                decoded.len(),
                id as c_int,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let native = NonNull::new(native).ok_or_else(|| failure(&error))?;
        Ok(Self { native, id })
    }
    pub(super) fn run(
        &mut self,
        first: &[f32],
        second: &[f32],
        third: &[f32],
        output: &mut [f32],
    ) -> Result<()> {
        let lengths = if self.id == 197 {
            [3 * 17 * 17 * 17, 3 * 256 * 256, 256, 3 * 17 * 17 * 17]
        } else {
            [3 * 224 * 224, 0, 0, 576]
        };
        if [first.len(), second.len(), third.len(), output.len()] != lengths
            || first
                .iter()
                .chain(second)
                .chain(third)
                .any(|v| !v.is_finite())
        {
            return Err(Error::InvalidMedia(
                "underwater MNN tensor length or finite-value contract mismatch".into(),
            ));
        }
        let mut error = [0; 512];
        // SAFETY: live slices have the verified lengths; the shim copies inputs,
        // bounds output by length, catches exceptions, and retains no pointers.
        let status = unsafe {
            insta360_mnn_run(
                self.native.as_ptr(),
                first.as_ptr(),
                first.len(),
                second.as_ptr(),
                second.len(),
                third.as_ptr(),
                third.len(),
                output.as_mut_ptr(),
                output.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status != 0 {
            return Err(failure(&error));
        }
        Ok(())
    }
}
impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: this is the unique pointer returned by create, destroyed once.
        unsafe {
            insta360_mnn_destroy(self.native.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_tensors_match_independent_mnn_reference_and_reject_invalid_inputs() {
        // Independent C++ MNN 3.6.1 Interpreter reference, CPU Precision_High,
        // one thread, every input float 0.25. The 16 feature indices span the
        // full avgpool tensor; tolerances permit CPU instruction differences.
        let mut original = insta360_rs_data_underwater_model_a::PAYLOADS[0].1.to_vec();
        original.extend_from_slice(insta360_rs_data_underwater_model_b::PAYLOADS[0].1);
        let mut preset = Model::open(&original, 197).unwrap();
        let first = vec![0.25; 3 * 17 * 17 * 17];
        let second = vec![0.25; 3 * 256 * 256];
        let third = [0.25; 256];
        let mut output = vec![0.0; first.len()];
        preset.run(&first, &second, &third, &mut output).unwrap();
        for (plane, expected) in
            output
                .chunks_exact(17 * 17 * 17)
                .zip([0.022_760_032, -0.004_656_446_6, -0.031_643_756])
        {
            assert!(plane.iter().all(|value| (value - expected).abs() < 0.0002));
        }
        output.fill(123.0);
        assert!(preset.run(&[], &second, &third, &mut output).is_err());
        assert!(output.iter().all(|value| *value == 123.0));
        let mut invalid = first.clone();
        invalid[0] = f32::NAN;
        assert!(preset.run(&invalid, &second, &third, &mut output).is_err());
        assert!(output.iter().all(|value| *value == 123.0));

        let original = insta360_rs_data_underwater_resources::PAYLOADS
            .iter()
            .find(|(path, _)| *path == "underwater/model198.ins")
            .unwrap()
            .1;
        let mut extractor = Model::open(original, 198).unwrap();
        let input = vec![0.25; 3 * 224 * 224];
        let mut feature = [0.0; 576];
        let expected = [
            0.084_218_964,
            -0.245_014_92,
            -0.321_125_8,
            -0.358_432_35,
            -0.124_974_8,
            -0.329_651_7,
            0.003_059_230_7,
            0.001_306_184_8,
            0.000_029_762_09,
            -0.004_858_599_5,
            -0.236_726_85,
            -0.318_330_62,
            0.001_876_944_6,
            -0.268_091_77,
            0.138_979_2,
            -0.101_641_26,
        ];
        for _ in 0..3 {
            extractor.run(&input, &[], &[], &mut feature).unwrap();
            for (index, expected) in expected.into_iter().enumerate() {
                assert!((feature[index * 575 / 15] - expected).abs() < 0.0002);
            }
        }
    }
    #[test]
    fn model_wrapper_rejects_unknown_truncated_and_corrupt_input() {
        for id in [0, 196, 197, 198, 199] {
            assert!(decode_model(&[], id).is_err());
        }
        let mut bytes = insta360_rs_data_underwater_resources::PAYLOADS[0]
            .1
            .to_vec();
        bytes[4] ^= 1;
        assert!(decode_model(&bytes, 198).is_err());
    }
}
