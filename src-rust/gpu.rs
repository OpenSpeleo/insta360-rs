//! Portable wgpu stitch renderer and backend discovery.

use std::sync::{mpsc, Arc, Mutex};

use bytemuck::{Pod, Zeroable};

use crate::calibration::LensProjectionModel;
use crate::color::CubeLut;
use crate::motion::readout::MAX_READOUT_POSES;
use crate::motion::{FrameMotion, ReadoutPoseTable};
use crate::stitch::{MaskCache, PreparedMasks};
use crate::{
    EquirectangularProjection, Error, GpuAdapterInfo, GpuFailure, GpuFailureCode, GpuFailureStage,
    LensFrame, Orientation, PanoramaFrame, ResolvedCalibration, Result, StitchEngine,
};

const WORKGROUP_WIDTH: u32 = 16;
const WORKGROUP_HEIGHT: u32 = 8;
const RGB_CHANNELS: usize = 3;
const RGBA_CHANNELS: usize = 4;

/// One borrowed 8-bit image plane with its decoded row stride.
#[derive(Clone, Copy, Debug)]
pub struct GpuPlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

/// YUV sample range used by the portable GPU color conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuYuvRange {
    Limited,
    Full,
}

/// Non-constant-luminance matrix used by the portable GPU color conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuYuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// Chroma sample placement for an 8-bit planar 4:2:0 frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuChromaLocation {
    Left,
    Center,
}

/// Borrowed 8-bit planar 4:2:0 frame uploaded directly to wgpu.
#[derive(Clone, Copy, Debug)]
pub struct GpuYuv420Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub y: GpuPlane<'a>,
    pub u: GpuPlane<'a>,
    pub v: GpuPlane<'a>,
    pub range: GpuYuvRange,
    pub matrix: GpuYuvMatrix,
    pub chroma_location: GpuChromaLocation,
}

/// Encoder-ready limited-range BT.709 planar YUV420 output from the GPU.
#[derive(Clone, Debug)]
pub struct GpuYuv420Output {
    width: u32,
    height: u32,
    y_stride: usize,
    chroma_stride: usize,
    u_offset: usize,
    v_offset: usize,
    data: Vec<u8>,
}

impl GpuYuv420Output {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn planes(&self) -> [GpuPlane<'_>; 3] {
        [
            GpuPlane {
                data: &self.data[..self.u_offset],
                stride: self.y_stride,
            },
            GpuPlane {
                data: &self.data[self.u_offset..self.v_offset],
                stride: self.chroma_stride,
            },
            GpuPlane {
                data: &self.data[self.v_offset..],
                stride: self.chroma_stride,
            },
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuOutputKind {
    Rgb,
    Yuv420,
}

enum GpuRenderedFrame {
    Rgb(PanoramaFrame),
    Yuv420(GpuYuv420Output),
}

#[derive(Clone, Copy, Debug)]
struct YuvOutputLayout {
    y_stride: u32,
    chroma_stride: u32,
    u_offset: u64,
    v_offset: u64,
    size: u64,
}

#[derive(Clone, Copy)]
enum GpuSourceFrames<'a> {
    Rgb(&'a [LensFrame; 2]),
    Yuv420(&'a [GpuYuv420Frame<'a>; 2]),
}

impl GpuSourceFrames<'_> {
    fn dimensions(self) -> [(u32, u32); 2] {
        match self {
            Self::Rgb(frames) => {
                std::array::from_fn(|index| (frames[index].width(), frames[index].height()))
            }
            Self::Yuv420(frames) => {
                std::array::from_fn(|index| (frames[index].width, frames[index].height))
            }
        }
    }

    fn input_kind(self) -> u32 {
        match self {
            Self::Rgb(_) => 0,
            Self::Yuv420(_) => 1,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameParams {
    output_size: [u32; 2],
    padding: [u32; 2],
    output_to_camera: [f32; 4],
    feather_dead_zone: [f32; 4],
    lut_domain_min_size: [f32; 4],
    lut_domain_scale: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LensParams {
    orientation: [f32; 4],
    intrinsics: [f32; 4],
    native: [f32; 4],
    source: [f32; 4],
    model_meta: [u32; 4],
    geometry: [f32; 4],
    coefficients: [[f32; 4]; 4],
    compatibility: [f32; 4],
    readout: [f32; 4],
}

/// Stateful portable GPU renderer.
///
/// It retains the adapter, device, queue, shaders, sampler, and one reusable
/// resource set for the active input/output dimensions.
pub struct GpuStitcher {
    device: wgpu::Device,
    queue: wgpu::Queue,
    stitch_pipeline: wgpu::ComputePipeline,
    radiometry_first_pipeline: wgpu::ComputePipeline,
    radiometry_second_pipeline: wgpu::ComputePipeline,
    radiometry_means_pipeline: wgpu::ComputePipeline,
    radiometry_slopes_pipeline: wgpu::ComputePipeline,
    rgb_to_yuv_pipeline: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    adapter: GpuAdapterInfo,
    resources: Mutex<Option<GpuFrameResources>>,
    masks: MaskCache,
    color_lut: Option<Arc<CubeLut>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GpuResourceKey {
    input_kind: u32,
    output_kind: GpuOutputKind,
    lens_width: u32,
    lens_height: u32,
    output_width: u32,
    output_height: u32,
    mask_bytes: u64,
}

struct GpuFrameResources {
    key: GpuResourceKey,
    source_textures: [wgpu::Texture; 2],
    yuv_textures: [[wgpu::Texture; 3]; 2],
    frame_buffer: wgpu::Buffer,
    readout_buffer: wgpu::Buffer,
    lens_buffer: wgpu::Buffer,
    mask_buffer: wgpu::Buffer,
    prepared_masks: Option<Arc<PreparedMasks>>,
    _slopes_buffer: wgpu::Buffer,
    _second_stats_buffer: wgpu::Buffer,
    output_buffer: wgpu::Buffer,
    readback: wgpu::Buffer,
    yuv_output_buffer: wgpu::Buffer,
    yuv_readback: wgpu::Buffer,
    stitch_bind_group: wgpu::BindGroup,
    radiometry_first_bind_group: wgpu::BindGroup,
    radiometry_second_bind_group: wgpu::BindGroup,
    radiometry_means_bind_group: wgpu::BindGroup,
    radiometry_slopes_bind_group: wgpu::BindGroup,
    rgb_to_yuv_bind_group: wgpu::BindGroup,
    upload_rgba: [Vec<u8>; 2],
    yuv_download: Vec<u8>,
}

impl std::fmt::Debug for GpuStitcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuStitcher")
            .field("adapter", &self.adapter)
            .finish_non_exhaustive()
    }
}

impl GpuStitcher {
    /// Opens a high-performance adapter and compiles the fixed stitch shader.
    pub fn new() -> Result<Self> {
        let backends = compiled_backends();
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = backends;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|error| {
            gpu_unavailable(
                GpuFailureCode::NoCompatibleAdapter,
                GpuFailureStage::Discovery,
                format!("no compatible compute adapter was found: {error}"),
                None,
            )
        })?;
        let adapter_info = public_adapter_info(adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("insta360-rs stitch device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| {
            gpu_unavailable(
                GpuFailureCode::DeviceRequest,
                GpuFailureStage::Preparation,
                format!("requesting the compute device failed: {error}"),
                Some(adapter_info.clone()),
            )
        })?;

        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("insta360-rs fixed stitch shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stitch_gpu.wgsl").into()),
        });
        let stitch_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("insta360-rs fixed stitch pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("stitch"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let radiometry_first_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("insta360-rs radiometry first pass"),
                layout: None,
                module: &shader,
                entry_point: Some("radiometry_first"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let radiometry_second_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("insta360-rs radiometry second pass"),
                layout: None,
                module: &shader,
                entry_point: Some("radiometry_second"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let radiometry_means_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("insta360-rs radiometry window means pass"),
                layout: None,
                module: &shader,
                entry_point: Some("radiometry_means"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let radiometry_slopes_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("insta360-rs radiometry slope pass"),
                layout: None,
                module: &shader,
                entry_point: Some("radiometry_slopes"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let rgb_to_yuv_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("insta360-rs RGB-to-YUV420 pass"),
                layout: None,
                module: &shader,
                entry_point: Some("rgb_to_yuv420"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let validation_error = pollster::block_on(validation.pop());
        let out_of_memory_error = pollster::block_on(out_of_memory.pop());
        if let Some(error) = validation_error {
            return Err(gpu_unavailable(
                GpuFailureCode::ShaderValidation,
                GpuFailureStage::Preparation,
                format!("validating the stitch shader failed: {error}"),
                Some(adapter_info),
            ));
        }
        if let Some(error) = out_of_memory_error {
            return Err(gpu_unavailable(
                GpuFailureCode::OutOfMemory,
                GpuFailureStage::Preparation,
                format!("allocating GPU pipelines failed: {error}"),
                Some(adapter_info),
            ));
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("insta360-rs source sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Ok(Self {
            device,
            queue,
            stitch_pipeline,
            radiometry_first_pipeline,
            radiometry_second_pipeline,
            radiometry_means_pipeline,
            radiometry_slopes_pipeline,
            rgb_to_yuv_pipeline,
            sampler,
            adapter: adapter_info,
            resources: Mutex::new(None),
            masks: MaskCache::default(),
            color_lut: None,
        })
    }

    /// Adapter used by this renderer.
    pub fn adapter_info(&self) -> &GpuAdapterInfo {
        &self.adapter
    }

    /// Applies a 3D color LUT after stitching and before RGB/YUV output conversion.
    ///
    /// The table is uploaded once when frame resources are prepared. Passing `None`
    /// disables the transform. Reapplying `None` or the same shared table retains
    /// existing frame resources. Geometry and source sampling are unaffected.
    pub fn set_color_lut(&mut self, lut: Option<Arc<CubeLut>>) {
        if match (&self.color_lut, &lut) {
            (None, None) => true,
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            _ => false,
        } {
            return;
        }
        self.color_lut = lut;
        *self
            .resources
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Stitches a pair while applying a gyro-derived output correction.
    pub fn stitch_with_orientation(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        correction: Orientation,
    ) -> Result<PanoramaFrame> {
        self.stitch_with_motion(
            lenses,
            calibration,
            projection,
            &FrameMotion::global(correction)?,
        )
    }

    /// Stitches with global stabilization and per-lens sensor readout correction.
    pub fn stitch_with_motion(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<PanoramaFrame> {
        match self.stitch_sources(
            GpuSourceFrames::Rgb(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Rgb,
        )? {
            GpuRenderedFrame::Rgb(frame) => Ok(frame),
            GpuRenderedFrame::Yuv420(_) => unreachable!("RGB output requested above"),
        }
    }

    /// Stitches decoded planar YUV420P frames without CPU RGB conversion.
    pub fn stitch_yuv420_with_orientation(
        &self,
        lenses: &[GpuYuv420Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        correction: Orientation,
    ) -> Result<PanoramaFrame> {
        self.stitch_yuv420_with_motion(
            lenses,
            calibration,
            projection,
            &FrameMotion::global(correction)?,
        )
    }

    /// Stitches with global stabilization and per-lens sensor readout correction.
    pub fn stitch_yuv420_with_motion(
        &self,
        lenses: &[GpuYuv420Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<PanoramaFrame> {
        match self.stitch_sources(
            GpuSourceFrames::Yuv420(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Rgb,
        )? {
            GpuRenderedFrame::Rgb(frame) => Ok(frame),
            GpuRenderedFrame::Yuv420(_) => unreachable!("RGB output requested above"),
        }
    }

    /// Stitches decoded YUV420P directly to encoder-ready YUV420P.
    pub fn stitch_yuv420_to_yuv420_with_orientation(
        &self,
        lenses: &[GpuYuv420Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        correction: Orientation,
    ) -> Result<GpuYuv420Output> {
        self.stitch_yuv420_to_yuv420_with_motion(
            lenses,
            calibration,
            projection,
            &FrameMotion::global(correction)?,
        )
    }

    /// Stitches with global stabilization and per-lens sensor readout correction.
    pub fn stitch_yuv420_to_yuv420_with_motion(
        &self,
        lenses: &[GpuYuv420Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<GpuYuv420Output> {
        match self.stitch_sources(
            GpuSourceFrames::Yuv420(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Yuv420,
        )? {
            GpuRenderedFrame::Yuv420(frame) => Ok(frame),
            GpuRenderedFrame::Rgb(_) => unreachable!("YUV output requested above"),
        }
    }

    /// Returns an encoder-consumed YUV allocation to this stitcher's scratch pool.
    #[cfg(feature = "media")]
    pub(crate) fn recycle_yuv420_output(&self, output: GpuYuv420Output) {
        let mut resource_guard = self
            .resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(resources) = resource_guard.as_mut() else {
            return;
        };
        if resources.key.output_width != output.width
            || resources.key.output_height != output.height
        {
            return;
        }
        recycle_download_buffer(&mut resources.yuv_download, output.data);
    }

    fn stitch_sources(
        &self,
        sources: GpuSourceFrames<'_>,
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        output_kind: GpuOutputKind,
    ) -> Result<GpuRenderedFrame> {
        let projection = projection.validate()?;
        calibration.validate_for_stitching()?;
        let dimensions = sources.dimensions();
        validate_source_frames(sources)?;
        validate_device_limits(&self.device, dimensions, projection, &self.adapter)?;

        let fisheye_masks = self.masks.prepare(dimensions, calibration)?;
        let mask_values = fisheye_masks
            .iter()
            .flatten()
            .map(|mask| mask.weights.len() as u64)
            .sum::<u64>();
        // WGSL rounds the header plus runtime-array minimum to vec4 alignment.
        let mask_bytes = 16 + mask_values.max(4) * 4;
        if mask_bytes > self.device.limits().max_storage_buffer_binding_size
            || mask_bytes > self.device.limits().max_buffer_size
        {
            return Err(gpu_unavailable(
                GpuFailureCode::UnsupportedLimits,
                GpuFailureStage::Preparation,
                "prepared source masks exceed the GPU storage-buffer limit".into(),
                Some(self.adapter.clone()),
            ));
        }
        let radiometry_enabled = calibration.lenses.iter().any(|lens| lens.lens_type != 0);
        let yuv_layout = (output_kind == GpuOutputKind::Yuv420)
            .then(|| yuv_output_layout(projection))
            .transpose()?;
        let (lut_domain_min_size, lut_domain_scale) = match &self.color_lut {
            Some(lut) => {
                let minimum = lut.domain_min();
                let maximum = lut.domain_max();
                (
                    [minimum[0], minimum[1], minimum[2], lut.size() as f32],
                    [
                        1.0 / (maximum[0] - minimum[0]),
                        1.0 / (maximum[1] - minimum[1]),
                        1.0 / (maximum[2] - minimum[2]),
                        0.0,
                    ],
                )
            }
            None => ([0.0; 4], [1.0; 4]),
        };
        let frame_params = FrameParams {
            output_size: [projection.width, projection.height],
            padding: yuv_layout.map_or([0, 0], |layout| [layout.y_stride, layout.chroma_stride]),
            output_to_camera: orientation_array(motion.correction().inverse()),
            feather_dead_zone: [
                0.08,
                projection.height as f32 * 0.25,
                if radiometry_enabled { 1.0 } else { 0.0 },
                0.0,
            ],
            lut_domain_min_size,
            lut_domain_scale,
        };
        let lens_params: [LensParams; 2] = std::array::from_fn(|index| {
            gpu_lens_params(
                dimensions[index],
                &calibration.lenses[index],
                calibration.lens_geometry[index],
                index,
                source_metadata(sources, index),
                motion.readout()[index].as_ref(),
            )
        });
        if lens_params
            .iter()
            .any(|lens| !gpu_lens_params_are_finite(lens))
        {
            return Err(gpu_unavailable(
                GpuFailureCode::UnsupportedLimits,
                GpuFailureStage::Preparation,
                "lens calibration exceeds the GPU's finite f32 parameter range".into(),
                Some(self.adapter.clone()),
            ));
        }

        let output_size = output_buffer_size(projection)?;
        let key = GpuResourceKey {
            input_kind: sources.input_kind(),
            output_kind,
            lens_width: dimensions[0].0,
            lens_height: dimensions[0].1,
            output_width: projection.width,
            output_height: projection.height,
            mask_bytes,
        };
        let mut resource_guard = self
            .resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if resource_guard
            .as_ref()
            .is_none_or(|resources| resources.key != key)
        {
            *resource_guard = Some(self.create_frame_resources(key, output_size)?);
        }
        let resources = resource_guard
            .as_mut()
            .expect("GPU frame resources initialized above");
        let out_of_memory = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        match sources {
            GpuSourceFrames::Rgb(lenses) => {
                for (lens_index, lens) in lenses.iter().enumerate() {
                    pack_rgb_to_rgba(lens, &mut resources.upload_rgba[lens_index]);
                    upload_source_texture(
                        &self.queue,
                        &resources.source_textures[lens_index],
                        lens,
                        &resources.upload_rgba[lens_index],
                    );
                }
            }
            GpuSourceFrames::Yuv420(lenses) => {
                for (lens_index, lens) in lenses.iter().enumerate() {
                    upload_yuv420_frame(&self.queue, &resources.yuv_textures[lens_index], lens);
                }
            }
        }
        self.queue.write_buffer(
            &resources.frame_buffer,
            0,
            bytemuck::bytes_of(&frame_params),
        );
        self.queue.write_buffer(
            &resources.lens_buffer,
            0,
            bytemuck::cast_slice(&lens_params),
        );
        if resources
            .prepared_masks
            .as_ref()
            .is_none_or(|previous| !Arc::ptr_eq(previous, &fisheye_masks))
        {
            let mut offsets = [u32::MAX, u32::MAX, 0, 0];
            let mut offset = 0_u32;
            for (index, mask) in fisheye_masks.iter().enumerate() {
                if let Some(mask) = mask {
                    offsets[index] = offset;
                    self.queue.write_buffer(
                        &resources.mask_buffer,
                        16 + u64::from(offset) * 4,
                        bytemuck::cast_slice(&mask.weights),
                    );
                    offset += mask.weights.len() as u32;
                }
            }
            self.queue
                .write_buffer(&resources.mask_buffer, 0, bytemuck::cast_slice(&offsets));
            resources.prepared_masks = Some(Arc::clone(&fisheye_masks));
        }
        for (index, table) in motion.readout().iter().enumerate() {
            if let Some(table) = table {
                let mut values = [[0.0_f32; 4]; MAX_READOUT_POSES];
                for (value, pose) in values.iter_mut().zip(table.poses()) {
                    *value = orientation_array(*pose);
                }
                self.queue.write_buffer(
                    &resources.readout_buffer,
                    (index * MAX_READOUT_POSES * std::mem::size_of::<[f32; 4]>()) as u64,
                    bytemuck::cast_slice(&values[..table.poses().len()]),
                );
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("insta360-rs stitch commands"),
            });
        let radiometry_workgroups = projection.width.div_ceil(64);
        if radiometry_enabled {
            for (label, pipeline, bind_group) in [
                (
                    "insta360-rs radiometry first pass",
                    &self.radiometry_first_pipeline,
                    &resources.radiometry_first_bind_group,
                ),
                (
                    "insta360-rs radiometry second pass",
                    &self.radiometry_second_pipeline,
                    &resources.radiometry_second_bind_group,
                ),
                (
                    "insta360-rs radiometry window means pass",
                    &self.radiometry_means_pipeline,
                    &resources.radiometry_means_bind_group,
                ),
                (
                    "insta360-rs radiometry slope pass",
                    &self.radiometry_slopes_pipeline,
                    &resources.radiometry_slopes_bind_group,
                ),
            ] {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(label),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.dispatch_workgroups(radiometry_workgroups, 1, 1);
            }
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("insta360-rs stitch pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.stitch_pipeline);
            pass.set_bind_group(0, &resources.stitch_bind_group, &[]);
            pass.dispatch_workgroups(
                projection.width.div_ceil(WORKGROUP_WIDTH),
                projection.height.div_ceil(WORKGROUP_HEIGHT),
                1,
            );
        }
        if output_kind == GpuOutputKind::Yuv420 {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("insta360-rs RGB-to-YUV420 pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.rgb_to_yuv_pipeline);
            pass.set_bind_group(0, &resources.rgb_to_yuv_bind_group, &[]);
            let [workgroups_x, workgroups_y] = yuv_dispatch_size(projection);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }
        let (gpu_output, readback, readback_size) = match output_kind {
            GpuOutputKind::Rgb => (&resources.output_buffer, &resources.readback, output_size),
            GpuOutputKind::Yuv420 => (
                &resources.yuv_output_buffer,
                &resources.yuv_readback,
                yuv_layout.expect("YUV output requested a layout").size,
            ),
        };
        encoder.copy_buffer_to_buffer(gpu_output, 0, readback, 0, readback_size);
        let submission = self.queue.submit([encoder.finish()]);
        let validation_error = pollster::block_on(validation.pop());
        let out_of_memory_error = pollster::block_on(out_of_memory.pop());
        if let Some(error) = validation_error {
            return Err(gpu_processing(
                GpuFailureCode::Submission,
                GpuFailureStage::Dispatch,
                format!("submitting the stitch shader failed: {error}"),
                &self.adapter,
            ));
        }
        if let Some(error) = out_of_memory_error {
            return Err(gpu_processing(
                GpuFailureCode::OutOfMemory,
                GpuFailureStage::Dispatch,
                format!("allocating or submitting GPU frame resources failed: {error}"),
                &self.adapter,
            ));
        }

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
                    format!("waiting for GPU completion failed: {error}"),
                    &self.adapter,
                )
            })?;
        receiver
            .recv()
            .map_err(|error| {
                gpu_processing(
                    GpuFailureCode::Readback,
                    GpuFailureStage::Readback,
                    format!("the GPU readback callback was lost: {error}"),
                    &self.adapter,
                )
            })?
            .map_err(|error| {
                gpu_processing(
                    GpuFailureCode::Readback,
                    GpuFailureStage::Readback,
                    format!("mapping the GPU output failed: {error}"),
                    &self.adapter,
                )
            })?;

        let mapped = slice.get_mapped_range().map_err(|error| {
            gpu_processing(
                GpuFailureCode::Readback,
                GpuFailureStage::Readback,
                format!("accessing the mapped GPU output failed: {error}"),
                &self.adapter,
            )
        })?;
        let rendered = match output_kind {
            GpuOutputKind::Rgb => {
                let mut rgb = Vec::with_capacity(
                    usize::try_from(projection.width)
                        .ok()
                        .and_then(|width| {
                            usize::try_from(projection.height)
                                .ok()
                                .and_then(|height| width.checked_mul(height))
                        })
                        .and_then(|pixels| pixels.checked_mul(RGB_CHANNELS))
                        .ok_or_else(|| {
                            Error::InvalidMedia("GPU panorama size overflowed".into())
                        })?,
                );
                for pixel in mapped.chunks_exact(RGBA_CHANNELS) {
                    rgb.extend_from_slice(&pixel[..RGB_CHANNELS]);
                }
                GpuRenderedFrame::Rgb(PanoramaFrame::new(
                    projection.width,
                    projection.height,
                    rgb,
                )?)
            }
            GpuOutputKind::Yuv420 => {
                let yuv_layout = yuv_layout.expect("YUV output requested a layout");
                let data = copy_into_recycled_buffer(&mut resources.yuv_download, &mapped);
                GpuRenderedFrame::Yuv420(GpuYuv420Output {
                    width: projection.width,
                    height: projection.height,
                    y_stride: yuv_layout.y_stride as usize,
                    chroma_stride: yuv_layout.chroma_stride as usize,
                    u_offset: usize::try_from(yuv_layout.u_offset)
                        .map_err(|_| Error::InvalidMedia("GPU YUV U offset overflowed".into()))?,
                    v_offset: usize::try_from(yuv_layout.v_offset)
                        .map_err(|_| Error::InvalidMedia("GPU YUV V offset overflowed".into()))?,
                    data,
                })
            }
        };
        drop(mapped);
        readback.unmap();
        Ok(rendered)
    }

    fn create_frame_resources(
        &self,
        key: GpuResourceKey,
        output_size: u64,
    ) -> Result<GpuFrameResources> {
        // Allocating textures, buffers, and bindings can fail before dispatch.
        // Always drain both scopes, including when CPU-side preparation fails,
        // so subsequent frames do not inherit a stale scope or invalid cache.
        let out_of_memory = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let resources = self.create_frame_resources_scoped(key, output_size);
        let validation_error = pollster::block_on(validation.pop());
        let out_of_memory_error = pollster::block_on(out_of_memory.pop());
        if let Some(error) = out_of_memory_error {
            return Err(gpu_unavailable(
                GpuFailureCode::OutOfMemory,
                GpuFailureStage::Preparation,
                format!("allocating GPU frame resources failed: {error}"),
                Some(self.adapter.clone()),
            ));
        }
        if let Some(error) = validation_error {
            return Err(gpu_unavailable(
                GpuFailureCode::UnsupportedLimits,
                GpuFailureStage::Preparation,
                format!("validating GPU frame resources failed: {error}"),
                Some(self.adapter.clone()),
            ));
        }
        resources
    }

    fn create_frame_resources_scoped(
        &self,
        key: GpuResourceKey,
        output_size: u64,
    ) -> Result<GpuFrameResources> {
        let lens_texture_size = wgpu::Extent3d {
            width: key.lens_width,
            height: key.lens_height,
            depth_or_array_layers: 1,
        };
        let dummy_size = wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        };
        let source_size = if key.input_kind == 0 {
            lens_texture_size
        } else {
            dummy_size
        };
        let source_textures = std::array::from_fn(|_| {
            create_sampled_texture(
                &self.device,
                "insta360-rs RGB source lens",
                source_size,
                wgpu::TextureFormat::Rgba8Unorm,
            )
        });
        let source_views = source_textures
            .each_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let yuv_textures = std::array::from_fn(|_| {
            std::array::from_fn(|plane| {
                let size = if key.input_kind == 1 {
                    if plane == 0 {
                        lens_texture_size
                    } else {
                        wgpu::Extent3d {
                            width: key.lens_width.div_ceil(2),
                            height: key.lens_height.div_ceil(2),
                            depth_or_array_layers: 1,
                        }
                    }
                } else {
                    dummy_size
                };
                create_sampled_texture(
                    &self.device,
                    "insta360-rs YUV source plane",
                    size,
                    wgpu::TextureFormat::R8Unorm,
                )
            })
        });
        let yuv_views = yuv_textures.each_ref().map(|planes| {
            planes
                .each_ref()
                .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()))
        });
        let frame_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs frame parameters",
            std::mem::size_of::<FrameParams>() as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let readout_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs sensor readout poses",
            (2 * MAX_READOUT_POSES * std::mem::size_of::<[f32; 4]>()) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let lens_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs lens parameters",
            std::mem::size_of::<[LensParams; 2]>() as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let mask_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs mask parameters",
            key.mask_bytes,
            wgpu::BufferUsages::STORAGE,
        );
        let slopes_size = u64::from(key.output_width)
            .checked_mul(2)
            .and_then(|values| values.checked_mul(std::mem::size_of::<[f32; 4]>() as u64))
            .ok_or_else(|| Error::InvalidMedia("GPU slope buffer size overflowed".into()))?;
        let slopes_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs radiometric slopes",
            slopes_size,
            wgpu::BufferUsages::STORAGE,
        );
        let second_stats_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs radiometric second-pass statistics",
            slopes_size,
            wgpu::BufferUsages::STORAGE,
        );
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("insta360-rs packed panorama"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_size = match key.output_kind {
            GpuOutputKind::Rgb => output_size,
            GpuOutputKind::Yuv420 => std::mem::size_of::<u32>() as u64,
        };
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("insta360-rs panorama readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let yuv_buffer_size = match key.output_kind {
            GpuOutputKind::Rgb => std::mem::size_of::<u32>() as u64,
            GpuOutputKind::Yuv420 => {
                yuv_output_layout(EquirectangularProjection {
                    width: key.output_width,
                    height: key.output_height,
                })?
                .size
            }
        };
        let yuv_output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("insta360-rs packed YUV420 panorama"),
            size: yuv_buffer_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let yuv_readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("insta360-rs YUV420 panorama readback"),
            size: yuv_buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let stitch_layout = self.stitch_pipeline.get_bind_group_layout(0);
        let lut_values = self
            .color_lut
            .as_ref()
            .map_or(&[[0.0; 4]][..], |lut| lut.values());
        let lut_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs color LUT",
            std::mem::size_of_val(lut_values) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        self.queue
            .write_buffer(&lut_buffer, 0, bytemuck::cast_slice(lut_values));
        let stitch_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("insta360-rs stitch bindings"),
            layout: &stitch_layout,
            entries: &[
                binding(0, frame_buffer.as_entire_binding()),
                binding(1, lens_buffer.as_entire_binding()),
                binding(2, mask_buffer.as_entire_binding()),
                binding(3, second_stats_buffer.as_entire_binding()),
                binding(4, wgpu::BindingResource::TextureView(&source_views[0])),
                binding(5, wgpu::BindingResource::TextureView(&source_views[1])),
                binding(6, wgpu::BindingResource::Sampler(&self.sampler)),
                binding(7, output_buffer.as_entire_binding()),
                binding(9, wgpu::BindingResource::TextureView(&yuv_views[0][0])),
                binding(10, wgpu::BindingResource::TextureView(&yuv_views[0][1])),
                binding(11, wgpu::BindingResource::TextureView(&yuv_views[0][2])),
                binding(12, wgpu::BindingResource::TextureView(&yuv_views[1][0])),
                binding(13, wgpu::BindingResource::TextureView(&yuv_views[1][1])),
                binding(14, wgpu::BindingResource::TextureView(&yuv_views[1][2])),
                binding(16, lut_buffer.as_entire_binding()),
                binding(17, readout_buffer.as_entire_binding()),
            ],
        });
        let radiometry_first_layout = self.radiometry_first_pipeline.get_bind_group_layout(0);
        let radiometry_first_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("insta360-rs radiometry first-pass bindings"),
                layout: &radiometry_first_layout,
                entries: &[
                    binding(0, frame_buffer.as_entire_binding()),
                    binding(1, lens_buffer.as_entire_binding()),
                    binding(2, mask_buffer.as_entire_binding()),
                    binding(3, slopes_buffer.as_entire_binding()),
                    binding(4, wgpu::BindingResource::TextureView(&source_views[0])),
                    binding(5, wgpu::BindingResource::TextureView(&source_views[1])),
                    binding(6, wgpu::BindingResource::Sampler(&self.sampler)),
                    binding(9, wgpu::BindingResource::TextureView(&yuv_views[0][0])),
                    binding(10, wgpu::BindingResource::TextureView(&yuv_views[0][1])),
                    binding(11, wgpu::BindingResource::TextureView(&yuv_views[0][2])),
                    binding(12, wgpu::BindingResource::TextureView(&yuv_views[1][0])),
                    binding(13, wgpu::BindingResource::TextureView(&yuv_views[1][1])),
                    binding(14, wgpu::BindingResource::TextureView(&yuv_views[1][2])),
                    binding(17, readout_buffer.as_entire_binding()),
                ],
            });
        let radiometry_second_layout = self.radiometry_second_pipeline.get_bind_group_layout(0);
        let radiometry_second_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("insta360-rs radiometry second-pass bindings"),
                layout: &radiometry_second_layout,
                entries: &[
                    binding(0, frame_buffer.as_entire_binding()),
                    binding(1, lens_buffer.as_entire_binding()),
                    binding(2, mask_buffer.as_entire_binding()),
                    binding(3, slopes_buffer.as_entire_binding()),
                    binding(4, wgpu::BindingResource::TextureView(&source_views[0])),
                    binding(5, wgpu::BindingResource::TextureView(&source_views[1])),
                    binding(6, wgpu::BindingResource::Sampler(&self.sampler)),
                    binding(8, second_stats_buffer.as_entire_binding()),
                    binding(9, wgpu::BindingResource::TextureView(&yuv_views[0][0])),
                    binding(10, wgpu::BindingResource::TextureView(&yuv_views[0][1])),
                    binding(11, wgpu::BindingResource::TextureView(&yuv_views[0][2])),
                    binding(12, wgpu::BindingResource::TextureView(&yuv_views[1][0])),
                    binding(13, wgpu::BindingResource::TextureView(&yuv_views[1][1])),
                    binding(14, wgpu::BindingResource::TextureView(&yuv_views[1][2])),
                    binding(17, readout_buffer.as_entire_binding()),
                ],
            });
        let radiometry_slopes_layout = self.radiometry_slopes_pipeline.get_bind_group_layout(0);
        let radiometry_means_layout = self.radiometry_means_pipeline.get_bind_group_layout(0);
        let radiometry_means_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("insta360-rs radiometry window means bindings"),
                layout: &radiometry_means_layout,
                entries: &[
                    binding(0, frame_buffer.as_entire_binding()),
                    binding(3, slopes_buffer.as_entire_binding()),
                    binding(8, second_stats_buffer.as_entire_binding()),
                ],
            });
        let radiometry_slopes_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("insta360-rs radiometry slope bindings"),
                layout: &radiometry_slopes_layout,
                entries: &[
                    binding(0, frame_buffer.as_entire_binding()),
                    binding(3, slopes_buffer.as_entire_binding()),
                    binding(8, second_stats_buffer.as_entire_binding()),
                ],
            });
        let rgb_to_yuv_layout = self.rgb_to_yuv_pipeline.get_bind_group_layout(0);
        let rgb_to_yuv_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("insta360-rs RGB-to-YUV420 bindings"),
            layout: &rgb_to_yuv_layout,
            entries: &[
                binding(0, frame_buffer.as_entire_binding()),
                binding(7, output_buffer.as_entire_binding()),
                binding(15, yuv_output_buffer.as_entire_binding()),
            ],
        });
        let upload_len = if key.input_kind == 0 {
            usize::try_from(key.lens_width)
                .ok()
                .and_then(|width| {
                    usize::try_from(key.lens_height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(RGBA_CHANNELS))
                .ok_or_else(|| Error::InvalidMedia("GPU upload size overflowed".into()))?
        } else {
            0
        };
        Ok(GpuFrameResources {
            key,
            source_textures,
            yuv_textures,
            frame_buffer,
            readout_buffer,
            lens_buffer,
            mask_buffer,
            prepared_masks: None,
            _slopes_buffer: slopes_buffer,
            _second_stats_buffer: second_stats_buffer,
            output_buffer,
            readback,
            yuv_output_buffer,
            yuv_readback,
            stitch_bind_group,
            radiometry_first_bind_group,
            radiometry_second_bind_group,
            radiometry_means_bind_group,
            radiometry_slopes_bind_group,
            rgb_to_yuv_bind_group,
            upload_rgba: std::array::from_fn(|_| Vec::with_capacity(upload_len)),
            yuv_download: Vec::new(),
        })
    }
}

impl StitchEngine for GpuStitcher {
    fn stitch(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
    ) -> Result<PanoramaFrame> {
        self.stitch_with_orientation(lenses, calibration, projection, Orientation::IDENTITY)
    }
}

/// Enumerates portable compute providers compiled for the current platform.
pub fn available_adapters() -> Vec<GpuAdapterInfo> {
    let backends = compiled_backends();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    let instance = wgpu::Instance::new(descriptor);
    pollster::block_on(instance.enumerate_adapters(backends))
        .into_iter()
        .map(|adapter| public_adapter_info(adapter.get_info()))
        .collect()
}

fn compiled_backends() -> wgpu::Backends {
    #[cfg(target_os = "macos")]
    {
        wgpu::Backends::METAL
    }
    #[cfg(target_os = "windows")]
    {
        wgpu::Backends::DX12
    }
    #[cfg(target_os = "linux")]
    {
        wgpu::Backends::VULKAN
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        wgpu::Backends::empty()
    }
}

fn public_adapter_info(info: wgpu::AdapterInfo) -> GpuAdapterInfo {
    GpuAdapterInfo {
        name: info.name,
        backend: format!("{:?}", info.backend),
        device_type: format!("{:?}", info.device_type),
        vendor: info.vendor,
        device: info.device,
        driver: info.driver,
        driver_info: info.driver_info,
    }
}

fn orientation_array(value: Orientation) -> [f32; 4] {
    [
        value.w as f32,
        value.x as f32,
        value.y as f32,
        value.z as f32,
    ]
}

fn gpu_lens_params(
    dimensions: (u32, u32),
    lens: &crate::ParsedLens,
    geometry: Option<crate::ResolvedLensGeometry>,
    lens_index: usize,
    source_metadata: [f32; 2],
    readout: Option<&ReadoutPoseTable>,
) -> LensParams {
    let mut coefficient_values = [0.0_f32; 16];
    let normalized = lens.polynomial_projection;
    let coefficients = normalized
        .as_ref()
        .map_or(lens.distortion_coefficients.as_slice(), |value| {
            value.coefficients.as_slice()
        });
    let focal_scale = normalized.map_or(1.0, |value| value.focal_scale);
    for (destination, source) in coefficient_values.iter_mut().zip(coefficients.iter()) {
        *destination = *source as f32;
    }
    let model = match lens.model {
        LensProjectionModel::PinholePolynomialV1 | LensProjectionModel::PinholePolynomialV2 => 1,
        LensProjectionModel::OmniRadtan => 2,
        LensProjectionModel::OmniRadtanPro => 3,
    };
    LensParams {
        orientation: orientation_array(lens.orientation),
        intrinsics: [
            lens.cx as f32,
            lens.cy as f32,
            (lens.fx * focal_scale) as f32,
            (lens.fy * focal_scale) as f32,
        ],
        native: [
            lens.xi.unwrap_or_default() as f32,
            lens.radius.unwrap_or_default() as f32,
            lens.canvas_width as f32,
            lens.canvas_height as f32,
        ],
        source: [
            dimensions.0 as f32,
            dimensions.1 as f32,
            source_metadata[0],
            source_metadata[1],
        ],
        model_meta: [
            model,
            lens.lens_type,
            lens_index as u32,
            coefficients.len() as u32,
        ],
        geometry: geometry.map_or([-1.0, -1.0, 0.0, 0.0], |geometry| {
            [
                geometry.half_fov_radians() as f32,
                geometry.blend_angle_radians() as f32,
                0.0,
                0.0,
            ]
        }),
        coefficients: [
            coefficient_values[0..4]
                .try_into()
                .expect("four coefficients"),
            coefficient_values[4..8]
                .try_into()
                .expect("four coefficients"),
            coefficient_values[8..12]
                .try_into()
                .expect("four coefficients"),
            coefficient_values[12..16]
                .try_into()
                .expect("four coefficients"),
        ],
        compatibility: [lens.k1 as f32, lens.k2 as f32, lens.k3 as f32, 0.0],
        readout: readout.map_or([0.0; 4], |table| {
            [
                table.poses().len() as f32,
                table.direction() as u32 as f32,
                table.sensor_fraction()[0] as f32,
                table.sensor_fraction()[1] as f32,
            ]
        }),
    }
}

fn gpu_lens_params_are_finite(lens: &LensParams) -> bool {
    [
        lens.orientation,
        lens.intrinsics,
        lens.native,
        lens.source,
        lens.geometry,
        lens.coefficients[0],
        lens.coefficients[1],
        lens.coefficients[2],
        lens.coefficients[3],
        lens.compatibility,
        lens.readout,
    ]
    .into_iter()
    .flatten()
    .all(f32::is_finite)
        && lens.intrinsics[2] > 0.0
        && lens.intrinsics[3] > 0.0
}

fn source_metadata(sources: GpuSourceFrames<'_>, lens_index: usize) -> [f32; 2] {
    let GpuSourceFrames::Yuv420(frames) = sources else {
        return [0.0, 0.0];
    };
    let frame = frames[lens_index];
    let range = match frame.range {
        GpuYuvRange::Limited => 0_u32,
        GpuYuvRange::Full => 1,
    };
    let matrix = match frame.matrix {
        GpuYuvMatrix::Bt601 => 0_u32,
        GpuYuvMatrix::Bt709 => 1,
        GpuYuvMatrix::Bt2020 => 2,
    };
    let chroma = match frame.chroma_location {
        GpuChromaLocation::Left => 0_u32,
        GpuChromaLocation::Center => 1,
    };
    [1.0, (range | (matrix << 1) | (chroma << 3)) as f32]
}

fn upload_source_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    frame: &LensFrame,
    rgba: &[u8],
) {
    let size = wgpu::Extent3d {
        width: frame.width(),
        height: frame.height(),
        depth_or_array_layers: 1,
    };
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame.width() * RGBA_CHANNELS as u32),
            rows_per_image: Some(frame.height()),
        },
        size,
    );
}

fn upload_yuv420_frame(
    queue: &wgpu::Queue,
    textures: &[wgpu::Texture; 3],
    frame: &GpuYuv420Frame<'_>,
) {
    let planes = [frame.y, frame.u, frame.v];
    for (plane_index, plane) in planes.into_iter().enumerate() {
        let width = if plane_index == 0 {
            frame.width
        } else {
            frame.width.div_ceil(2)
        };
        let height = if plane_index == 0 {
            frame.height
        } else {
            frame.height.div_ceil(2)
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &textures[plane_index],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            plane.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(plane.stride as u32),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}

fn create_sampled_texture(
    device: &wgpu::Device,
    label: &'static str,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn dynamic_buffer(
    device: &wgpu::Device,
    label: &'static str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn binding(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

fn pack_rgb_to_rgba(frame: &LensFrame, rgba: &mut Vec<u8>) {
    rgba.clear();
    let required = frame.as_rgb8().len() / RGB_CHANNELS * RGBA_CHANNELS;
    if rgba.capacity() < required {
        rgba.reserve_exact(required);
    }
    for pixel in frame.as_rgb8().chunks_exact(RGB_CHANNELS) {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
}

fn copy_into_recycled_buffer(scratch: &mut Vec<u8>, source: &[u8]) -> Vec<u8> {
    let mut output = std::mem::take(scratch);
    output.clear();
    output.extend_from_slice(source);
    output
}

#[cfg(any(feature = "media", test))]
fn recycle_download_buffer(scratch: &mut Vec<u8>, mut returned: Vec<u8>) {
    if returned.capacity() > scratch.capacity() {
        returned.clear();
        *scratch = returned;
    }
}

fn output_buffer_size(projection: EquirectangularProjection) -> Result<u64> {
    u64::from(projection.width)
        .checked_mul(u64::from(projection.height))
        .and_then(|pixels| pixels.checked_mul(RGBA_CHANNELS as u64))
        .ok_or_else(|| Error::InvalidMedia("GPU panorama size overflowed".into()))
}

fn yuv_dispatch_size(projection: EquirectangularProjection) -> [u32; 2] {
    let horizontal_blocks = projection.width.div_ceil(8);
    let vertical_blocks = projection.height.div_ceil(2);
    [
        horizontal_blocks.div_ceil(WORKGROUP_WIDTH),
        vertical_blocks.div_ceil(WORKGROUP_HEIGHT),
    ]
}

fn yuv_output_layout(projection: EquirectangularProjection) -> Result<YuvOutputLayout> {
    if !projection.width.is_multiple_of(2) || !projection.height.is_multiple_of(2) {
        return Err(Error::InvalidMedia(
            "GPU YUV420 output dimensions must be even".into(),
        ));
    }
    let y_stride = projection
        .width
        .checked_add(7)
        .map(|value| value / 8 * 8)
        .ok_or_else(|| Error::InvalidMedia("GPU YUV stride overflowed".into()))?;
    let chroma_width = projection.width / 2;
    let chroma_stride = chroma_width
        .checked_add(3)
        .map(|value| value / 4 * 4)
        .ok_or_else(|| Error::InvalidMedia("GPU chroma stride overflowed".into()))?;
    let y_size = u64::from(y_stride)
        .checked_mul(u64::from(projection.height))
        .ok_or_else(|| Error::InvalidMedia("GPU Y plane size overflowed".into()))?;
    let chroma_size = u64::from(chroma_stride)
        .checked_mul(u64::from(projection.height / 2))
        .ok_or_else(|| Error::InvalidMedia("GPU chroma plane size overflowed".into()))?;
    let v_offset = y_size
        .checked_add(chroma_size)
        .ok_or_else(|| Error::InvalidMedia("GPU YUV offset overflowed".into()))?;
    let size = v_offset
        .checked_add(chroma_size)
        .ok_or_else(|| Error::InvalidMedia("GPU YUV output size overflowed".into()))?;
    Ok(YuvOutputLayout {
        y_stride,
        chroma_stride,
        u_offset: y_size,
        v_offset,
        size,
    })
}

fn validate_source_frames(sources: GpuSourceFrames<'_>) -> Result<()> {
    let dimensions = sources.dimensions();
    if dimensions[0] != dimensions[1] {
        return Err(Error::InvalidMedia(format!(
            "GPU stitching requires equal lens dimensions, found {}x{} and {}x{}",
            dimensions[0].0, dimensions[0].1, dimensions[1].0, dimensions[1].1
        )));
    }
    if dimensions[0].0 == 0 || dimensions[0].1 == 0 {
        return Err(Error::InvalidMedia(
            "GPU source dimensions must be non-zero".into(),
        ));
    }
    if let GpuSourceFrames::Yuv420(frames) = sources {
        for (lens_index, frame) in frames.iter().enumerate() {
            validate_plane(lens_index, "Y", frame.y, frame.width, frame.height)?;
            validate_plane(
                lens_index,
                "U",
                frame.u,
                frame.width.div_ceil(2),
                frame.height.div_ceil(2),
            )?;
            validate_plane(
                lens_index,
                "V",
                frame.v,
                frame.width.div_ceil(2),
                frame.height.div_ceil(2),
            )?;
        }
    }
    Ok(())
}

fn validate_plane(
    lens_index: usize,
    name: &str,
    plane: GpuPlane<'_>,
    width: u32,
    height: u32,
) -> Result<()> {
    let width = width as usize;
    let height = height as usize;
    if plane.stride < width || plane.stride > u32::MAX as usize {
        return Err(Error::InvalidMedia(format!(
            "GPU lens {lens_index} {name} plane stride {} cannot store a {width}-byte row",
            plane.stride
        )));
    }
    let required = plane
        .stride
        .checked_mul(height.saturating_sub(1))
        .and_then(|bytes| bytes.checked_add(width))
        .ok_or_else(|| Error::InvalidMedia("GPU YUV plane size overflowed".into()))?;
    if plane.data.len() < required {
        return Err(Error::InvalidMedia(format!(
            "GPU lens {lens_index} {name} plane requires {required} bytes, received {}",
            plane.data.len()
        )));
    }
    Ok(())
}

fn validate_device_limits(
    device: &wgpu::Device,
    dimensions: [(u32, u32); 2],
    projection: EquirectangularProjection,
    adapter: &GpuAdapterInfo,
) -> Result<()> {
    let limits = device.limits();
    // Only the input is a texture. Panorama pixels live in a storage buffer,
    // so a wide panorama must not be rejected by the texture dimension limit.
    let maximum_dimension = dimensions[0].0.max(dimensions[0].1);
    let output_size = output_buffer_size(projection)?;
    let slopes_size = u64::from(projection.width) * 2 * std::mem::size_of::<[f32; 4]>() as u64;
    let maximum_storage_size = output_size.max(slopes_size);
    if maximum_dimension > limits.max_texture_dimension_2d
        || maximum_storage_size > limits.max_storage_buffer_binding_size
        || maximum_storage_size > limits.max_buffer_size
        || projection.width.div_ceil(WORKGROUP_WIDTH) > limits.max_compute_workgroups_per_dimension
        || projection.height.div_ceil(WORKGROUP_HEIGHT)
            > limits.max_compute_workgroups_per_dimension
    {
        return Err(gpu_unavailable(
            GpuFailureCode::UnsupportedLimits,
            GpuFailureStage::Preparation,
            format!(
                "requested dimensions exceed device limits: {maximum_storage_size}-byte storage buffer, {maximum_dimension}px source texture, {}x{} panorama",
                projection.width, projection.height
            ),
            Some(adapter.clone()),
        ));
    }
    Ok(())
}

fn gpu_unavailable(
    code: GpuFailureCode,
    stage: GpuFailureStage,
    message: String,
    adapter: Option<GpuAdapterInfo>,
) -> Error {
    let mut failure = GpuFailure::new(code, stage, message);
    failure.adapter = adapter;
    Error::GpuUnavailable(Box::new(failure))
}

fn gpu_processing(
    code: GpuFailureCode,
    stage: GpuFailureStage,
    message: String,
    adapter: &GpuAdapterInfo,
) -> Error {
    Error::GpuProcessing(Box::new(
        GpuFailure::new(code, stage, message).with_adapter(adapter.clone()),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn unchanged_color_lut_reuses_gpu_resources_and_changes_invalidate_them() {
        use crate::calibration::synthetic_dual_fisheye_calibration;
        use crate::{EquirectangularProjection, LensFrame, Orientation};
        use std::sync::Arc;
        if super::available_adapters().is_empty() {
            assert!(
                std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_none(),
                "GPU is required"
            );
            return;
        }
        let mut stitcher = super::GpuStitcher::new().unwrap();
        let lenses = [
            LensFrame::new(16, 16, vec![96; 16 * 16 * 3]).unwrap(),
            LensFrame::new(16, 16, vec![96; 16 * 16 * 3]).unwrap(),
        ];
        let calibration = synthetic_dual_fisheye_calibration(16, 16).unwrap();
        let projection = EquirectangularProjection {
            width: 16,
            height: 8,
        };
        let render = |stitcher: &super::GpuStitcher| {
            stitcher
                .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
                .unwrap()
        };
        let buffer = |stitcher: &super::GpuStitcher| {
            stitcher
                .resources
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .output_buffer
                .clone()
        };
        let uncorrected = render(&stitcher);
        let original_buffer = buffer(&stitcher);
        stitcher.set_color_lut(None);
        assert_eq!(buffer(&stitcher), original_buffer);
        assert_eq!(render(&stitcher).as_rgb8(), uncorrected.as_rgb8());
        let red = Arc::new(
            crate::color::CubeLut::parse_cube(
                format!("LUT_3D_SIZE 2\n{}", "1 0 0\n".repeat(8)).as_bytes(),
            )
            .unwrap(),
        );
        stitcher.set_color_lut(Some(Arc::clone(&red)));
        assert!(stitcher.resources.lock().unwrap().is_none());
        let corrected = render(&stitcher);
        assert!(corrected
            .as_rgb8()
            .chunks_exact(3)
            .all(|pixel| pixel == [255, 0, 0]));
        let corrected_buffer = buffer(&stitcher);
        assert_ne!(corrected_buffer, original_buffer);
        stitcher.set_color_lut(Some(Arc::clone(&red)));
        assert_eq!(buffer(&stitcher), corrected_buffer);
        assert_eq!(render(&stitcher).as_rgb8(), corrected.as_rgb8());
        let blue = Arc::new(
            crate::color::CubeLut::parse_cube(
                format!("LUT_3D_SIZE 2\n{}", "0 0 1\n".repeat(8)).as_bytes(),
            )
            .unwrap(),
        );
        stitcher.set_color_lut(Some(blue));
        assert!(stitcher.resources.lock().unwrap().is_none());
        assert!(render(&stitcher)
            .as_rgb8()
            .chunks_exact(3)
            .all(|pixel| pixel == [0, 0, 255]));
        stitcher.set_color_lut(None);
        assert!(stitcher.resources.lock().unwrap().is_none());
        assert_eq!(render(&stitcher).as_rgb8(), uncorrected.as_rgb8());
    }

    #[test]
    fn frame_allocation_validation_returns_an_error_and_allows_recovery() {
        if super::available_adapters().is_empty() {
            assert!(
                std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_none(),
                "GPU is required"
            );
            return;
        }
        let stitcher = super::GpuStitcher::new().expect("GPU renderer");
        let key = super::GpuResourceKey {
            mask_bytes: 32,
            input_kind: 0,
            output_kind: super::GpuOutputKind::Rgb,
            lens_width: 2,
            lens_height: 2,
            output_width: 2,
            output_height: 1,
        };
        // A three-byte storage binding cannot contain the shader's u32 pixel.
        // This exercises real wgpu validation without exhausting GPU memory.
        let error = match stitcher.create_frame_resources(key, 3) {
            Ok(_) => panic!("invalid GPU allocation must fail"),
            Err(error) => error,
        };
        assert!(matches!(error, crate::Error::GpuUnavailable(_)));
        stitcher
            .create_frame_resources(key, 8)
            .expect("all scopes drained after failed allocation");
    }

    use crate::EquirectangularProjection;

    #[test]
    fn yuv_dispatch_counts_workgroups_not_shader_invocations() {
        assert_eq!(
            super::yuv_dispatch_size(EquirectangularProjection {
                width: 1_920,
                height: 960,
            }),
            [15, 60]
        );
    }

    #[test]
    fn recycled_yuv_download_reuses_the_returned_allocation() {
        let source = vec![42_u8; 1_024];
        let mut scratch = Vec::with_capacity(source.len());
        let first = super::copy_into_recycled_buffer(&mut scratch, &source);
        let allocation = first.as_ptr();
        assert!(scratch.is_empty());

        super::recycle_download_buffer(&mut scratch, first);
        let second = super::copy_into_recycled_buffer(&mut scratch, &source);

        assert_eq!(second.as_ptr(), allocation);
        assert_eq!(second, source);
    }

    #[test]
    fn stitch_shader_parses_and_validates_without_an_adapter() {
        let module = wgpu::naga::front::wgsl::parse_str(include_str!("stitch_gpu.wgsl"))
            .expect("stitch WGSL must parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("stitch WGSL must validate");
    }
}
