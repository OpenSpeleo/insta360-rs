use insta360_rs::{RollingShutterCorrection, StitchConfig};

#[test]
fn projection_requires_an_exact_representable_two_to_one_ratio() {
    use insta360_rs::EquirectangularProjection;

    for (width, height) in [(0, 0), (1, 1), (u32::MAX, u32::MAX), (u32::MAX, 1 << 31)] {
        assert!(EquirectangularProjection { width, height }
            .validate()
            .is_err());
    }
    for (width, height) in [(2, 1), (3840, 1920), (u32::MAX - 1, u32::MAX / 2)] {
        assert!(EquirectangularProjection { width, height }
            .validate()
            .is_ok());
    }
}

#[test]
fn older_configs_default_readout_policy_and_new_policies_round_trip() {
    let mut encoded = serde_json::to_value(StitchConfig::default()).unwrap();
    encoded.as_object_mut().unwrap().remove("rolling_shutter");
    let restored: StitchConfig = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored.rolling_shutter, RollingShutterCorrection::Auto);
    for policy in [
        RollingShutterCorrection::Auto,
        RollingShutterCorrection::Off,
        RollingShutterCorrection::Required,
    ] {
        let config = StitchConfig {
            rolling_shutter: policy,
            ..StitchConfig::default()
        };
        let restored: StitchConfig =
            serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(restored, config);
    }
}
