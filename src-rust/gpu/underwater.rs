//! GPU pixel stages over an exclusively borrowed CPU neural color session.
use super::*;

const ANALYSIS_SIZE: usize = 224;
const ANALYSIS_BYTES: u64 = (ANALYSIS_SIZE * ANALYSIS_SIZE * 3) as u64;
const LUT_ENTRIES: usize = 17 * 17 * 17;

/// Internal synchronous frame transaction. A failed/uncommitted neural frame
/// resets its history; this interface does not transfer model ownership.
pub(crate) trait GpuUnderwaterProcessor {
    fn dimensions(&self) -> (u32, u32);
    fn needs_analysis(&self) -> bool;
    fn update(&mut self, rgb224: &[u8]) -> Result<()>;
    fn lut(&self) -> &[[u8; 3]];
    fn commit(&mut self);
}

pub(super) struct UnderwaterPipelines {
    analysis: wgpu::ComputePipeline,
    apply: wgpu::ComputePipeline,
}

pub(super) struct UnderwaterResources {
    analysis_bindings: wgpu::BindGroup,
    apply_bindings: wgpu::BindGroup,
    analysis_output: wgpu::Buffer,
    analysis_readback: wgpu::Buffer,
    lut: wgpu::Buffer,
    previous_lut: Vec<[u8; 3]>,
    packed_lut: Vec<u32>,
}

impl UnderwaterPipelines {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("insta360-rs integer underwater shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../underwater_gpu.wgsl").into()),
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &shader,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        Self {
            analysis: pipeline("downsample_rgb24"),
            apply: pipeline("apply_lut"),
        }
    }

    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: &wgpu::Buffer,
        dimensions: (u32, u32),
        adapter: &GpuAdapterInfo,
    ) -> Result<UnderwaterResources> {
        // Publish the cached stage only after both allocation scopes succeed.
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let params = dynamic_buffer(
            device,
            "underwater image size",
            16,
            wgpu::BufferUsages::UNIFORM,
        );
        queue.write_buffer(
            &params,
            0,
            bytemuck::cast_slice(&[dimensions.0, dimensions.1, 0, 0]),
        );
        let values: Vec<[u32; 4]> = [dimensions.0, dimensions.1]
            .into_iter()
            .flat_map(|size| {
                (0..ANALYSIS_SIZE).map(move |index| {
                    let (first, second, a, b) =
                        crate::underwater::resize::weights(index, size as usize, ANALYSIS_SIZE);
                    [first as u32, second as u32, a as u32, b as u32]
                })
            })
            .collect();
        let taps = dynamic_buffer(
            device,
            "underwater exact resize taps",
            (values.len() * 16) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        queue.write_buffer(&taps, 0, bytemuck::cast_slice(&values));
        let analysis_output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("underwater RGB224 analysis"),
            size: ANALYSIS_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let analysis_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("underwater bounded analysis readback"),
            size: ANALYSIS_BYTES,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lut = dynamic_buffer(
            device,
            "underwater integer LUT",
            (LUT_ENTRIES * 4) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let analysis_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("underwater analysis bindings"),
            layout: &self.analysis.get_bind_group_layout(0),
            entries: &[
                binding(0, params.as_entire_binding()),
                binding(1, pixels.as_entire_binding()),
                binding(2, taps.as_entire_binding()),
                binding(3, analysis_output.as_entire_binding()),
            ],
        });
        let apply_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("underwater application bindings"),
            layout: &self.apply.get_bind_group_layout(0),
            entries: &[
                binding(0, params.as_entire_binding()),
                binding(1, pixels.as_entire_binding()),
                binding(4, lut.as_entire_binding()),
            ],
        });
        let resources = UnderwaterResources {
            analysis_bindings,
            apply_bindings,
            analysis_output,
            analysis_readback,
            lut,
            previous_lut: Vec::with_capacity(LUT_ENTRIES),
            packed_lut: vec![0; LUT_ENTRIES],
        };
        let validation_error = pollster::block_on(validation.pop());
        let out_of_memory_error = pollster::block_on(out_of_memory.pop());
        if let Some(error) = out_of_memory_error {
            return Err(gpu_unavailable(
                GpuFailureCode::OutOfMemory,
                GpuFailureStage::Preparation,
                format!("allocating underwater GPU resources failed: {error}"),
                Some(adapter.clone()),
            ));
        }
        if let Some(error) = validation_error {
            return Err(gpu_unavailable(
                GpuFailureCode::UnsupportedLimits,
                GpuFailureStage::Preparation,
                format!("validating underwater GPU resources failed: {error}"),
                Some(adapter.clone()),
            ));
        }
        Ok(resources)
    }
}

impl GpuStitcher {
    pub(super) fn process_underwater(
        &self,
        resources: &mut GpuFrameResources,
        mut encoder: wgpu::CommandEncoder,
        processor: &mut dyn GpuUnderwaterProcessor,
    ) -> Result<wgpu::CommandEncoder> {
        let dimensions = (resources.key.output_width, resources.key.output_height);
        if resources.underwater.is_none() {
            resources.underwater = Some(self.underwater_pipelines.prepare(
                &self.device,
                &self.queue,
                &resources._output_buffer,
                dimensions,
                &self.adapter,
            )?);
        }
        let stage = resources
            .underwater
            .as_mut()
            .expect("underwater resources prepared");
        if processor.needs_analysis() {
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("underwater exact analysis resize"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.underwater_pipelines.analysis);
                pass.set_bind_group(0, &stage.analysis_bindings, &[]);
                pass.dispatch_workgroups(
                    (ANALYSIS_SIZE * ANALYSIS_SIZE / 4).div_ceil(64) as u32,
                    1,
                    1,
                );
            }
            encoder.copy_buffer_to_buffer(
                &stage.analysis_output,
                0,
                &stage.analysis_readback,
                0,
                ANALYSIS_BYTES,
            );
            let submission = self.queue.submit([encoder.finish()]);
            self.read_underwater_analysis(&stage.analysis_readback, submission, processor)?;
            encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("underwater application and output commands"),
                });
        }
        let table = processor.lut();
        if table.len() != LUT_ENTRIES {
            return Err(Error::InvalidMedia(
                "GPU underwater LUT must contain 17³ RGB entries".into(),
            ));
        }
        // Comparing 15KB avoids stale epochs when a renderer switches sessions,
        // resets history, or reallocates GPU resources. No frame-sized copy.
        if stage.previous_lut.as_slice() != table {
            for (packed, rgb) in stage.packed_lut.iter_mut().zip(table) {
                *packed = u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16);
            }
            self.queue
                .write_buffer(&stage.lut, 0, bytemuck::cast_slice(&stage.packed_lut));
            stage.previous_lut.clear();
            stage.previous_lut.extend_from_slice(table);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("underwater integer tetrahedral application"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.underwater_pipelines.apply);
            pass.set_bind_group(0, &stage.apply_bindings, &[]);
            pass.dispatch_workgroups(dimensions.0.div_ceil(16), dimensions.1.div_ceil(8), 1);
        }
        Ok(encoder)
    }

    fn read_underwater_analysis(
        &self,
        readback: &wgpu::Buffer,
        submission: wgpu::SubmissionIndex,
        processor: &mut dyn GpuUnderwaterProcessor,
    ) -> Result<()> {
        let result = (|| {
            let slice = readback.slice(..);
            let (sender, receiver) = mpsc::sync_channel(1);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: None,
                })
                .map_err(|error| {
                    gpu_processing(
                        GpuFailureCode::DeviceLost,
                        GpuFailureStage::Readback,
                        format!("waiting for underwater analysis failed: {error}"),
                        &self.adapter,
                    )
                })?;
            receiver
                .recv()
                .map_err(|error| {
                    gpu_processing(
                        GpuFailureCode::Readback,
                        GpuFailureStage::Readback,
                        format!("underwater analysis callback was lost: {error}"),
                        &self.adapter,
                    )
                })?
                .map_err(|error| {
                    gpu_processing(
                        GpuFailureCode::Readback,
                        GpuFailureStage::Readback,
                        format!("mapping underwater analysis failed: {error}"),
                        &self.adapter,
                    )
                })?;
            let mapped = slice.get_mapped_range().map_err(|error| {
                gpu_processing(
                    GpuFailureCode::Readback,
                    GpuFailureStage::Readback,
                    format!("reading underwater analysis failed: {error}"),
                    &self.adapter,
                )
            })?;
            processor.update(&mapped)
        })();
        readback.unmap();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<GpuStitcher> {
        if available_adapters().is_empty() {
            assert!(
                std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_none(),
                "GPU is required"
            );
            return None;
        }
        Some(GpuStitcher::new().expect("GPU renderer"))
    }

    struct Analysis(Vec<u8>);
    impl GpuUnderwaterProcessor for Analysis {
        fn dimensions(&self) -> (u32, u32) {
            (0, 0)
        }
        fn needs_analysis(&self) -> bool {
            true
        }
        fn update(&mut self, image: &[u8]) -> Result<()> {
            self.0 = image.to_vec();
            Ok(())
        }
        fn lut(&self) -> &[[u8; 3]] {
            &[]
        }
        fn commit(&mut self) {}
    }

    #[test]
    fn invalid_stage_allocation_drains_scopes_before_valid_preparation() {
        let Some(gpu) = gpu() else { return };
        let out_of_memory = gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let invalid = dynamic_buffer(
            &gpu.device,
            "invalid three-byte pixel binding",
            3,
            wgpu::BufferUsages::STORAGE,
        );
        let result = gpu.underwater_pipelines.prepare(
            &gpu.device,
            &gpu.queue,
            &invalid,
            (1, 1),
            &gpu.adapter,
        );
        let _ = pollster::block_on(validation.pop());
        let _ = pollster::block_on(out_of_memory.pop());
        assert!(matches!(result, Err(Error::GpuUnavailable(_))));
        let valid = dynamic_buffer(
            &gpu.device,
            "valid four-byte pixel binding",
            4,
            wgpu::BufferUsages::STORAGE,
        );
        gpu.underwater_pipelines
            .prepare(&gpu.device, &gpu.queue, &valid, (1, 1), &gpu.adapter)
            .unwrap();
    }

    struct Transaction {
        table: Vec<[u8; 3]>,
        fail: bool,
        analyze: bool,
        updates: usize,
        commits: usize,
    }
    impl GpuUnderwaterProcessor for Transaction {
        fn dimensions(&self) -> (u32, u32) {
            (16, 8)
        }
        fn needs_analysis(&self) -> bool {
            self.analyze
        }
        fn update(&mut self, image: &[u8]) -> Result<()> {
            assert_eq!(image.len(), ANALYSIS_BYTES as usize);
            self.updates += 1;
            if self.fail {
                Err(Error::Media("test inference failure".into()))
            } else {
                Ok(())
            }
        }
        fn lut(&self) -> &[[u8; 3]] {
            &self.table
        }
        fn commit(&mut self) {
            self.commits += 1;
        }
    }

    #[test]
    fn failed_analysis_evicts_stage_and_retry_commits_once_then_reuses_buffers() {
        let Some(gpu) = gpu() else { return };
        let lenses = [
            LensFrame::new(16, 16, vec![96; 16 * 16 * 3]).unwrap(),
            LensFrame::new(16, 16, vec![112; 16 * 16 * 3]).unwrap(),
        ];
        let calibration = crate::calibration::synthetic_dual_fisheye_calibration(16, 16).unwrap();
        let projection = EquirectangularProjection {
            width: 16,
            height: 8,
        };
        let motion = FrameMotion::global(Orientation::IDENTITY).unwrap();
        let render = |processor: &mut Transaction| {
            gpu.stitch_sources(
                GpuSourceFrames::Rgb(&lenses),
                &calibration,
                projection,
                &motion,
                GpuOutputRequest {
                    kind: GpuOutputKind::Rgb,
                    underwater: Some(processor),
                },
                None,
            )
        };
        let mut processor = Transaction {
            table: vec![[80, 120, 160]; LUT_ENTRIES],
            fail: true,
            analyze: true,
            updates: 0,
            commits: 0,
        };
        assert!(matches!(render(&mut processor), Err(Error::Media(_))));
        assert_eq!((processor.updates, processor.commits), (1, 0));
        assert!(gpu
            .resources
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .underwater
            .is_none());
        processor.fail = false;
        let GpuRenderedFrame::Rgb(recovered) = render(&mut processor).unwrap() else {
            panic!("RGB requested")
        };
        assert!(recovered
            .as_rgb8()
            .chunks_exact(3)
            .all(|rgb| rgb == [80, 120, 160]));
        assert_eq!((processor.updates, processor.commits), (2, 1));
        let buffers = || {
            let guard = gpu.resources.lock().unwrap();
            let stage = guard.as_ref().unwrap().underwater.as_ref().unwrap();
            (
                stage.lut.clone(),
                stage.analysis_readback.clone(),
                stage.packed_lut.as_ptr() as usize,
            )
        };
        let original = buffers();
        processor.analyze = false;
        let GpuRenderedFrame::Rgb(stable) = render(&mut processor).unwrap() else {
            panic!("RGB requested")
        };
        assert!(stable.as_rgb8() == recovered.as_rgb8());
        assert_eq!((processor.updates, processor.commits), (2, 2));
        assert_eq!(buffers(), original);
    }

    #[test]
    fn exact_analysis_resize_matches_cpu_bytes_for_odd_sizes_and_edges() {
        let Some(gpu) = gpu() else { return };
        for (width, height) in [(1, 1), (3, 5), (225, 223), (513, 257), (1280, 640)] {
            let input = dynamic_buffer(
                &gpu.device,
                "test analysis input",
                u64::from(width * height) * 4,
                wgpu::BufferUsages::STORAGE,
            );
            let resources = gpu
                .underwater_pipelines
                .prepare(
                    &gpu.device,
                    &gpu.queue,
                    &input,
                    (width, height),
                    &gpu.adapter,
                )
                .unwrap();
            for seed in [0, 173] {
                let words: Vec<u32> = (0..width * height)
                    .map(|index| {
                        let bytes = [
                            index.wrapping_mul(71).wrapping_add(seed) as u8,
                            (index / width).wrapping_mul(191).wrapping_add(seed) as u8,
                            index.wrapping_mul(37).wrapping_add(seed) as u8,
                            index as u8,
                        ];
                        u32::from_le_bytes(bytes)
                    })
                    .collect();
                let rgb: Vec<u8> = words
                    .iter()
                    .flat_map(|word| {
                        let bytes = word.to_le_bytes();
                        [bytes[0], bytes[1], bytes[2]]
                    })
                    .collect();
                let mut expected = vec![0; ANALYSIS_BYTES as usize];
                crate::underwater::resize::rgb8(
                    &rgb,
                    width as usize,
                    height as usize,
                    &mut expected,
                    ANALYSIS_SIZE,
                    ANALYSIS_SIZE,
                );
                gpu.queue
                    .write_buffer(&input, 0, bytemuck::cast_slice(&words));
                let mut encoder = gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                {
                    let mut pass =
                        encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                    pass.set_pipeline(&gpu.underwater_pipelines.analysis);
                    pass.set_bind_group(0, &resources.analysis_bindings, &[]);
                    pass.dispatch_workgroups(
                        (ANALYSIS_SIZE * ANALYSIS_SIZE / 4).div_ceil(64) as u32,
                        1,
                        1,
                    );
                }
                encoder.copy_buffer_to_buffer(
                    &resources.analysis_output,
                    0,
                    &resources.analysis_readback,
                    0,
                    ANALYSIS_BYTES,
                );
                let submission = gpu.queue.submit([encoder.finish()]);
                let mut actual = Analysis(Vec::new());
                gpu.read_underwater_analysis(&resources.analysis_readback, submission, &mut actual)
                    .unwrap();
                let differing = actual
                    .0
                    .iter()
                    .zip(&expected)
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(actual.0.len(), expected.len());
                assert_eq!(
                    differing, 0,
                    "{width}×{height}, seed{seed}: byte differences"
                );
            }
        }
    }

    #[test]
    fn integer_gpu_lut_matches_generic_scalar_negative_deltas_ties_and_alpha() {
        let Some(gpu) = gpu() else { return };
        let (width, height) = (511, 257);
        let size = u64::from(width * height) * 4;
        let input = dynamic_buffer(
            &gpu.device,
            "test ILUT input",
            size,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let resources = gpu
            .underwater_pipelines
            .prepare(
                &gpu.device,
                &gpu.queue,
                &input,
                (width, height),
                &gpu.adapter,
            )
            .unwrap();
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test ILUT output"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for seed in [0, 193] {
            let table: Vec<[u8; 3]> = (0..17_usize)
                .flat_map(|x| {
                    (0..17_usize).flat_map(move |y| {
                        (0..17_usize).map(move |z| {
                            [
                                ((x * 73 + y * 151 + z * 29 + seed) ^ (x * y * 11)) as u8,
                                ((x * 113 + y * 17 + z * 197 + seed) ^ (y * z * 7)) as u8,
                                ((x * 23 + y * 199 + z * 61 + seed) ^ (x * z * 13)) as u8,
                            ]
                        })
                    })
                })
                .collect();
            let bytes: Vec<u8> = [16_u32, 256, 16]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .chain(table.iter().flat_map(|rgb| rgb.iter().copied()))
                .collect();
            let scalar = crate::underwater::IntegerLut::parse(&bytes).unwrap();
            let packed: Vec<u32> = table
                .iter()
                .map(|v| u32::from_le_bytes([v[0], v[1], v[2], 0]))
                .collect();
            gpu.queue
                .write_buffer(&resources.lut, 0, bytemuck::cast_slice(&packed));
            let words: Vec<u32> = (0..width * height)
                .map(|index| {
                    u32::from_le_bytes([
                        index as u8,
                        (index >> 8) as u8,
                        (index.wrapping_mul(71) + (index >> 16)) as u8,
                        (index >> 3) as u8,
                    ])
                })
                .collect();
            gpu.queue
                .write_buffer(&input, 0, bytemuck::cast_slice(&words));
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                pass.set_pipeline(&gpu.underwater_pipelines.apply);
                pass.set_bind_group(0, &resources.apply_bindings, &[]);
                pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(8), 1);
            }
            encoder.copy_buffer_to_buffer(&input, 0, &readback, 0, size);
            let submission = gpu.queue.submit([encoder.finish()]);
            let slice = readback.slice(..);
            let (sender, receiver) = mpsc::sync_channel(1);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap()
            });
            gpu.device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: None,
                })
                .unwrap();
            receiver.recv().unwrap().unwrap();
            let actual = slice.get_mapped_range().unwrap();
            for (index, (word, actual)) in words.iter().zip(actual.chunks_exact(4)).enumerate() {
                let rgba = word.to_le_bytes();
                let expected = scalar.sample([rgba[0], rgba[1], rgba[2]]);
                assert_eq!(
                    actual,
                    [expected[0], expected[1], expected[2], rgba[3]],
                    "pixel{index}, seed{seed}"
                );
            }
            drop(actual);
            readback.unmap();
        }
    }
}
