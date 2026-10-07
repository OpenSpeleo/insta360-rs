//! Verified model decoding and ownership of the independent MNN CPU runtime.
use crate::{assets::sha256, Error, Result};
use aes::{
    cipher::{BlockCipherDecrypt, KeyInit},
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

/// Only audited identities may cross the native model parsing boundary.
pub(crate) fn decode_model(original: &[u8], id: u32) -> Result<Vec<u8>> {
    let (length, original_hash, decoded_hash) = match id {
        197 => (
            15_009_214,
            "53a24a86a41673adfbb56709cda53ec16584801d8918ad5ee0306f5027a92d5e",
            "67bd7fa68fdc488b9cd139a850ced8b59107745023f110896f6859f193a98736",
        ),
        198 => (
            7_574_990,
            "0d9998552303ed52e7da489e126d6ac0884e7990fda05c14ae0e7ebb62afaf52",
            "e1b7c4189166c1bf4b451146c87d295d12850fc069f01976b5ef9aa077d19bad",
        ),
        213 => (
            4_002_162,
            "a855aa2106101edd5c28ed0922d21223c2e420c3520917cbd7d371b7437e0cce",
            "2fc715cb0b4a8de1b079d74079039cfa2f32e22b4db182e7cff0014d603257cf",
        ),
        _ => return Err(Error::InvalidMedia("unknown MNN model identity".into())),
    };
    if original.len() != length
        || sha256(original).to_hex() != original_hash
        || original[0] != 0
        || original[length - 1] != 0
    {
        return Err(Error::InvalidMedia(format!(
            "MNN model {id} original resource identity mismatch"
        )));
    }
    // V1 encrypts only the first and last 2000 bytes. Preserve original assets;
    // decode into job-owned memory and verify the result before native parsing.
    let mut decoded = original[1..length - 1].to_vec();
    let cipher = Aes256::new((&WRAPPER_KEY).into());
    let end = decoded.len();
    for range in [0..2000, end - 2000..end] {
        for block in decoded[range].as_chunks_mut::<16>().0 {
            cipher.decrypt_block(block.into());
        }
    }
    if sha256(&decoded).to_hex() != decoded_hash {
        return Err(Error::InvalidMedia(format!(
            "MNN model {id} decoded resource identity mismatch"
        )));
    }
    Ok(decoded)
}

unsafe extern "C" {
    fn insta360_mnn_version() -> *const c_char;
    fn insta360_mnn_create(
        bytes: *const u8,
        length: usize,
        id: c_int,
        cpu_threads: c_int,
        error: *mut c_char,
        capacity: usize,
    ) -> *mut c_void;
    fn insta360_mnn_run(
        native: *mut c_void,
        inputs: *const *const f32,
        input_lengths: *const usize,
        input_count: usize,
        outputs: *const *mut f32,
        output_lengths: *const usize,
        output_count: usize,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    #[cfg(all(test, feature = "underwater-ai"))]
    fn insta360_mnn_actual_threads(native: *mut c_void) -> c_int;
    fn insta360_mnn_destroy(native: *mut c_void);
}

pub(crate) fn runtime_version() -> Result<&'static str> {
    // SAFETY: the linked independent runtime returns immutable static text.
    let pointer = unsafe { insta360_mnn_version() };
    if pointer.is_null() {
        return Err(Error::Media("MNN returned no runtime version".into()));
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map_err(|_| Error::Media("MNN returned an invalid runtime version".into()))
}

#[derive(Debug)]
pub(crate) struct Model {
    native: NonNull<c_void>,
    id: u32,
}
// A session belongs to one job; mutable access prevents concurrent inference.
unsafe impl Send for Model {}

fn failure(bytes: &[c_char; 512]) -> Error {
    // SAFETY: zero-initialized buffer; the C shim terminates within capacity.
    let message = unsafe { CStr::from_ptr(bytes.as_ptr()) }.to_string_lossy();
    Error::Media(format!("independent MNN: {message}"))
}

impl Model {
    #[cfg(feature = "ai-stitching")]
    pub(crate) fn open(original: &[u8], id: u32) -> Result<Self> {
        Self::open_with_threads(original, id, 1)
    }

    /// An explicit, bounded CPU budget; precision and tensor contracts are fixed.
    pub(crate) fn open_with_threads(original: &[u8], id: u32, cpu_threads: usize) -> Result<Self> {
        if !(1..=4).contains(&cpu_threads) {
            return Err(Error::InvalidMedia(
                "MNN CPU thread count must be in 1..=4".into(),
            ));
        }
        if runtime_version()? != "3.6.1" {
            return Err(Error::Media("independent MNN must be version 3.6.1".into()));
        }
        let decoded = decode_model(original, id)?;
        let mut error = [0; 512];
        // SAFETY: create copies model data and retains no Rust buffer pointers.
        let native = unsafe {
            insta360_mnn_create(
                decoded.as_ptr(),
                decoded.len(),
                id as c_int,
                cpu_threads as c_int,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        Ok(Self {
            native: NonNull::new(native).ok_or_else(|| failure(&error))?,
            id,
        })
    }

    #[cfg(all(test, feature = "underwater-ai"))]
    pub(crate) fn actual_threads(&self) -> Result<usize> {
        // SAFETY: the exclusively owned model remains alive during this query.
        let threads = unsafe { insta360_mnn_actual_threads(self.native.as_ptr()) };
        if !(1..=4).contains(&threads) {
            return Err(Error::Media(
                "MNN did not report a valid CPU thread count".into(),
            ));
        }
        Ok(threads as usize)
    }

    #[cfg(test)]
    pub(crate) fn allocation_identity(&self) -> usize {
        self.native.as_ptr() as usize
    }

    pub(crate) fn run(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) -> Result<()> {
        let (expected_inputs, expected_outputs): (&[usize], &[usize]) = match self.id {
            197 => (&[3 * 17 * 17 * 17, 3 * 256 * 256, 256], &[3 * 17 * 17 * 17]),
            198 => (&[3 * 224 * 224], &[576]),
            213 => (
                &[3 * 544 * 64, 3 * 544 * 64, 544 * 64, 544 * 64],
                &[2 * 136 * 16, 2 * 136 * 16],
            ),
            _ => unreachable!("verified model identity"),
        };
        if inputs.len() != expected_inputs.len()
            || outputs.len() != expected_outputs.len()
            || inputs.iter().zip(expected_inputs).any(|(input, length)| {
                input.len() != *length || input.iter().any(|value| !value.is_finite())
            })
            || outputs
                .iter()
                .zip(expected_outputs)
                .any(|(output, length)| output.len() != *length)
        {
            return Err(Error::InvalidMedia(
                "MNN tensor length or finite-value contract mismatch".into(),
            ));
        }
        let input_pointers: [*const f32; 4] =
            std::array::from_fn(|i| inputs.get(i).map_or(std::ptr::null(), |v| v.as_ptr()));
        let output_pointers: [*mut f32; 2] = std::array::from_fn(|i| {
            outputs
                .get_mut(i)
                .map_or(std::ptr::null_mut(), |v| v.as_mut_ptr())
        });
        let mut error = [0; 512];
        // SAFETY: all tensors match the verified contract. The shim copies
        // inputs, bounds outputs, catches exceptions and retains no pointers.
        let status = unsafe {
            insta360_mnn_run(
                self.native.as_ptr(),
                input_pointers.as_ptr(),
                expected_inputs.as_ptr(),
                inputs.len(),
                output_pointers.as_ptr(),
                expected_outputs.as_ptr(),
                outputs.len(),
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
        // SAFETY: this is the uniquely owned pointer returned by create.
        unsafe {
            insta360_mnn_destroy(self.native.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original_model(id: u32) -> Vec<u8> {
        match id {
            197 => [
                insta360_rs_data_underwater_model_a::PAYLOADS[0].1,
                insta360_rs_data_underwater_model_b::PAYLOADS[0].1,
            ]
            .concat(),
            198 => insta360_rs_data_underwater_resources::PAYLOADS
                .iter()
                .find(|(path, _)| *path == "underwater/model198.ins")
                .unwrap()
                .1
                .to_vec(),
            213 => insta360_rs_data_ai_stitch_video::PAYLOADS
                .iter()
                .find(|(path, _)| *path == "models/ai-seam-studio-video-213/model.ins")
                .unwrap()
                .1
                .to_vec(),
            _ => unreachable!("test fixture must be an audited model"),
        }
    }

    fn assert_decoded_model(id: u32, expected_length: usize, expected_hash: &str) {
        let original = original_model(id);
        let original_hash = sha256(&original);
        let decoded = decode_model(&original, id).unwrap();
        assert_eq!(decoded.len(), expected_length, "model {id}");
        // Audited plaintext identities, not an encrypt/decrypt round trip using
        // the same implementation. This checks the key, AES mode and both edges.
        assert_eq!(sha256(&decoded).to_hex(), expected_hash, "model {id}");
        assert_eq!(
            sha256(&original),
            original_hash,
            "source model {id} changed"
        );
        let payload = &original[1..original.len() - 1];
        let tail = payload.len() - 2000;
        assert_ne!(&decoded[..2000], &payload[..2000], "model {id} head");
        assert_ne!(&decoded[tail..], &payload[tail..], "model {id} tail");
        assert_eq!(
            &decoded[2000..tail],
            &payload[2000..tail],
            "model {id} plaintext body changed"
        );
    }

    #[test]
    fn decodes_split_underwater_model_197_to_audited_plaintext() {
        assert_decoded_model(
            197,
            15_009_212,
            "67bd7fa68fdc488b9cd139a850ced8b59107745023f110896f6859f193a98736",
        );
    }

    #[test]
    fn decodes_underwater_model_198_to_audited_plaintext() {
        assert_decoded_model(
            198,
            7_574_988,
            "e1b7c4189166c1bf4b451146c87d295d12850fc069f01976b5ef9aa077d19bad",
        );
    }

    #[test]
    fn decodes_seam_model_213_to_audited_plaintext() {
        assert_decoded_model(
            213,
            4_002_160,
            "2fc715cb0b4a8de1b079d74079039cfa2f32e22b4db182e7cff0014d603257cf",
        );
    }

    fn assert_identity_rejected(original: &[u8], id: u32) {
        assert!(
            matches!(decode_model(original, id), Err(Error::InvalidMedia(message))
                if message == format!("MNN model {id} original resource identity mismatch")),
            "model {id} must reject altered original bytes before decryption"
        );
    }

    #[test]
    fn rejects_unknown_and_mismatched_model_identities() {
        let original = original_model(198);
        for id in [0, 196, 199, 214, u32::MAX] {
            assert!(
                matches!(decode_model(&original, id), Err(Error::InvalidMedia(message))
                    if message == "unknown MNN model identity"),
                "unknown model {id}"
            );
        }
        for id in [197, 213] {
            assert_identity_rejected(&original, id);
        }
    }

    #[test]
    fn rejects_truncated_and_extended_models_without_panicking() {
        for id in [197, 198, 213] {
            let mut original = original_model(id);
            for length in [0, 1, 16, 2001, original.len() - 1] {
                assert_identity_rejected(&original[..length], id);
            }
            original.push(0);
            assert_identity_rejected(&original, id);
        }
    }

    #[test]
    fn rejects_corruption_in_wrapper_encrypted_edges_and_plaintext_body() {
        for id in [197, 198, 213] {
            let mut original = original_model(id);
            let length = original.len();
            // Include both wrapper markers and both sides of each boundary
            // between encrypted bytes and the untouched plaintext body.
            for index in [
                0,
                1,
                2000,
                2001,
                length / 2,
                length - 2002,
                length - 2001,
                length - 2,
                length - 1,
            ] {
                original[index] ^= 1;
                assert_identity_rejected(&original, id);
                original[index] ^= 1;
            }
        }
    }
}
