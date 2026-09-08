use std::time::Duration;

use insta360_rs::{
    BackendReport, EffectiveBackend, ExportResult, GpuAdapterInfo, GpuFailure, GpuFailureCode,
    GpuFailureStage, ProcessingBackend,
};

fn adapter() -> GpuAdapterInfo {
    GpuAdapterInfo {
        name: "test adapter".into(),
        backend: "Vulkan".into(),
        device_type: "DiscreteGpu".into(),
        vendor: 0x1234,
        device: 0x5678,
        driver: "test".into(),
        driver_info: "test driver".into(),
    }
}

#[test]
fn backend_reports_distinguish_request_selection_and_fallback() {
    let gpu = BackendReport::gpu(ProcessingBackend::Auto, adapter());
    assert_eq!(gpu.requested, ProcessingBackend::Auto);
    assert_eq!(gpu.selected, EffectiveBackend::Gpu);
    assert!(gpu.adapter.is_some());
    assert!(gpu.fallback.is_none());

    let failure = GpuFailure::new(
        GpuFailureCode::NoCompatibleAdapter,
        GpuFailureStage::Discovery,
        "no adapter",
    );
    let fallback = BackendReport::cpu_fallback(ProcessingBackend::Auto, failure.clone());
    assert_eq!(fallback.selected, EffectiveBackend::Cpu);
    assert_eq!(fallback.fallback, Some(failure));

    let cpu = BackendReport::cpu(ProcessingBackend::Cpu);
    assert_eq!(cpu.requested, ProcessingBackend::Cpu);
    assert_eq!(cpu.selected, EffectiveBackend::Cpu);
    assert!(cpu.fallback.is_none());
}

#[test]
fn export_result_deserializes_without_a_backend_report() {
    let result = ExportResult {
        outputs: Vec::new(),
        frames_written: 0,
        elapsed: Duration::ZERO,
        backend: BackendReport::cpu(ProcessingBackend::Cpu),
    };
    let mut serialized = serde_json::to_value(result).expect("serialize export result");
    serialized
        .as_object_mut()
        .expect("export result is an object")
        .remove("backend");

    let decoded: ExportResult =
        serde_json::from_value(serialized).expect("legacy result remains readable");
    assert_eq!(decoded.backend, BackendReport::default());
}

#[test]
fn gpu_failure_uses_stable_machine_readable_names() {
    let failure = GpuFailure::new(
        GpuFailureCode::DeviceLost,
        GpuFailureStage::Dispatch,
        "device reset",
    )
    .with_adapter(adapter());

    assert_eq!(failure.code.as_str(), "device_lost");
    assert_eq!(failure.stage.as_str(), "dispatch");
    assert!(failure.to_string().contains("device reset"));
    assert_eq!(
        failure.adapter.as_ref().map(|info| info.vendor),
        Some(0x1234)
    );
}

#[cfg(feature = "media")]
#[test]
fn media_gpu_capabilities_are_internally_consistent() {
    let capabilities = insta360_rs::media::MediaCapabilities::detect();
    assert_eq!(
        capabilities.gpu_available,
        !capabilities.gpu_adapters.is_empty()
    );
    if !capabilities.gpu_compiled {
        assert!(!capabilities.gpu_available);
        assert!(capabilities.gpu_unavailable_reason.is_some());
    }
}
