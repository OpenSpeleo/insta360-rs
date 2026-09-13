//! Underwater tensor contracts over the shared independent MNN adapter.
#[cfg(test)]
use crate::mnn::decode_model;
pub(super) use crate::mnn::runtime_version;
use crate::Result;

#[derive(Debug)]
pub(super) struct Model {
    inner: crate::mnn::Model,
    id: u32,
}
impl Model {
    pub(super) fn open_with_threads(original: &[u8], id: u32, cpu_threads: usize) -> Result<Self> {
        if !matches!(id, 197 | 198) {
            return Err(crate::Error::InvalidMedia(
                "unknown underwater model identity".into(),
            ));
        }
        Ok(Self {
            inner: crate::mnn::Model::open_with_threads(original, id, cpu_threads)?,
            id,
        })
    }

    #[cfg(test)]
    pub(super) fn open(original: &[u8], id: u32) -> Result<Self> {
        Self::open_with_threads(original, id, 1)
    }
    #[cfg(test)]
    pub(super) fn actual_threads(&self) -> Result<usize> {
        self.inner.actual_threads()
    }

    #[cfg(test)]
    pub(super) fn allocation_identity(&self) -> usize {
        self.inner.allocation_identity()
    }
    pub(super) fn run(
        &mut self,
        first: &[f32],
        second: &[f32],
        third: &[f32],
        output: &mut [f32],
    ) -> Result<()> {
        match self.id {
            197 => self.inner.run(&[first, second, third], &mut [output]),
            198 if second.is_empty() && third.is_empty() => self.inner.run(&[first], &mut [output]),
            _ => Err(crate::Error::InvalidMedia(
                "underwater MNN tensor contract mismatch".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_cpu_thread_budgets_fail_before_model_parsing() {
        for threads in [0, 5, usize::MAX] {
            let error = Model::open_with_threads(&[], 197, threads).unwrap_err();
            assert!(error.to_string().contains("CPU thread count"));
        }
    }

    #[test]
    fn linked_runtime_is_the_pinned_mnn_version() {
        assert_eq!(runtime_version().unwrap(), "3.6.1");
    }

    #[test]
    #[ignore = "requires a fresh MNN process; run the isolated shipping check"]
    fn model_varied_tensors_match_complete_reference() {
        // See tests/reference/README.md: generated through a standalone C++
        // Interpreter, without this adapter, with two nonuniform input patterns.
        let bytes = include_bytes!("../../tests/fixtures/underwater-mnn-reference-v1.bin");
        assert_eq!(&bytes[..8], b"MNNREF1\0");
        // MNN's process-global pool never grows after its first creation.
        // Establish its maximum first and verify each actual backend budget.
        for threads in [4, 1, 2, 3] {
            let mut references = bytes[8..]
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()));
            for id in [197, 198] {
                let original = if id == 197 {
                    let mut original = insta360_rs_data_underwater_model_a::PAYLOADS[0].1.to_vec();
                    original.extend_from_slice(insta360_rs_data_underwater_model_b::PAYLOADS[0].1);
                    original
                } else {
                    insta360_rs_data_underwater_resources::PAYLOADS
                        .iter()
                        .find(|(path, _)| *path == "underwater/model198.ins")
                        .unwrap()
                        .1
                        .to_vec()
                };
                let mut model = Model::open_with_threads(&original, id, threads).unwrap();
                assert_eq!(model.actual_threads().unwrap(), threads);
                for pattern in 0..2 {
                    let sample = |input: usize, index: usize| -> f32 {
                        if id == 198 {
                            (((index * if pattern == 0 { 17 } else { 71 } + 23 + pattern * 37)
                                % 257) as i32
                                - 128) as f32
                                / 128.0
                        } else {
                            let factor = match input {
                                0 => pattern + 3,
                                1 => pattern + 7,
                                _ => 13,
                            };
                            let offset = match input {
                                0 => 19,
                                1 => 43,
                                _ => pattern * 31,
                            };
                            ((index * factor + offset) % 257) as f32 / 256.0
                        }
                    };
                    let lengths = if id == 197 {
                        [3 * 17 * 17 * 17, 3 * 256 * 256, 256]
                    } else {
                        [3 * 224 * 224, 0, 0]
                    };
                    let inputs: [Vec<f32>; 3] = std::array::from_fn(|input| {
                        (0..lengths[input])
                            .map(|index| sample(input, index))
                            .collect()
                    });
                    let mut actual = vec![0.0; if id == 197 { 3 * 17 * 17 * 17 } else { 576 }];
                    model
                        .run(&inputs[0], &inputs[1], &inputs[2], &mut actual)
                        .unwrap();
                    for (index, value) in actual.into_iter().enumerate() {
                        let expected = references.next().expect("complete reference tensor");
                        let tolerance = 0.0005 + expected.abs() * 0.0005;
                        assert!(
                            (value - expected).abs() <= tolerance,
                            "model {id}, threads {threads}, pattern {pattern}, element {index}: {value} vs {expected}"
                        );
                    }
                }
            }
            assert!(
                references.next().is_none(),
                "no unconsumed reference elements"
            );
        }
    }

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
