"""Native configuration, enum, capability and exception contracts."""

import unittest

import insta360_rs as api

ENUM_MEMBERS = {
    "Housing": "AUTO NONE VENTURE_CASE DIVE_CASE SPHERICAL_DIVE_CASE INVISIBLE_DIVE_CASE DIVE_CASE_PRO",
    "Environment": "AUTO AIR UNDERWATER",
    "LensAccessory": "AUTO NONE CLIP_ON_LENS_GUARD ADHESIVE_SPHERE_LENS_GUARD PROTECTOR_A PROTECTOR_S PROTECTOR_AS ND16 ND32 ND64 ND128",
    "MountingAccessory": "AUTO NONE DIVE_BUDDY",
    "UnderwaterColorMode": "OFF LEGACY AI",
    "Stabilization": "OFF FLOW_STATE DIRECTION_LOCK",
    "RollingShutterCorrection": "AUTO OFF REQUIRED",
    "ProcessingBackend": "AUTO CPU GPU",
    "ColorConversion": "AUTO PRESERVE I_LOG_TO_REC709",
    "EffectiveBackend": "CPU GPU UNKNOWN",
    "ImageFormat": "PNG JPEG",
    "AudioPolicy": "COPY DROP",
    "MediaAcceleration": "AUTO SOFTWARE HARDWARE",
    "ExportPhase": "PROBING DECODING STITCHING ENCODING FINALIZING UNKNOWN",
}


class ConfigTests(unittest.TestCase):
    def test_every_documented_enum_member_is_exposed_and_distinct(self):
        for name, members in ENUM_MEMBERS.items():
            enum = getattr(api, name)
            values = [getattr(enum, member) for member in members.split()]
            with self.subTest(enum=name):
                self.assertEqual(
                    [member for member in dir(enum) if member.isupper()],
                    sorted(members.split()),
                )
            for i, value in enumerate(values):
                with self.subTest(enum=name, member=members.split()[i]):
                    self.assertIsInstance(value, enum)
                    self.assertEqual(value, getattr(enum, members.split()[i]))
                    self.assertIn(name, repr(value))
                    self.assertTrue(all(value != other for other in values[i + 1 :]))

    def test_defaults_and_explicit_none_are_identical(self):
        for config in (
            api.StitchConfig(),
            api.StitchConfig(
                housing=None,
                environment=None,
                lens_accessory=None,
                mounting_accessory=None,
                underwater_color=None,
                stabilization=None,
                rolling_shutter=None,
                backend=None,
                color_conversion=None,
                width=None,
                height=None,
            ),
        ):
            self.assertEqual(config.housing, api.Housing.AUTO)
            self.assertEqual(config.environment, api.Environment.AUTO)
            self.assertEqual(config.lens_accessory, api.LensAccessory.AUTO)
            self.assertEqual(config.mounting_accessory, api.MountingAccessory.AUTO)
            self.assertEqual(config.underwater_color.mode, api.UnderwaterColorMode.OFF)
            self.assertEqual(config.stabilization, api.Stabilization.DIRECTION_LOCK)
            self.assertEqual(config.rolling_shutter, api.RollingShutterCorrection.AUTO)
            self.assertEqual(config.backend, api.ProcessingBackend.AUTO)
            self.assertEqual(config.color_conversion, api.ColorConversion.AUTO)
            self.assertIsNone(config.width)
            self.assertIsNone(config.height)
            self.assertIn("StitchConfig(", repr(config))

    def test_every_enum_configuration_value_round_trips(self):
        fields = {
            "housing": "Housing",
            "environment": "Environment",
            "lens_accessory": "LensAccessory",
            "mounting_accessory": "MountingAccessory",
            "stabilization": "Stabilization",
            "rolling_shutter": "RollingShutterCorrection",
            "backend": "ProcessingBackend",
            "color_conversion": "ColorConversion",
        }
        for field, enum_name in fields.items():
            for member in ENUM_MEMBERS[enum_name].split():
                value = getattr(getattr(api, enum_name), member)
                with self.subTest(field=field, member=member):
                    config = api.StitchConfig(**{field: value})
                    self.assertEqual(getattr(config, field), value)
                    mutable = api.StitchConfig()
                    setattr(mutable, field, value)
                    self.assertEqual(getattr(mutable, field), value)

    def test_dimensions_are_mutable_and_can_be_reset(self):
        config = api.StitchConfig(width=128, height=64)
        self.assertEqual((config.width, config.height), (128, 64))
        config.width, config.height = 64, 32
        self.assertEqual((config.width, config.height), (64, 32))
        config.width = config.height = None
        self.assertEqual((config.width, config.height), (None, None))

    def test_constructor_rejects_invalid_projection(self):
        for dimensions in (
            {"width": 128},
            {"height": 64},
            {"width": 0, "height": 0},
            {"width": 128, "height": 65},
            {"width": 64, "height": 64},
        ):
            with self.subTest(dimensions=dimensions):
                with self.assertRaises(api.InvalidMediaError):
                    api.StitchConfig(**dimensions)

    def test_unsigned_dimensions_reject_overflow_and_nonintegers(self):
        for value, error in (
            (-1, OverflowError),
            (2**32, OverflowError),
            (1.5, TypeError),
            ("64", TypeError),
        ):
            with self.subTest(value=value):
                with self.assertRaises(error):
                    api.StitchConfig(width=value, height=32)
                with self.assertRaises(error):
                    api.StitchConfig().width = value

    def test_enum_fields_reject_strings_integers_and_wrong_enum_types(self):
        for field in (
            "housing",
            "environment",
            "lens_accessory",
            "mounting_accessory",
            "stabilization",
            "rolling_shutter",
            "backend",
            "color_conversion",
        ):
            for value in ("AUTO", 0, api.AudioPolicy.COPY):
                with self.subTest(field=field, value=value):
                    with self.assertRaises(TypeError):
                        api.StitchConfig(**{field: value})
                    with self.assertRaises(TypeError):
                        setattr(api.StitchConfig(), field, value)

    def test_configuration_is_keyword_only(self):
        with self.assertRaises(TypeError):
            api.StitchConfig(api.Housing.NONE)
        with self.assertRaises(TypeError):
            api.StitchConfig.underwater_photogrammetry(api.Housing.NONE)

    def test_underwater_preset_defaults(self):
        config = api.StitchConfig.underwater_photogrammetry()
        self.assertEqual(config.housing, api.Housing.AUTO)
        self.assertEqual(config.environment, api.Environment.UNDERWATER)
        self.assertEqual(config.underwater_color.mode, api.UnderwaterColorMode.OFF)
        self.assertEqual(config.stabilization, api.Stabilization.DIRECTION_LOCK)
        self.assertEqual(config.rolling_shutter, api.RollingShutterCorrection.AUTO)
        self.assertEqual(config.backend, api.ProcessingBackend.AUTO)
        self.assertEqual(config.color_conversion, api.ColorConversion.AUTO)
        self.assertIsNone(config.width)
        self.assertIsNone(config.height)

    def test_underwater_preset_preserves_rolling_shutter_overrides(self):
        for name in ENUM_MEMBERS["RollingShutterCorrection"].split():
            correction = getattr(api.RollingShutterCorrection, name)
            with self.subTest(rolling_shutter=name):
                config = api.StitchConfig.underwater_photogrammetry(
                    rolling_shutter=correction,
                    backend=api.ProcessingBackend.CPU,
                    width=128,
                    height=64,
                )
                self.assertEqual(config.rolling_shutter, correction)
                self.assertEqual(config.stabilization, api.Stabilization.DIRECTION_LOCK)
                self.assertEqual(config.backend, api.ProcessingBackend.CPU)
                self.assertEqual((config.width, config.height), (128, 64))
        config = api.StitchConfig.underwater_photogrammetry(rolling_shutter=None)
        self.assertEqual(config.rolling_shutter, api.RollingShutterCorrection.AUTO)
        for value in ("AUTO", 0, api.Stabilization.OFF):
            with self.subTest(invalid_rolling_shutter=value):
                with self.assertRaises(TypeError):
                    api.StitchConfig.underwater_photogrammetry(rolling_shutter=value)

    def test_underwater_preset_preserves_every_housing_override(self):
        for name in ENUM_MEMBERS["Housing"].split():
            value = getattr(api.Housing, name)
            with self.subTest(housing=name):
                config = api.StitchConfig.underwater_photogrammetry(
                    housing=value,
                    backend=api.ProcessingBackend.CPU,
                    width=128,
                    height=64,
                )
                self.assertEqual(config.housing, value)
                self.assertEqual(config.environment, api.Environment.UNDERWATER)
                self.assertEqual(config.backend, api.ProcessingBackend.CPU)
                self.assertEqual((config.width, config.height), (128, 64))

    def test_removed_optical_setup_is_rejected(self):
        self.assertFalse(hasattr(api, "OpticalSetup"))
        with self.assertRaises(TypeError):
            api.StitchConfig(optical_setup="InvisibleDiveCaseUnderwater")

    def test_underwater_options_validate_and_remain_opt_in(self):
        for mode in (api.UnderwaterColorMode.LEGACY, api.UnderwaterColorMode.AI):
            for strength in (0.0, 0.3, 1.0):
                options = api.UnderwaterColorOptions(mode=mode, strength=strength)
                config = api.StitchConfig(underwater_color=options)
                self.assertAlmostEqual(config.underwater_color.strength, strength)
                self.assertEqual(config.underwater_color.mode, mode)
        for strength in (-0.1, 1.1, float("nan"), float("inf")):
            with self.subTest(strength=strength):
                with self.assertRaises(api.InvalidMediaError):
                    api.UnderwaterColorOptions(
                        mode=api.UnderwaterColorMode.LEGACY, strength=strength
                    )
        with self.assertRaises(api.InvalidMediaError):
            api.UnderwaterColorOptions(strength=0.5)
        with self.assertRaises(api.InvalidMediaError):
            api.UnderwaterColorOptions(mode=api.UnderwaterColorMode.LEGACY, style=0)
        with self.assertRaises(api.InvalidMediaError):
            api.UnderwaterColorOptions(mode=api.UnderwaterColorMode.AI, style=4)
        self.assertEqual(
            api.StitchConfig(housing=api.Housing.DIVE_CASE_PRO).underwater_color.mode,
            api.UnderwaterColorMode.OFF,
        )

    def test_underwater_options_are_immutable_and_configuration_accepts_replacements(
        self,
    ):
        original = api.UnderwaterColorOptions(
            mode=api.UnderwaterColorMode.LEGACY, strength=0.8, balance=0.5
        )
        config = api.StitchConfig(underwater_color=original)
        for options in (original, config.underwater_color):
            for name, value in (
                ("mode", api.UnderwaterColorMode.OFF),
                ("strength", 0.0),
                ("balance", 0.0),
                ("style", 0),
            ):
                with self.subTest(name=name):
                    with self.assertRaises(AttributeError):
                        setattr(options, name, value)
        config.underwater_color = api.UnderwaterColorOptions()
        self.assertEqual(config.underwater_color.mode, api.UnderwaterColorMode.OFF)
        self.assertEqual(original.mode, api.UnderwaterColorMode.LEGACY)
        self.assertAlmostEqual(original.strength, 0.8)
        with self.assertRaises(TypeError):
            config.underwater_color = {"mode": "OFF"}

    def test_underwater_preset_validates_projection(self):
        with self.assertRaises(api.InvalidMediaError):
            api.StitchConfig.underwater_photogrammetry(width=128)


class CapabilityTests(unittest.TestCase):
    def test_capability_fields_are_consistent_and_read_only(self):
        capabilities = api.capabilities()
        self.assertIsInstance(capabilities, api.Capabilities)
        for field in (
            "image_export",
            "video_export",
            "gpu_compiled",
            "gpu_available",
            "underwater_ai_compiled",
        ):
            self.assertIsInstance(getattr(capabilities, field), bool)
            with self.assertRaises(AttributeError):
                setattr(capabilities, field, False)
        self.assertTrue(
            capabilities.image_export, "native FFmpeg initialization failed"
        )
        self.assertEqual(capabilities.video_export, bool(capabilities.hevc_encoders))
        self.assertEqual(capabilities.gpu_available, bool(capabilities.gpu_adapters))
        if capabilities.gpu_available:
            self.assertTrue(capabilities.gpu_compiled)
            self.assertIsNone(capabilities.gpu_unavailable_reason)
        else:
            self.assertIsInstance(capabilities.gpu_unavailable_reason, str)
            self.assertTrue(capabilities.gpu_unavailable_reason)
        self.assertTrue(
            all(isinstance(name, str) for name in capabilities.hevc_encoders)
        )
        for adapter in capabilities.gpu_adapters:
            self.assertIsInstance(adapter, api.GpuAdapterInfo)
            for field in ("name", "backend", "device_type", "driver", "driver_info"):
                self.assertIsInstance(getattr(adapter, field), str)
            for field in ("vendor", "device"):
                self.assertIsInstance(getattr(adapter, field), int)
            with self.assertRaises(AttributeError):
                adapter.name = "changed"
        capabilities.hevc_encoders.clear()
        self.assertEqual(capabilities.video_export, bool(capabilities.hevc_encoders))

    def test_exception_inheritance_and_messages(self):
        for name in (
            "Insta360IOError",
            "InvalidMediaError",
            "UnsupportedCameraError",
            "MissingCalibrationError",
            "AmbiguousOpticalSetupError",
            "ConflictingOpticsError",
            "MissingCapabilityError",
            "GpuUnavailableError",
            "CancelledError",
            "MediaProcessingError",
            "GpuProcessingError",
        ):
            with self.subTest(exception=name):
                error = getattr(api, name)("native diagnostic")
                self.assertIsInstance(error, api.Insta360Error)
                self.assertIsInstance(error, Exception)
                self.assertEqual(str(error), "native diagnostic")
        self.assertTrue(issubclass(api.GpuUnavailableError, api.MissingCapabilityError))
        self.assertTrue(issubclass(api.GpuProcessingError, api.MediaProcessingError))


if __name__ == "__main__":
    unittest.main()
