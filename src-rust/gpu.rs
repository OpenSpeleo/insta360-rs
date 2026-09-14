//! Portable wgpu stitch renderer and backend discovery.

use std::sync::{mpsc, Arc, Mutex};

mod underwater;
pub(crate) use underwater::GpuUnderwaterProcessor;
use underwater::{UnderwaterPipelines, UnderwaterResources};

use bytemuck::{Pod, Zeroable};

use crate::calibration::LensProjectionModel;
use crate::color::CubeLut;
use crate::motion::readout::MAX_READOUT_POSES;
use crate::motion::{FrameMotion, ReadoutPoseTable};
use crate::stitch::{MaskCache, PreparedMasks, PreparedStitchPlan};
use crate::{
    EquirectangularProjection, Error, GpuAdapterInfo, GpuFailure, GpuFailureCode, GpuFailureStage,
    LensFrame, Orientation, PanoramaFrame, ResolvedCalibration, Result, StitchEngine,
};

const WORKGROUP_WIDTH: u32 = 16;
const WORKGROUP_HEIGHT: u32 = 8;
const RGB_CHANNELS: usize = 3;
const RGBA_CHANNELS: usize = 4;

pub use crate::stitch::{
    ChromaLocation as GpuChromaLocation, Nv12Frame as GpuNv12Frame, Plane as GpuPlane,
    Yuv420Frame as GpuYuv420Frame, YuvMatrix as GpuYuvMatrix, YuvRange as GpuYuvRange,
};

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
pub(crate) enum GpuOutputKind {
    Rgb,
    Yuv420,
}

pub(crate) struct GpuOutputRequest<'a> {
    pub(crate) kind: GpuOutputKind,
    pub(crate) underwater: Option<&'a mut dyn GpuUnderwaterProcessor>,
}

impl From<GpuOutputKind> for GpuOutputRequest<'_> {
    fn from(kind: GpuOutputKind) -> Self {
        Self {
            kind,
            underwater: None,
        }
    }
}

pub(crate) enum GpuRenderedFrame {
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
pub(crate) enum GpuSourceFrames<'a> {
    Rgb(&'a [LensFrame; 2]),
    Yuv420(&'a [GpuYuv420Frame<'a>; 2]),
    Nv12(&'a [GpuNv12Frame<'a>; 2]),
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
            Self::Nv12(frames) => {
                std::array::from_fn(|index| (frames[index].width, frames[index].height))
            }
        }
    }

    fn input_kind(self) -> u32 {
        match self {
            Self::Rgb(_) => 0,
            Self::Yuv420(_) => 1,
            Self::Nv12(_) => 2,
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
    rgb_pack_pipeline: wgpu::ComputePipeline,
    rgb_upload_pipeline: wgpu::ComputePipeline,
    underwater_pipelines: UnderwaterPipelines,
    sampler: wgpu::Sampler,
    adapter: GpuAdapterInfo,
    resources: Mutex<Option<GpuFrameResources>>,
    masks: Arc<MaskCache>,
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
    correction_bytes: u64,
}

struct GpuFrameResources {
    key: GpuResourceKey,
    _source_textures: [wgpu::Texture; 2],
    yuv_textures: [[wgpu::Texture; 3]; 2],
    frame_buffer: wgpu::Buffer,
    readout_buffer: wgpu::Buffer,
    lens_buffer: wgpu::Buffer,
    mask_buffer: wgpu::Buffer,
    correction_buffer: wgpu::Buffer,
    prepared_masks: Option<Arc<PreparedMasks>>,
    _slopes_buffer: wgpu::Buffer,
    _second_stats_buffer: wgpu::Buffer,
    _output_buffer: wgpu::Buffer,
    readback: wgpu::Buffer,
    packed_output_buffer: wgpu::Buffer,
    stitch_bind_group: wgpu::BindGroup,
    radiometry_first_bind_group: wgpu::BindGroup,
    radiometry_second_bind_group: wgpu::BindGroup,
    radiometry_means_bind_group: wgpu::BindGroup,
    radiometry_slopes_bind_group: wgpu::BindGroup,
    output_bind_group: wgpu::BindGroup,
    rgb_uploads: Option<[RgbUpload; 2]>,
    yuv_download: Vec<u8>,
    underwater: Option<UnderwaterResources>,
}

/// One bounded strip buffer per lens; dimensions do not change between frames.
struct RgbUpload {
    pixels: wgpu::Buffer,
    params: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    rows: u32,
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
        let rgb_pack_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("insta360-rs RGB24 packing pass"),
            layout: None,
            module: &shader,
            entry_point: Some("pack_rgb24"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let rgb_upload_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("insta360-rs RGB24 upload expansion"),
                layout: None,
                module: &shader,
                entry_point: Some("expand_rgb24"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let underwater_pipelines = UnderwaterPipelines::new(&device);
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
            rgb_pack_pipeline,
            rgb_upload_pipeline,
            underwater_pipelines,
            sampler,
            adapter: adapter_info,
            resources: Mutex::new(None),
            masks: Arc::new(MaskCache::default()),
            color_lut: None,
        })
    }

    /// Adapter used by this renderer.
    pub fn adapter_info(&self) -> &GpuAdapterInfo {
        &self.adapter
    }

    #[cfg(feature = "media")]
    pub(crate) fn mask_cache(&self) -> Arc<MaskCache> {
        Arc::clone(&self.masks)
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
        self.stitch_with_motion_and_plan(lenses, calibration, projection, motion, None)
    }

    /// Renders using the retained correction for this exact source pair.
    pub fn stitch_with_motion_and_plan(
        &self,
        lenses: &[LensFrame; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        plan: Option<&PreparedStitchPlan>,
    ) -> Result<PanoramaFrame> {
        match self.stitch_sources(
            GpuSourceFrames::Rgb(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Rgb.into(),
            plan,
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
        self.stitch_yuv420_with_motion_and_plan(lenses, calibration, projection, motion, None)
    }

    /// Renders using the retained correction for this exact source pair.
    pub fn stitch_yuv420_with_motion_and_plan(
        &self,
        lenses: &[GpuYuv420Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        plan: Option<&PreparedStitchPlan>,
    ) -> Result<PanoramaFrame> {
        match self.stitch_sources(
            GpuSourceFrames::Yuv420(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Rgb.into(),
            plan,
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
        self.stitch_yuv420_to_yuv420_with_motion_and_plan(
            lenses,
            calibration,
            projection,
            motion,
            None,
        )
    }

    /// Renders using the retained correction for this exact source pair.
    pub fn stitch_yuv420_to_yuv420_with_motion_and_plan(
        &self,
        lenses: &[GpuYuv420Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        plan: Option<&PreparedStitchPlan>,
    ) -> Result<GpuYuv420Output> {
        match self.stitch_sources(
            GpuSourceFrames::Yuv420(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Yuv420.into(),
            plan,
        )? {
            GpuRenderedFrame::Yuv420(frame) => Ok(frame),
            GpuRenderedFrame::Rgb(_) => unreachable!("YUV output requested above"),
        }
    }

    /// Stitches NV12 directly with the same calibration, color and motion policy
    /// as planar YUV420, without full-resolution CPU RGB conversion or packing.
    pub fn stitch_nv12_with_motion(
        &self,
        lenses: &[GpuNv12Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<PanoramaFrame> {
        self.stitch_nv12_with_motion_and_plan(lenses, calibration, projection, motion, None)
    }

    /// Renders using the retained correction for this exact source pair.
    pub fn stitch_nv12_with_motion_and_plan(
        &self,
        lenses: &[GpuNv12Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        plan: Option<&PreparedStitchPlan>,
    ) -> Result<PanoramaFrame> {
        match self.stitch_sources(
            GpuSourceFrames::Nv12(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Rgb.into(),
            plan,
        )? {
            GpuRenderedFrame::Rgb(frame) => Ok(frame),
            GpuRenderedFrame::Yuv420(_) => unreachable!("RGB output requested above"),
        }
    }

    /// Stitches NV12 directly to encoder-ready limited-range BT.709 YUV420.
    pub fn stitch_nv12_to_yuv420_with_motion(
        &self,
        lenses: &[GpuNv12Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
    ) -> Result<GpuYuv420Output> {
        self.stitch_nv12_to_yuv420_with_motion_and_plan(
            lenses,
            calibration,
            projection,
            motion,
            None,
        )
    }

    /// Renders using the retained correction for this exact source pair.
    pub fn stitch_nv12_to_yuv420_with_motion_and_plan(
        &self,
        lenses: &[GpuNv12Frame<'_>; 2],
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        plan: Option<&PreparedStitchPlan>,
    ) -> Result<GpuYuv420Output> {
        match self.stitch_sources(
            GpuSourceFrames::Nv12(lenses),
            calibration,
            projection,
            motion,
            GpuOutputKind::Yuv420.into(),
            plan,
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

    pub(crate) fn stitch_sources(
        &self,
        sources: GpuSourceFrames<'_>,
        calibration: &ResolvedCalibration,
        projection: EquirectangularProjection,
        motion: &FrameMotion,
        mut output: GpuOutputRequest<'_>,
        plan: Option<&PreparedStitchPlan>,
    ) -> Result<GpuRenderedFrame> {
        let projection = projection.validate()?;
        let output_kind = output.kind;
        if output.underwater.as_ref().is_some_and(|processor| {
            processor.dimensions() != (projection.width, projection.height)
        }) {
            return Err(Error::InvalidMedia(
                "GPU underwater session dimensions do not match the panorama".into(),
            ));
        }
        calibration.validate_for_stitching()?;
        let dimensions = sources.dimensions();
        validate_source_frames(sources)?;
        if let Some(plan) = plan {
            plan.validate(calibration, dimensions, motion)?;
        }
        let correction = plan.map_or_else(|| vec![[0.0; 4]; 3], PreparedStitchPlan::gpu_data);
        validate_device_limits(&self.device, dimensions, projection, &self.adapter)?;

        let fisheye_masks = self.masks.prepare(dimensions, calibration)?;
        let mask_layout = mask_buffer_layout(
            fisheye_masks
                .each_ref()
                .map(|mask| mask.as_ref().map(|mask| (mask.width, mask.height))),
        );
        let mask_bytes = mask_layout.bytes;
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
            correction_bytes: (correction.len().max(3) * 16) as u64,
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
                let uploads = resources
                    .rgb_uploads
                    .as_ref()
                    .expect("RGB upload resources");
                for (upload, lens) in uploads.iter().zip(lenses) {
                    self.upload_rgb(upload, lens);
                }
            }
            GpuSourceFrames::Yuv420(lenses) => {
                for (lens_index, lens) in lenses.iter().enumerate() {
                    upload_yuv420_frame(&self.queue, &resources.yuv_textures[lens_index], lens);
                }
            }
            GpuSourceFrames::Nv12(lenses) => {
                for (lens_index, lens) in lenses.iter().enumerate() {
                    upload_nv12_frame(&self.queue, &resources.yuv_textures[lens_index], lens);
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
        self.queue.write_buffer(
            &resources.correction_buffer,
            0,
            bytemuck::cast_slice(&correction),
        );
        if resources
            .prepared_masks
            .as_ref()
            .is_none_or(|previous| !Arc::ptr_eq(previous, &fisheye_masks))
        {
            for (index, mask) in fisheye_masks.iter().enumerate() {
                if let Some(mask) = mask {
                    self.queue.write_buffer(
                        &resources.mask_buffer,
                        16 + u64::from(mask_layout.offsets[index]) * 4,
                        bytemuck::cast_slice(&mask.weights),
                    );
                    let chroma = pack_chroma_support(&mask.weights, mask.width, mask.height);
                    self.queue.write_buffer(
                        &resources.mask_buffer,
                        16 + u64::from(mask_layout.offsets[index + 2]) * 4,
                        bytemuck::cast_slice(&chroma),
                    );
                }
            }
            self.queue.write_buffer(
                &resources.mask_buffer,
                0,
                bytemuck::cast_slice(&mask_layout.offsets),
            );
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
        if let Some(processor) = output.underwater.as_deref_mut() {
            encoder = match self.process_underwater(resources, encoder, processor) {
                Ok(encoder) => encoder,
                Err(error) => {
                    // Analysis/inference can fail before the final submission.
                    // Drain scopes so the next frame never inherits stale state.
                    let scope_result = check_submission_errors(
                        pollster::block_on(validation.pop()),
                        pollster::block_on(out_of_memory.pop()),
                        &self.adapter,
                    );
                    resources.underwater = None;
                    scope_result?;
                    return Err(error);
                }
            };
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("insta360-rs output packing pass"),
                timestamp_writes: None,
            });
            let (pipeline, workgroups) = match output_kind {
                GpuOutputKind::Rgb => (&self.rgb_pack_pipeline, rgb_dispatch_size(projection)),
                GpuOutputKind::Yuv420 => (&self.rgb_to_yuv_pipeline, yuv_dispatch_size(projection)),
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &resources.output_bind_group, &[]);
            pass.dispatch_workgroups(workgroups[0], workgroups[1], 1);
        }
        let readback = &resources.readback;
        encoder.copy_buffer_to_buffer(
            &resources.packed_output_buffer,
            0,
            readback,
            0,
            resources.packed_output_buffer.size(),
        );
        let submission = self.queue.submit([encoder.finish()]);
        if let Err(error) = check_submission_errors(
            pollster::block_on(validation.pop()),
            pollster::block_on(out_of_memory.pop()),
            &self.adapter,
        ) {
            resources.underwater = None;
            return Err(error);
        }

        // End every mapping attempt, including poll/callback/range failures.
        // The mapped view stays inside this scope and is dropped before unmap.
        let rendered = (|| -> Result<GpuRenderedFrame> {
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
                    let rgb_len = usize::try_from(output_size / RGBA_CHANNELS as u64)
                        .ok()
                        .and_then(|pixels| pixels.checked_mul(RGB_CHANNELS))
                        .ok_or_else(|| {
                            Error::InvalidMedia("GPU panorama size overflowed".into())
                        })?;
                    // The GPU has already removed alpha with integer byte operations.
                    // Only one bulk copy remains; padded tail words are not public pixels.
                    let rgb = mapped[..rgb_len].to_vec();
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
                        u_offset: usize::try_from(yuv_layout.u_offset).map_err(|_| {
                            Error::InvalidMedia("GPU YUV U offset overflowed".into())
                        })?,
                        v_offset: usize::try_from(yuv_layout.v_offset).map_err(|_| {
                            Error::InvalidMedia("GPU YUV V offset overflowed".into())
                        })?,
                        data,
                    })
                }
            };
            drop(mapped);
            Ok(rendered)
        })();
        readback.unmap();
        if rendered.is_err() {
            resources.underwater = None;
        }
        let rendered = rendered?;
        if let Some(processor) = output.underwater {
            processor.commit();
        }
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
                wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            )
        });
        let source_views = source_textures
            .each_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let yuv_textures = std::array::from_fn(|_| {
            std::array::from_fn(|plane| {
                let size = if key.input_kind == 1 || key.input_kind == 2 && plane < 2 {
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
                    if key.input_kind == 2 && plane == 1 {
                        wgpu::TextureFormat::Rg8Unorm
                    } else {
                        wgpu::TextureFormat::R8Unorm
                    },
                    wgpu::TextureUsages::empty(),
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
        let correction_buffer = dynamic_buffer(
            &self.device,
            "insta360-rs prepared correction",
            key.correction_bytes,
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
        let packed_size = packed_output_size(
            EquirectangularProjection {
                width: key.output_width,
                height: key.output_height,
            },
            key.output_kind,
        )?;
        let packed_output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("insta360-rs packed RGB/YUV panorama"),
            size: packed_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("insta360-rs packed panorama readback"),
            size: packed_size,
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
                binding(21, correction_buffer.as_entire_binding()),
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
        let output_layout = match key.output_kind {
            GpuOutputKind::Rgb => &self.rgb_pack_pipeline,
            GpuOutputKind::Yuv420 => &self.rgb_to_yuv_pipeline,
        }
        .get_bind_group_layout(0);
        let output_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("insta360-rs output packing bindings"),
            layout: &output_layout,
            entries: &[
                binding(0, frame_buffer.as_entire_binding()),
                binding(7, output_buffer.as_entire_binding()),
                binding(15, packed_output_buffer.as_entire_binding()),
            ],
        });
        let rgb_uploads = if key.input_kind == 0 {
            let limits = self.device.limits();
            let rows = rgb_upload_rows(
                key.lens_width,
                key.lens_height,
                limits
                    .max_buffer_size
                    .min(limits.max_storage_buffer_binding_size),
            )?;
            Some([
                self.create_rgb_upload(&source_views[0], key.lens_width, rows)?,
                self.create_rgb_upload(&source_views[1], key.lens_width, rows)?,
            ])
        } else {
            None
        };
        Ok(GpuFrameResources {
            key,
            _source_textures: source_textures,
            yuv_textures,
            frame_buffer,
            readout_buffer,
            lens_buffer,
            mask_buffer,
            correction_buffer,
            prepared_masks: None,
            _slopes_buffer: slopes_buffer,
            _second_stats_buffer: second_stats_buffer,
            _output_buffer: output_buffer,
            readback,
            packed_output_buffer,
            stitch_bind_group,
            radiometry_first_bind_group,
            radiometry_second_bind_group,
            radiometry_means_bind_group,
            radiometry_slopes_bind_group,
            output_bind_group,
            rgb_uploads,
            yuv_download: Vec::new(),
            underwater: None,
        })
    }

    fn create_rgb_upload(
        &self,
        texture: &wgpu::TextureView,
        width: u32,
        rows: u32,
    ) -> Result<RgbUpload> {
        let size = u64::from(width)
            .checked_mul(u64::from(rows))
            .and_then(|pixels| pixels.checked_mul(RGB_CHANNELS as u64))
            .and_then(|bytes| bytes.checked_add(3))
            .map(|bytes| bytes / 4 * 4)
            .ok_or_else(|| Error::InvalidMedia("GPU RGB upload size overflowed".into()))?;
        let pixels = dynamic_buffer(
            &self.device,
            "insta360-rs packed RGB upload",
            size,
            wgpu::BufferUsages::STORAGE,
        );
        let params = dynamic_buffer(
            &self.device,
            "insta360-rs RGB upload strip",
            16,
            wgpu::BufferUsages::UNIFORM,
        );
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("insta360-rs RGB upload bindings"),
            layout: &self.rgb_upload_pipeline.get_bind_group_layout(0),
            entries: &[
                binding(18, pixels.as_entire_binding()),
                binding(19, wgpu::BindingResource::TextureView(texture)),
                binding(20, params.as_entire_binding()),
            ],
        });
        Ok(RgbUpload {
            pixels,
            params,
            bindings,
            rows,
        })
    }

    fn upload_rgb(&self, upload: &RgbUpload, frame: &LensFrame) {
        let row_bytes = frame.width() as usize * RGB_CHANNELS;
        for origin in (0..frame.height()).step_by(upload.rows as usize) {
            let rows = upload.rows.min(frame.height() - origin);
            let start = origin as usize * row_bytes;
            let end = start + rows as usize * row_bytes;
            write_rgb_bytes(&self.queue, &upload.pixels, &frame.as_rgb8()[start..end]);
            self.queue.write_buffer(
                &upload.params,
                0,
                bytemuck::cast_slice(&[origin, rows, 0, 0]),
            );
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("insta360-rs RGB upload commands"),
                });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("insta360-rs RGB upload pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.rgb_upload_pipeline);
                pass.set_bind_group(0, &upload.bindings, &[]);
                pass.dispatch_workgroups(
                    frame.width().div_ceil(WORKGROUP_WIDTH),
                    rows.div_ceil(WORKGROUP_HEIGHT),
                    1,
                );
            }
            // Queue order protects a reused strip buffer. The final stitch submission
            // waits for these uploads without introducing a CPU/GPU wait per strip.
            self.queue.submit([encoder.finish()]);
        }
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
    let (kind, range, matrix, chroma) = match sources {
        GpuSourceFrames::Rgb(_) => return [0.0, 0.0],
        GpuSourceFrames::Yuv420(frames) => {
            let frame = frames[lens_index];
            (1.0, frame.range, frame.matrix, frame.chroma_location)
        }
        GpuSourceFrames::Nv12(frames) => {
            let frame = frames[lens_index];
            (2.0, frame.range, frame.matrix, frame.chroma_location)
        }
    };
    let range = match range {
        GpuYuvRange::Limited => 0_u32,
        GpuYuvRange::Full => 1,
    };
    let matrix = match matrix {
        GpuYuvMatrix::Bt601 => 0_u32,
        GpuYuvMatrix::Bt709 => 1,
        GpuYuvMatrix::Bt2020 => 2,
    };
    let chroma = match chroma {
        GpuChromaLocation::Left => 0_u32,
        GpuChromaLocation::Center => 1,
    };
    [kind, (range | (matrix << 1) | (chroma << 3)) as f32]
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
        upload_plane(queue, &textures[plane_index], plane, width, height);
    }
}

fn upload_nv12_frame(queue: &wgpu::Queue, textures: &[wgpu::Texture; 3], frame: &GpuNv12Frame<'_>) {
    upload_plane(queue, &textures[0], frame.y, frame.width, frame.height);
    upload_plane(
        queue,
        &textures[1],
        frame.uv,
        frame.width.div_ceil(2),
        frame.height.div_ceil(2),
    );
}

fn upload_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    plane: GpuPlane<'_>,
    width: u32,
    height: u32,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
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

fn create_sampled_texture(
    device: &wgpu::Device,
    label: &'static str,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    extra_usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | extra_usage,
        view_formats: &[],
    })
}

struct MaskBufferLayout {
    // Word offsets after the header: first/second luma, first/second chroma.
    offsets: [u32; 4],
    bytes: u64,
}

fn chroma_support_words(width: usize, height: usize) -> usize {
    (width.div_ceil(2) * height.div_ceil(2)).div_ceil(u32::BITS as usize)
}

// Dimensions come from validated MaskCache entries (at most 64M total pixels).
// Luma retains f32 bits; each chroma cell needs only one support bit.
fn mask_buffer_layout(dimensions: [Option<(usize, usize)>; 2]) -> MaskBufferLayout {
    let mut offsets = [u32::MAX, u32::MAX, 0, 0];
    let mut words = 0_u32;
    for (index, dimensions) in dimensions.into_iter().enumerate() {
        if let Some((width, height)) = dimensions {
            offsets[index] = words;
            words += (width * height) as u32;
            offsets[index + 2] = words;
            words += chroma_support_words(width, height) as u32;
        }
    }
    MaskBufferLayout {
        offsets,
        // WGSL rounds the header plus runtime-array minimum to vec4 alignment.
        bytes: 16 + u64::from(words.max(4)) * 4,
    }
}

fn pack_chroma_support(weights: &[f32], width: usize, height: usize) -> Vec<u32> {
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let mut words = vec![0_u32; chroma_support_words(width, height)];
    for y in 0..chroma_height {
        for x in 0..chroma_width {
            if (y * 2..(y * 2 + 2).min(height))
                .all(|yy| (x * 2..(x * 2 + 2).min(width)).all(|xx| weights[yy * width + xx] > 0.0))
            {
                let cell = y * chroma_width + x;
                words[cell / u32::BITS as usize] |= 1 << (cell % u32::BITS as usize);
            }
        }
    }
    words
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

fn rgb_upload_rows(width: u32, height: u32, limit: u64) -> Result<u32> {
    let row_bytes = u64::from(width) * RGB_CHANNELS as u64;
    let rows = (limit / 4 * 4)
        .checked_div(row_bytes)
        .unwrap_or(0)
        .min(u64::from(height));
    if rows == 0 {
        return Err(Error::InvalidMedia(
            "a GPU RGB upload row exceeds buffer limits".into(),
        ));
    }
    Ok(rows as u32)
}

/// Bulk-copy packed bytes; only the final partial storage word needs padding.
fn write_rgb_bytes(queue: &wgpu::Queue, buffer: &wgpu::Buffer, rgb: &[u8]) {
    let aligned = rgb.len() / 4 * 4;
    if aligned != 0 {
        queue.write_buffer(buffer, 0, &rgb[..aligned]);
    }
    let tail = &rgb[aligned..];
    if !tail.is_empty() {
        let mut word = [0_u8; 4];
        word[..tail.len()].copy_from_slice(tail);
        queue.write_buffer(buffer, aligned as u64, &word);
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

// One invocation packs four consecutive RGBA pixels into three RGB words.
// The two-dimensional grid keeps large exports within per-axis dispatch limits.
fn rgb_dispatch_size(projection: EquirectangularProjection) -> [u32; 2] {
    [
        projection.width.div_ceil(WORKGROUP_WIDTH),
        projection.height.div_ceil(4).div_ceil(WORKGROUP_HEIGHT),
    ]
}

fn check_submission_errors(
    validation: Option<wgpu::Error>,
    out_of_memory: Option<wgpu::Error>,
    adapter: &GpuAdapterInfo,
) -> Result<()> {
    if let Some(error) = validation {
        return Err(gpu_processing(
            GpuFailureCode::Submission,
            GpuFailureStage::Dispatch,
            format!("submitting GPU frame commands failed: {error}"),
            adapter,
        ));
    }
    if let Some(error) = out_of_memory {
        return Err(gpu_processing(
            GpuFailureCode::OutOfMemory,
            GpuFailureStage::Dispatch,
            format!("allocating or submitting GPU frame resources failed: {error}"),
            adapter,
        ));
    }
    Ok(())
}

fn packed_output_size(projection: EquirectangularProjection, kind: GpuOutputKind) -> Result<u64> {
    match kind {
        GpuOutputKind::Rgb => (output_buffer_size(projection)? / RGBA_CHANNELS as u64)
            .div_ceil(4)
            .checked_mul(12)
            .ok_or_else(|| Error::InvalidMedia("GPU packed RGB size overflowed".into())),
        GpuOutputKind::Yuv420 => Ok(yuv_output_layout(projection)?.size),
    }
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
    if let GpuSourceFrames::Nv12(frames) = sources {
        for (lens_index, frame) in frames.iter().enumerate() {
            validate_plane(lens_index, "Y", frame.y, frame.width, frame.height)?;
            let row_bytes = frame
                .width
                .div_ceil(2)
                .checked_mul(2)
                .ok_or_else(|| Error::InvalidMedia("NV12 chroma row size overflowed".into()))?;
            if !frame.uv.stride.is_multiple_of(2) {
                return Err(Error::InvalidMedia(
                    "NV12 chroma stride must contain complete UV texels".into(),
                ));
            }
            validate_plane(
                lens_index,
                "UV",
                frame.uv,
                row_bytes,
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
    fn packed_chroma_matches_complete_luma_footprints_and_clears_tail_bits() {
        for width in [1_usize, 2, 3, 5, 63, 64, 65] {
            for height in [1_usize, 2, 3, 7] {
                for pattern in 0..16_u32 {
                    let weights: Vec<f32> = (0..width * height)
                        .map(|pixel| {
                            let position = (pixel / width % 2) * 2 + pixel % width % 2;
                            if pattern & (1 << position) == 0 {
                                0.0
                            } else {
                                // Positive feather values must retain support.
                                (position + 1) as f32 * 1.0e-6
                            }
                        })
                        .collect();
                    let words = super::pack_chroma_support(&weights, width, height);
                    let chroma_width = width.div_ceil(2);
                    let cells = chroma_width * height.div_ceil(2);
                    assert_eq!(words.len(), cells.div_ceil(32));
                    // Independent reference: each excluded luma pixel clears
                    // its owning chroma cell, including partial edge cells.
                    let mut expected = vec![true; cells];
                    for (pixel, &weight) in weights.iter().enumerate() {
                        if weight <= 0.0 {
                            expected[(pixel / width / 2) * chroma_width + pixel % width / 2] =
                                false;
                        }
                    }
                    for bit in 0..words.len() * 32 {
                        assert_eq!(
                            words[bit / 32] & (1 << (bit % 32)) != 0,
                            expected.get(bit).copied().unwrap_or(false),
                            "{width}x{height} pattern={pattern} bit={bit}",
                        );
                    }
                }
            }
        }
        // This bit pattern is NaN as f32: it must stay an integer on the GPU.
        assert_eq!(super::pack_chroma_support(&[1.0; 128], 64, 2), [u32::MAX]);
    }

    #[test]
    fn mask_buffer_layout_uses_word_offsets_and_handles_absent_lenses() {
        let empty = super::mask_buffer_layout([None, None]);
        assert_eq!(empty.offsets, [u32::MAX, u32::MAX, 0, 0]);
        assert_eq!(empty.bytes, 32);

        let second_only = super::mask_buffer_layout([None, Some((3, 5))]);
        assert_eq!(second_only.offsets, [u32::MAX, 0, 0, 15]);
        assert_eq!(second_only.bytes, 80);

        let pair = super::mask_buffer_layout([Some((3, 5)), Some((5, 1))]);
        assert_eq!(pair.offsets, [0, 16, 15, 21]);
        assert_eq!(pair.bytes, 104);
    }

    #[test]
    fn eight_k_source_mask_pair_fits_default_storage_binding_limit() {
        // No image allocation: exercise the production sizing/offset path.
        let layout = super::mask_buffer_layout([Some((3840, 3840)); 2]);
        assert_eq!(layout.offsets, [0, 14_860_800, 14_745_600, 29_606_400]);
        assert_eq!(layout.bytes, 118_886_416);
        assert!(layout.bytes <= 128 * 1024 * 1024);
        // A float per chroma cell caused this otherwise-supported size to fail.
        let float_support_bytes = 16 + 2 * (3840_u64 * 3840 + 1920 * 1920) * 4;
        assert_eq!(float_support_bytes, 147_456_016);
        assert!(float_support_bytes > 128 * 1024 * 1024);
    }

    #[test]
    fn complete_chroma_footprints_contain_every_luma_interpolation_tap() {
        // Prove the fast-path implication independently in each axis. Its
        // Cartesian product covers the 2D footprint. Include odd sizes, both
        // sitings, all edge clamps and subpixel intervals around half centers.
        for length in 1_usize..=65 {
            let chroma_length = length.div_ceil(2);
            for offset in [0.0, 0.5] {
                for position in 0..=(length - 1) * 16 {
                    let source = position as f64 / 16.0;
                    let chroma = ((source - offset) / 2.0).clamp(0.0, (chroma_length - 1) as f64);
                    let mut covered = vec![false; length];
                    for cell in [chroma.floor() as usize, chroma.ceil() as usize] {
                        covered[cell * 2..((cell + 1) * 2).min(length)].fill(true);
                    }
                    for luma in [source.floor() as usize, source.ceil() as usize] {
                        assert!(
                            covered[luma],
                            "size={length} offset={offset} source={source} missing luma={luma}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rgb_upload_strip_capacity_includes_storage_word_padding() {
        assert_eq!(super::rgb_upload_rows(65, 5, 392).unwrap(), 2);
        assert_eq!(super::rgb_upload_rows(1, 3, 7).unwrap(), 1);
        assert_eq!(super::rgb_upload_rows(65, 5, 2_000).unwrap(), 5);
        assert!(super::rgb_upload_rows(65, 5, 194).is_err());
        assert!(super::rgb_upload_rows(0, 5, 1024).is_err());
    }

    #[test]
    fn rgb_gpu_upload_preserves_every_byte_across_odd_tails_and_reused_strips() {
        if super::available_adapters().is_empty() {
            assert!(
                std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_none(),
                "GPU is required"
            );
            return;
        }
        let gpu = super::GpuStitcher::new().unwrap();
        for (width, height) in [(1, 1), (2, 3), (3, 5), (65, 5), (257, 7)] {
            let size = wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            };
            let texture = super::create_sampled_texture(
                &gpu.device,
                "test expanded RGB",
                size,
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            // Force several strips even for tiny images: queue ordering must keep
            // earlier data/uniform writes alive until their expansion completes.
            for strip_rows in [1, 2, height] {
                let upload = gpu
                    .create_rgb_upload(&view, width, strip_rows.min(height))
                    .unwrap();
                let row_bytes = width * 4;
                let stride = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                    * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("test RGB upload readback"),
                    size: u64::from(stride) * u64::from(height),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                for seed in [17_u8, 231, 0] {
                    let rgb: Vec<u8> = (0..width as usize * height as usize * 3)
                        .map(|index| (index as u8).wrapping_mul(37).wrapping_add(seed))
                        .collect();
                    let expected: Vec<u8> = rgb
                        .chunks_exact(3)
                        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
                        .collect();
                    let frame = super::LensFrame::new(width, height, rgb.clone()).unwrap();
                    gpu.upload_rgb(&upload, &frame);
                    let mut encoder = gpu
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    encoder.copy_texture_to_buffer(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyBufferInfo {
                            buffer: &readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(stride),
                                rows_per_image: Some(height),
                            },
                        },
                        size,
                    );
                    let submission = gpu.queue.submit([encoder.finish()]);
                    let slice = readback.slice(..);
                    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                    slice.map_async(wgpu::MapMode::Read, move |result| {
                        sender.send(result).unwrap();
                    });
                    gpu.device
                        .poll(wgpu::PollType::Wait {
                            submission_index: Some(submission),
                            timeout: None,
                        })
                        .unwrap();
                    receiver.recv().unwrap().unwrap();
                    let mapped = slice.get_mapped_range().unwrap();
                    for (row, expected) in mapped
                        .chunks_exact(stride as usize)
                        .zip(expected.chunks_exact(row_bytes as usize))
                    {
                        assert!(row[..row_bytes as usize] == *expected, "RGB upload changed bytes: {width}x{height}, strips={strip_rows}, seed={seed}");
                    }
                    assert_eq!(frame.as_rgb8(), rgb);
                    drop(mapped);
                    readback.unmap();
                }
            }
        }
    }

    #[test]
    fn packed_rgb_readback_preserves_bytes_tail_pixels_and_reused_buffers() {
        use bytemuck::Zeroable;
        if super::available_adapters().is_empty() {
            assert!(
                std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_none(),
                "GPU is required"
            );
            return;
        }
        let stitcher = super::GpuStitcher::new().expect("GPU renderer");
        for (width, height) in [(1, 1), (2, 1), (3, 1), (4, 1), (5, 3), (33, 17)] {
            let projection = super::EquirectangularProjection { width, height };
            let rgba_len = super::output_buffer_size(projection).unwrap();
            let resources = stitcher
                .create_frame_resources(
                    super::GpuResourceKey {
                        mask_bytes: 32,
                        correction_bytes: 48,
                        input_kind: 0,
                        output_kind: super::GpuOutputKind::Rgb,
                        lens_width: 2,
                        lens_height: 2,
                        output_width: width,
                        output_height: height,
                    },
                    rgba_len,
                )
                .unwrap();
            let source = super::dynamic_buffer(
                &stitcher.device,
                "test RGBA pixels",
                rgba_len,
                wgpu::BufferUsages::STORAGE,
            );
            let params = super::FrameParams {
                output_size: [width, height],
                ..super::FrameParams::zeroed()
            };
            stitcher
                .queue
                .write_buffer(&resources.frame_buffer, 0, bytemuck::bytes_of(&params));
            let bindings = stitcher
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("test RGB packing bindings"),
                    layout: &stitcher.rgb_pack_pipeline.get_bind_group_layout(0),
                    entries: &[
                        super::binding(0, resources.frame_buffer.as_entire_binding()),
                        super::binding(7, source.as_entire_binding()),
                        super::binding(15, resources.packed_output_buffer.as_entire_binding()),
                    ],
                });
            for seed in [17_u8, 231, 0] {
                // Include arbitrary alpha and all channel values; only alpha may disappear.
                let rgba: Vec<u8> = (0..rgba_len as usize)
                    .map(|index| (index as u8).wrapping_mul(37).wrapping_add(seed))
                    .collect();
                let expected: Vec<u8> = rgba
                    .chunks_exact(4)
                    .flat_map(|pixel| pixel[..3].iter().copied())
                    .collect();
                stitcher.queue.write_buffer(&source, 0, &rgba);
                let mut encoder = stitcher
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                {
                    let mut pass =
                        encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                    pass.set_pipeline(&stitcher.rgb_pack_pipeline);
                    pass.set_bind_group(0, &bindings, &[]);
                    let groups = super::rgb_dispatch_size(projection);
                    pass.dispatch_workgroups(groups[0], groups[1], 1);
                }
                encoder.copy_buffer_to_buffer(
                    &resources.packed_output_buffer,
                    0,
                    &resources.readback,
                    0,
                    resources.readback.size(),
                );
                let submission = stitcher.queue.submit([encoder.finish()]);
                let slice = resources.readback.slice(..);
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                slice.map_async(wgpu::MapMode::Read, move |result| {
                    sender.send(result).unwrap();
                });
                stitcher
                    .device
                    .poll(wgpu::PollType::Wait {
                        submission_index: Some(submission),
                        timeout: None,
                    })
                    .unwrap();
                receiver.recv().unwrap().unwrap();
                let mapped = slice.get_mapped_range().unwrap();
                assert_eq!(
                    &mapped[..expected.len()],
                    expected,
                    "{width}x{height}, seed {seed}"
                );
                assert!(
                    mapped[expected.len()..].iter().all(|byte| *byte == 0),
                    "tail bytes must be cleared on every dispatch"
                );
                drop(mapped);
                resources.readback.unmap();
            }
        }
    }

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
        let color = [64u8, 96, 128];
        let lenses = [
            LensFrame::new(16, 16, color.repeat(16 * 16)).unwrap(),
            LensFrame::new(16, 16, color.repeat(16 * 16)).unwrap(),
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
            let guard = stitcher.resources.lock().unwrap();
            let resources = guard.as_ref().unwrap();
            (
                resources._output_buffer.clone(),
                resources.packed_output_buffer.clone(),
                resources.readback.clone(),
            )
        };
        let uncorrected = render(&stitcher);
        // A constant LUT can paint even a failed, all-black projection. Qualify
        // the uncorrected renderer against the known input before testing reuse.
        for (index, pixel) in uncorrected.as_rgb8().chunks_exact(3).enumerate() {
            assert!(
                pixel.iter().zip(color).all(|(a, b)| a.abs_diff(b) <= 1),
                "uncorrected pixel {index}: {pixel:?}, expected {color:?}, adapter {:?}",
                stitcher.adapter_info()
            );
        }
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
            correction_bytes: 48,
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
