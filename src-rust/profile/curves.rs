//! Vendor physical curves used for accessory conversion, not per-unit calibration.
use super::{android_binary_provenance, binary_provenance, ProfileProvenance};

/// Generic angle-in-degrees to physical-radius polynomial from the SDK.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalCurve {
    pub lens_id: u32,
    pub coefficients: [f64; 5],
    pub provenance: ProfileProvenance,
}

const CURVES: &[PhysicalCurve] = &[
    PhysicalCurve {
        lens_id: 131,
        coefficients: [
            0.0,
            0.023200178479423714,
            -2.6527512394617657e-5,
            1.2910990660481257e-6,
            -1.0421589669384779e-8,
        ],
        provenance: android_binary_provenance(
            "ins::Lens::getCoeff0x3fcd958; data0x15191a0/0x15191b0 and literal0xbe4661545ad02d9d",
        ),
    },
    PhysicalCurve {
        lens_id: 147,
        coefficients: [0.0, 0.02311, 6.271e-5, -5.268e-7, -1.293e-9],
        provenance: android_binary_provenance(
            "ins::Lens::getCoeff0x3fccc88; five doubles0x1b989a8",
        ),
    },
    PhysicalCurve {
        lens_id: 148,
        coefficients: [0.0, 0.0245, 5.707e-5, -9.534e-7, 2.095e-9],
        provenance: android_binary_provenance(
            "ins::Lens::getCoeff0x3fcdb3c; five doubles0x1b989f8",
        ),
    },
    PhysicalCurve {
        lens_id: 149,
        coefficients: [0.0, 0.0213, 0.000204, -3.485e-6, 1.458e-8],
        provenance: android_binary_provenance(
            "ins::Lens::getCoeff0x3fcccec; five doubles0x1b989d0",
        ),
    },
    PhysicalCurve {
        lens_id: 150,
        coefficients: [0.0, 0.02271, 0.00019, -3.754e-6, 1.706e-8],
        provenance: android_binary_provenance(
            "ins::Lens::getCoeff0x3fcdc20; five doubles0x1b98a20",
        ),
    },
    PhysicalCurve {
        lens_id: 13,
        coefficients: [-0.0028074, 0.024317, -5.045e-05, 1.1095e-06, -8.8568e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9ab8; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 17,
        coefficients: [-0.0028074, 0.024317, -5.045e-05, 1.1095e-06, -8.8568e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9ab8; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 19,
        coefficients: [0.0, 0.024134, -7.4557e-05, 2.0846e-06, -1.4576e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9b60; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 24,
        coefficients: [0.0, 0.0256, -7.0571e-05, 2.2326e-06, -1.7215e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9e34; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 27,
        coefficients: [0.0, 0.023693, -5.9584e-05, 1.8402e-06, -1.4809e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9e60; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 29,
        coefficients: [0.0, 0.024029, -6.86e-05, 2.0052e-06, -1.4266e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa4f8; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 34,
        coefficients: [0.0, 0.05247669, -2.57248e-05, 8.14169254e-07, -7.908613e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9b88; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 38,
        coefficients: [0.0, 0.024336, -7.55532e-05, 2.13171e-06, -1.5040305e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9eb4; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 40,
        coefficients: [0.0, 0.0259398, -7.3377467e-05, 2.2925e-06, -1.7776969e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa5d4; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 41,
        coefficients: [
            0.0,
            0.02413266,
            -7.451592e-05,
            2.0839511e-06,
            -1.4573211e-08,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa5fc; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 42,
        coefficients: [
            0.0,
            0.024446313655691,
            -7.9118706055e-05,
            2.24180908e-06,
            -1.5675326e-08,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9bb4; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 43,
        coefficients: [0.0, 0.02586, -8.1e-05, 2.41e-06, -1.819e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa624; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 44,
        coefficients: [0.0, 0.02434, -7.555e-05, 2.132e-06, -1.504e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa64c; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 52,
        coefficients: [0.0, 0.02351, -4.995e-05, 1.609e-06, -1.324e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa674; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 53,
        coefficients: [0.0, 0.02467, -8.474e-05, 2.384e-06, -1.664e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa6a0; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 54,
        coefficients: [0.0, 0.02279, -6.703e-05, 1.849e-06, -1.206e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9f30; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 64,
        coefficients: [0.0, 0.04272, -4.509e-05, 1.774e-06, -1.642e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa748; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 70,
        coefficients: [0.0, 0.02235, -5.382e-05, 2.001e-06, -1.392e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa770; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 71,
        coefficients: [0.0, 0.02249, -5.79e-05, 1.932e-06, -1.295e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa79c; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 76,
        coefficients: [0.0, 0.02302, -5.102e-05, 1.855e-06, -1.285e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9fac; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 77,
        coefficients: [0.0, 0.02296, -5.413e-05, 1.95e-06, -1.334e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9fd4; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 78,
        coefficients: [0.0, 0.02481, -8.254e-05, 2.739e-06, -1.972e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa7c8; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 79,
        coefficients: [0.0, 0.02252, -4.713e-05, 1.749e-06, -1.191e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa7f4; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 84,
        coefficients: [0.0, 0.02299, -5.257e-05, 1.903e-06, -1.31e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9ffc; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 86,
        coefficients: [0.0, 0.02056, 0.000135, -2.033e-06, 8.676e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa024; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 87,
        coefficients: [0.0, 0.01859, 0.0002531, -4.285e-06, 1.96e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa8a0; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 106,
        coefficients: [0.0, 0.02279, -4.871e-05, 1.795e-06, -1.235e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa8f8; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 107,
        coefficients: [0.0, 0.02264, -4.94e-05, 1.811e-06, -1.228e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa920; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 108,
        coefficients: [0.0, 0.0227, -4.905e-05, 1.803e-06, -1.232e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa0a4; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 113,
        coefficients: [0.0, 0.03159, -8.415e-05, 2.201e-06, -1.284e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa124; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 114,
        coefficients: [0.0, 0.0373774417, -0.0001059214, 3.5811e-06, -2.03e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa14c; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 115,
        coefficients: [0.0, 0.0321906505, -9.37245e-05, 2.5248e-06, -1.5e-08],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa174; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 117,
        coefficients: [0.0, 0.03072, 2.395e-05, 6.076e-08, -2.092e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa974; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 118,
        coefficients: [0.0, 0.02859, 2.395e-05, 6.076e-08, -2.092e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa18c; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 119,
        coefficients: [0.0, 0.03025, 5.341e-05, 1.623e-07, -6.208e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa19c; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 120,
        coefficients: [0.0, 0.0283, 0.0001661, -2.11e-06, 5.046e-09],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa1c8; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 140,
        coefficients: [
            0.0,
            0.02347874251877121,
            -2.7674053581301105e-05,
            1.3785975546613045e-06,
            -1.1205771662550173e-08,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa248; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 141,
        coefficients: [
            0.0,
            0.050058759932186774,
            -0.00010797875428738478,
            6.946467501059848e-06,
            -1.2074006897891668e-07,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa274; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 142,
        coefficients: [
            0.0,
            0.024508114438217286,
            -2.3584121988814713e-05,
            7.382934532711228e-07,
            -6.482923649818883e-09,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa2a0; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 144,
        coefficients: [
            0.0,
            0.12438555392403998,
            0.0002479751929076328,
            -3.7340206414873895e-06,
            3.2034028362148135e-07,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1f9c60; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 145,
        coefficients: [
            0.0,
            0.25824308167628196,
            -0.00015105065170424535,
            5.064253775010642e-05,
            -3.3091941273973424e-07,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1faa24; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 193,
        coefficients: [
            0.0,
            0.0454490911,
            -8.74282443e-05,
            3.79173971e-06,
            -2.77278772e-08,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fabc0; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 197,
        coefficients: [
            0.0,
            0.0462993571,
            -9.4612701e-05,
            4.15071135e-06,
            -3.05585771e-08,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fabec; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 198,
        coefficients: [
            0.0,
            0.043319707,
            0.000296024276,
            -3.03947899e-06,
            4.07985584e-09,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fac18; dispatch table0x5073430",
        ),
    },
    PhysicalCurve {
        lens_id: 199,
        coefficients: [
            0.0,
            0.037579787,
            0.000560197696,
            -7.55318454e-06,
            2.55466717e-08,
        ],
        provenance: binary_provenance(
            "ins::Lens::getCoeff arm64 0x1fa430; dispatch table0x5073430",
        ),
    },
];
/// Returns a physical curve only when its exact native branch was recovered.
pub fn physical_curve(lens_id: u32) -> Option<&'static PhysicalCurve> {
    CURVES.iter().find(|curve| curve.lens_id == lens_id)
}
/// Returns all recovered physical curves, including catalog-only lens IDs.
pub fn physical_curves() -> &'static [PhysicalCurve] {
    CURVES
}
