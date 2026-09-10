//! Typed optical and underwater configuration conversions.
use pyo3::prelude::*;

macro_rules! optical_enum {
    ($py:ident, $core:ident, $name:literal, $first:ident $(, $variant:ident)* $(,)?) => {
        #[pyclass(name=$name, module="insta360_rs._native", eq, eq_int, from_py_object, rename_all="SCREAMING_SNAKE_CASE")]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum $py { $first $(, $variant)* }
        impl From<$py> for insta360_rs::$core {
            fn from(value: $py) -> Self { match value { $py::$first => Self::$first, $($py::$variant => Self::$variant),* } }
        }
        impl From<insta360_rs::$core> for $py {
            fn from(value: insta360_rs::$core) -> Self { match value { insta360_rs::$core::$first => Self::$first, $(insta360_rs::$core::$variant => Self::$variant,)* _ => Self::$first } }
        }
    };
}
optical_enum!(
    PyHousing,
    Housing,
    "Housing",
    Auto,
    None,
    VentureCase,
    DiveCase,
    SphericalDiveCase,
    InvisibleDiveCase,
    DiveCasePro
);
optical_enum!(
    PyEnvironment,
    Environment,
    "Environment",
    Auto,
    Air,
    Underwater
);
optical_enum!(
    PyLensAccessory,
    LensAccessory,
    "LensAccessory",
    Auto,
    None,
    ClipOnLensGuard,
    AdhesiveSphereLensGuard,
    ProtectorA,
    ProtectorS,
    ProtectorAS,
    Nd16,
    Nd32,
    Nd64,
    Nd128
);
optical_enum!(
    PyMountingAccessory,
    MountingAccessory,
    "MountingAccessory",
    Auto,
    None,
    DiveBuddy
);
optical_enum!(
    PyUnderwaterColorMode,
    UnderwaterColorMode,
    "UnderwaterColorMode",
    Off,
    Legacy,
    Ai
);

#[pyclass(
    name = "UnderwaterColorOptions",
    module = "insta360_rs._native",
    frozen,
    from_py_object
)]
#[derive(Clone, Debug)]
pub(crate) struct PyUnderwaterColorOptions {
    #[pyo3(get)]
    pub mode: PyUnderwaterColorMode,
    #[pyo3(get)]
    pub strength: Option<f32>,
    #[pyo3(get)]
    pub balance: Option<f32>,
    #[pyo3(get)]
    pub style: Option<u32>,
}
impl Default for PyUnderwaterColorOptions {
    fn default() -> Self {
        Self {
            mode: PyUnderwaterColorMode::Off,
            strength: None,
            balance: None,
            style: None,
        }
    }
}
#[pymethods]
impl PyUnderwaterColorOptions {
    #[new]
    #[pyo3(signature=(*, mode=None, strength=None, balance=None, style=None))]
    fn new(
        mode: Option<PyUnderwaterColorMode>,
        strength: Option<f32>,
        balance: Option<f32>,
        style: Option<u32>,
    ) -> PyResult<Self> {
        let value = Self {
            mode: mode.unwrap_or(PyUnderwaterColorMode::Off),
            strength,
            balance,
            style,
        };
        value.to_core().map_err(super::to_py_error)?;
        Ok(value)
    }
    fn __repr__(&self) -> String {
        format!(
            "UnderwaterColorOptions(mode={:?}, strength={:?}, balance={:?}, style={:?})",
            self.mode, self.strength, self.balance, self.style
        )
    }
}
impl PyUnderwaterColorOptions {
    pub fn to_core(&self) -> insta360_rs::Result<insta360_rs::UnderwaterColorOptions> {
        let options = insta360_rs::UnderwaterColorOptions {
            mode: self.mode.into(),
            strength: self.strength,
            balance: self.balance,
            style: self.style,
        };
        options.validate()?;
        Ok(options)
    }
}
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyHousing>()?;
    module.add_class::<PyEnvironment>()?;
    module.add_class::<PyLensAccessory>()?;
    module.add_class::<PyMountingAccessory>()?;
    module.add_class::<PyUnderwaterColorMode>()?;
    module.add_class::<PyUnderwaterColorOptions>()?;
    module.add_class::<PyOpticalSelection>()?;
    module.add_class::<PyOpticalInspection>()?;
    module.add_class::<PyOpticalResolution>()?;
    Ok(())
}

#[pyclass(
    name = "OpticalSelection",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub(crate) struct PyOpticalSelection {
    #[pyo3(get)]
    housing: PyHousing,
    #[pyo3(get)]
    environment: PyEnvironment,
    #[pyo3(get)]
    lens_accessory: PyLensAccessory,
    #[pyo3(get)]
    mounting_accessory: PyMountingAccessory,
}
impl From<insta360_rs::OpticalSelection> for PyOpticalSelection {
    fn from(value: insta360_rs::OpticalSelection) -> Self {
        Self {
            housing: value.housing.into(),
            environment: value.environment.into(),
            lens_accessory: value.lens_accessory.into(),
            mounting_accessory: value.mounting_accessory.into(),
        }
    }
}
#[pyclass(
    name = "OpticalInspection",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub(crate) struct PyOpticalInspection {
    #[pyo3(get)]
    detected: Option<PyOpticalSelection>,
    #[pyo3(get)]
    evidence: Option<String>,
    #[pyo3(get)]
    encoded_lens_id: Option<u32>,
    #[pyo3(get)]
    ambiguity: Option<String>,
}
impl From<insta360_rs::optics::OpticalInspection> for PyOpticalInspection {
    fn from(value: insta360_rs::optics::OpticalInspection) -> Self {
        Self {
            detected: value.detected.map(Into::into),
            evidence: value.evidence.map(evidence_name),
            encoded_lens_id: value.encoded_lens_id,
            ambiguity: value.ambiguity,
        }
    }
}
#[pyclass(
    name = "OpticalResolution",
    module = "insta360_rs._native",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Debug)]
pub(crate) struct PyOpticalResolution {
    #[pyo3(get)]
    requested: PyOpticalSelection,
    #[pyo3(get)]
    detected: PyOpticalSelection,
    #[pyo3(get)]
    effective: PyOpticalSelection,
    #[pyo3(get)]
    evidence: String,
    #[pyo3(get)]
    source_lens_id: u32,
    #[pyo3(get)]
    target_lens_id: u32,
    #[pyo3(get)]
    sensor_crop_applied: bool,
}
impl From<insta360_rs::optics::OpticalResolution> for PyOpticalResolution {
    fn from(value: insta360_rs::optics::OpticalResolution) -> Self {
        Self {
            requested: value.requested.into(),
            detected: value.detected.into(),
            effective: value.effective.into(),
            evidence: evidence_name(value.evidence),
            source_lens_id: value.source_lens_id,
            target_lens_id: value.target_lens_id,
            sensor_crop_applied: value.sensor_crop_applied,
        }
    }
}

fn evidence_name(value: insta360_rs::optics::OpticalEvidence) -> String {
    match value {
        insta360_rs::optics::OpticalEvidence::RecordedState => "recorded_state",
        insta360_rs::optics::OpticalEvidence::EncodedLens => "encoded_lens",
    }
    .into()
}
