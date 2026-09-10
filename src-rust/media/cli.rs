//! Command-line frontend for the high-level media API.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use crate::{
    probe, AudioPolicy, BackendReport, ColorConversion, EffectiveBackend, Environment,
    EquirectangularProjection, ExportResult, FrameSelection, Housing, ImageExportOptions, InputSet,
    LensAccessory, MediaAcceleration, MountingAccessory, ProcessingBackend,
    RollingShutterCorrection, Stabilization, StitchConfig, UnderwaterColorMode,
    UnderwaterColorOptions, VideoExportOptions,
};

use super::{ExportEvent, ExportJob, Exporter, MediaCapabilities};

#[derive(Debug, Parser)]
#[command(name = "insta360-rs", version, about)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Copy every encoded stream, audio track, and metadata record into a folder.
    Extract {
        /// One input discovers its sibling lens; two inputs specify a split pair.
        #[arg(required = true, num_args = 1..=2)]
        input: Vec<PathBuf>,
        /// An absent or empty destination directory.
        output_dir: PathBuf,
        /// Print the completed extraction report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inspect an INSV input without decoding the complete recording.
    Probe {
        #[arg(required = true)]
        input: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Export selected stitched images directly from INSV.
    ExportFrames {
        #[arg(required = true, num_args = 1..=2)]
        input: Vec<PathBuf>,
        output_dir: PathBuf,
        #[arg(long, value_delimiter = ',', conflicts_with = "timestamps")]
        indices: Vec<u64>,
        #[arg(long, value_delimiter = ',', conflicts_with = "indices")]
        timestamps: Vec<f64>,
        /// Equirectangular output width. Height is always width / 2.
        #[arg(long)]
        width: Option<u32>,
        #[command(flatten)]
        stitch: StitchArguments,
    },
    /// Export a stitched equirectangular HEVC MP4.
    ExportVideo {
        #[arg(required = true, num_args = 1..=2)]
        input: Vec<PathBuf>,
        output: PathBuf,
        /// Equirectangular output width. Height is always width / 2.
        #[arg(long)]
        width: Option<u32>,
        /// HEVC quality from 1 (lowest) to 100 (highest).
        #[arg(long, default_value_t = 90)]
        quality: u8,
        /// Source-relative interval start in seconds. Defaults to zero.
        #[arg(long)]
        start: Option<f64>,
        /// Interval length in seconds. Defaults to the rest of the recording.
        #[arg(long)]
        duration: Option<f64>,
        /// Audio handling. Copy is reserved until synchronized remuxing is implemented.
        #[arg(long, value_enum, default_value_t = CliAudioPolicy::Drop)]
        audio: CliAudioPolicy,
        /// Hardware codec policy, independent of the stitch backend.
        #[arg(long, value_enum, default_value_t = CliMediaAcceleration::Auto)]
        media_acceleration: CliMediaAcceleration,
        #[command(flatten)]
        stitch: StitchArguments,
    },
    /// Print the media and acceleration capabilities of this build.
    Capabilities {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, clap::Args)]
struct StitchArguments {
    #[arg(long, value_enum, default_value_t = Housing::Auto)]
    housing: Housing,
    #[arg(long, value_enum, default_value_t = Environment::Auto)]
    environment: Environment,
    #[arg(long, value_enum, default_value_t = LensAccessory::Auto)]
    lens_accessory: LensAccessory,
    #[arg(long, value_enum, default_value_t = MountingAccessory::Auto)]
    mounting_accessory: MountingAccessory,
    #[arg(long, value_enum, default_value_t = UnderwaterColorMode::Off)]
    underwater_color: UnderwaterColorMode,
    #[arg(long)]
    underwater_strength: Option<f32>,
    #[arg(long)]
    underwater_balance: Option<f32>,
    #[arg(long)]
    underwater_style: Option<u32>,
    #[arg(long, value_enum, default_value_t = CliStabilization::DirectionLock)]
    stabilization: CliStabilization,
    /// Correct motion during sensor readout (requires stabilization).
    #[arg(long, value_enum, default_value_t = CliRollingShutter::Auto)]
    rolling_shutter: CliRollingShutter,
    #[arg(long, value_enum, default_value_t = CliBackend::Auto)]
    backend: CliBackend,
    /// Convert identified I-Log to Rec.709 automatically, preserve it, or request conversion explicitly.
    #[arg(long, value_enum, default_value_t = CliColorConversion::Auto)]
    color_conversion: CliColorConversion,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliStabilization {
    Off,
    FlowState,
    #[default]
    DirectionLock,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliRollingShutter {
    #[default]
    Auto,
    Off,
    Required,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliBackend {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliColorConversion {
    #[default]
    Auto,
    Preserve,
    #[value(name = "i-log-to-rec709")]
    ILogToRec709,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliAudioPolicy {
    Copy,
    #[default]
    Drop,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliMediaAcceleration {
    #[default]
    Auto,
    Software,
    Hardware,
}

impl StitchArguments {
    fn config(&self) -> StitchConfig {
        StitchConfig {
            housing: self.housing,
            environment: self.environment,
            lens_accessory: self.lens_accessory,
            mounting_accessory: self.mounting_accessory,
            underwater_color: UnderwaterColorOptions {
                mode: self.underwater_color,
                strength: self.underwater_strength,
                balance: self.underwater_balance,
                style: self.underwater_style,
            },
            stabilization: match self.stabilization {
                CliStabilization::Off => Stabilization::Off,
                CliStabilization::FlowState => Stabilization::FlowState,
                CliStabilization::DirectionLock => Stabilization::DirectionLock,
            },
            rolling_shutter: match self.rolling_shutter {
                CliRollingShutter::Auto => RollingShutterCorrection::Auto,
                CliRollingShutter::Off => RollingShutterCorrection::Off,
                CliRollingShutter::Required => RollingShutterCorrection::Required,
            },
            backend: match self.backend {
                CliBackend::Auto => ProcessingBackend::Auto,
                CliBackend::Cpu => ProcessingBackend::Cpu,
                CliBackend::Gpu => ProcessingBackend::Gpu,
            },
            color_conversion: match self.color_conversion {
                CliColorConversion::Auto => ColorConversion::Auto,
                CliColorConversion::Preserve => ColorConversion::Preserve,
                CliColorConversion::ILogToRec709 => ColorConversion::ILogToRec709,
            },
            ..StitchConfig::default()
        }
    }
}

pub fn run() -> crate::Result<()> {
    match Arguments::parse().command {
        Command::Extract {
            input,
            output_dir,
            json,
        } => {
            let inputs = match input.as_slice() {
                [path] => InputSet::discover(path)?,
                _ => InputSet::new(input)?,
            };
            let report = crate::extract(&inputs, output_dir)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|error| crate::Error::Media(error.to_string()))?
                );
            } else {
                println!(
                    "extracted {} streams and {} records from {} inputs",
                    report.stream_count, report.record_count, report.input_count
                );
                println!("output: {}", report.output_dir.display());
                println!("manifest: {}", report.manifest_path.display());
                for warning in &report.warnings {
                    eprintln!("warning: {warning}");
                }
            }
        }
        Command::Probe { input, json } => {
            let info = probe(&InputSet::new(input)?)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&info)
                        .map_err(|error| crate::Error::Media(error.to_string()))?
                );
            } else {
                println!(
                    "camera: {}",
                    info.camera_name.as_deref().unwrap_or("unknown")
                );
                println!("inputs: {}", info.inputs.len());
                println!("video tracks: {}", info.video_tracks.len());
                println!("offset versions: {:?}", info.offset_versions);
                println!("optical profiles: {:?}", info.optical_profiles);
            }
        }
        Command::ExportFrames {
            input,
            output_dir,
            indices,
            timestamps,
            width,
            stitch,
        } => {
            let selection = if !indices.is_empty() {
                FrameSelection::Indices(indices)
            } else if !timestamps.is_empty() {
                FrameSelection::Timestamps(
                    timestamps
                        .into_iter()
                        .map(seconds_to_duration)
                        .collect::<crate::Result<_>>()?,
                )
            } else {
                return Err(crate::Error::InvalidMedia(
                    "provide --indices or --timestamps".into(),
                ));
            };
            let exporter = Exporter::new(InputSet::new(input)?, stitch.config())?;
            let job = exporter.export_frames(
                output_dir,
                selection,
                ImageExportOptions {
                    scale_width: width,
                    ..ImageExportOptions::default()
                },
            );
            let result = wait_for_export(job)?;
            println!("wrote {} frames", result.frames_written);
            print_backend_report(&result.backend);
            print_performance(&result);
        }
        Command::ExportVideo {
            input,
            output,
            width,
            quality,
            start,
            duration,
            audio,
            media_acceleration,
            stitch,
        } => {
            let exporter = Exporter::new(InputSet::new(input)?, stitch.config())?;
            let options = VideoExportOptions {
                quality,
                audio: match audio {
                    CliAudioPolicy::Copy => AudioPolicy::Copy,
                    CliAudioPolicy::Drop => AudioPolicy::Drop,
                },
                acceleration: match media_acceleration {
                    CliMediaAcceleration::Auto => MediaAcceleration::Auto,
                    CliMediaAcceleration::Software => MediaAcceleration::Software,
                    CliMediaAcceleration::Hardware => MediaAcceleration::Hardware,
                },
                projection: width.map(|width| EquirectangularProjection {
                    width,
                    height: width / 2,
                }),
                start: start.map(seconds_to_duration).transpose()?,
                duration: duration.map(seconds_to_duration).transpose()?,
            };
            let result = wait_for_export(exporter.export_video(output, options))?;
            println!("wrote {}", result.outputs[0].display());
            print_backend_report(&result.backend);
            print_performance(&result);
        }
        Command::Capabilities { json } => {
            let capabilities = MediaCapabilities::detect();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&capabilities)
                        .map_err(|error| crate::Error::Media(error.to_string()))?
                );
            } else {
                println!("image export: {}", capabilities.image_export);
                println!("video export: {}", capabilities.video_export);
                println!("GPU compiled: {}", capabilities.gpu_compiled);
                println!("GPU available: {}", capabilities.gpu_available);
                if let Some(reason) = &capabilities.gpu_unavailable_reason {
                    println!("GPU unavailable reason: {reason}");
                }
                for adapter in &capabilities.gpu_adapters {
                    println!(
                        "GPU adapter: {} ({}, {}, {:04x}:{:04x})",
                        adapter.name,
                        adapter.backend,
                        adapter.device_type,
                        adapter.vendor,
                        adapter.device
                    );
                }
                println!("HEVC encoders: {:?}", capabilities.hevc_encoders);
            }
        }
    }
    Ok(())
}

fn wait_for_export(job: ExportJob) -> crate::Result<ExportResult> {
    let report = |event| match event {
        ExportEvent::StabilizationPrepared(description) => eprintln!("{description}"),
        ExportEvent::Warning(warning) => eprintln!("warning: {warning}"),
        _ => {}
    };
    while !job.is_finished() {
        if let Some(event) = job.recv_event_timeout(Duration::from_millis(100)) {
            report(event);
        }
    }
    while let Some(event) = job.try_event() {
        report(event);
    }
    job.wait()
}

fn print_backend_report(report: &BackendReport) {
    let selected = match report.selected {
        EffectiveBackend::Cpu => "cpu",
        EffectiveBackend::Gpu => "gpu",
    };
    println!("stitch backend: {selected}");
    if let Some(adapter) = &report.adapter {
        println!("GPU adapter: {} ({})", adapter.name, adapter.backend);
    }
    if let Some(fallback) = &report.fallback {
        println!("GPU fallback: {fallback}");
    }
}

fn print_performance(result: &ExportResult) {
    let elapsed = result.elapsed.as_secs_f64();
    println!("elapsed: {elapsed:.3}s");
    if elapsed > 0.0 {
        println!(
            "throughput: {:.2} frames/s",
            result.frames_written as f64 / elapsed
        );
    }
}

fn seconds_to_duration(seconds: f64) -> crate::Result<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(crate::Error::InvalidMedia(
            "timestamps must be finite non-negative seconds".into(),
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extract_single_and_split_inputs_without_stitch_options() {
        for paths in [vec!["primary.insv"], vec!["primary.insv", "companion.insv"]] {
            let mut args = vec!["insta360-rs", "extract"];
            args.extend(paths.iter().copied());
            args.extend(["destination", "--json"]);
            let arguments = Arguments::try_parse_from(args).expect("valid extraction command");
            let Command::Extract {
                input,
                output_dir,
                json,
            } = arguments.command
            else {
                panic!("expected extract command");
            };
            assert_eq!(input, paths.iter().map(PathBuf::from).collect::<Vec<_>>());
            assert_eq!(output_dir, PathBuf::from("destination"));
            assert!(json);
        }
        assert!(Arguments::try_parse_from(["insta360-rs", "extract", "input.insv"]).is_err());
        assert!(Arguments::try_parse_from([
            "insta360-rs",
            "extract",
            "input.insv",
            "output",
            "--quality",
            "80"
        ])
        .is_err());
    }

    #[test]
    fn rejects_negative_and_non_finite_timestamps() {
        assert!(seconds_to_duration(-1.0).is_err());
        assert!(seconds_to_duration(f64::NAN).is_err());
        assert!(seconds_to_duration(f64::INFINITY).is_err());
    }

    #[test]
    fn parses_seconds_without_losing_milliseconds() {
        assert_eq!(
            seconds_to_duration(2.5).expect("valid timestamp"),
            Duration::from_millis(2_500)
        );
    }

    #[test]
    fn parses_video_export_controls() {
        let arguments = Arguments::try_parse_from([
            "insta360-rs",
            "export-video",
            "source.insv",
            "stitched.mp4",
            "--width",
            "3840",
            "--quality",
            "85",
            "--start",
            "12.5",
            "--duration",
            "60",
            "--audio",
            "drop",
            "--media-acceleration",
            "hardware",
            "--stabilization",
            "off",
            "--rolling-shutter",
            "off",
            "--backend",
            "cpu",
        ])
        .expect("valid video export arguments");

        let Command::ExportVideo {
            input,
            output,
            width,
            quality,
            start,
            duration,
            audio,
            media_acceleration,
            stitch,
        } = arguments.command
        else {
            panic!("expected export-video command");
        };
        assert_eq!(input, [PathBuf::from("source.insv")]);
        assert_eq!(output, PathBuf::from("stitched.mp4"));
        assert_eq!(width, Some(3_840));
        assert_eq!(quality, 85);
        assert_eq!(start, Some(12.5));
        assert_eq!(duration, Some(60.0));
        assert!(matches!(audio, CliAudioPolicy::Drop));
        assert!(matches!(media_acceleration, CliMediaAcceleration::Hardware));
        assert!(matches!(stitch.stabilization, CliStabilization::Off));
        assert_eq!(
            stitch.config().rolling_shutter,
            RollingShutterCorrection::Off
        );
        assert!(matches!(stitch.backend, CliBackend::Cpu));
        assert_eq!(stitch.config().color_conversion, ColorConversion::Auto);
    }

    #[test]
    fn both_export_commands_map_color_conversion_choices() {
        for command in ["export-frames", "export-video"] {
            for (value, expected) in [
                ("auto", ColorConversion::Auto),
                ("preserve", ColorConversion::Preserve),
                ("i-log-to-rec709", ColorConversion::ILogToRec709),
            ] {
                let arguments = Arguments::try_parse_from([
                    "insta360-rs",
                    command,
                    "source.insv",
                    "output",
                    "--color-conversion",
                    value,
                ])
                .expect("valid color conversion");
                let stitch = match arguments.command {
                    Command::ExportFrames { stitch, .. } | Command::ExportVideo { stitch, .. } => {
                        stitch
                    }
                    _ => panic!("expected export command"),
                };
                assert_eq!(stitch.config().color_conversion, expected);
            }
        }
    }

    #[test]
    fn optical_and_restoration_controls_reach_both_export_configs() {
        for command in ["export-frames", "export-video"] {
            let arguments = Arguments::try_parse_from([
                "insta360-rs",
                command,
                "source.insv",
                "output",
                "--housing",
                "dive-case-pro",
                "--environment",
                "underwater",
                "--lens-accessory",
                "none",
                "--mounting-accessory",
                "dive-buddy",
                "--underwater-color",
                "legacy",
                "--underwater-strength",
                "0.4",
                "--underwater-balance",
                "0.7",
            ])
            .unwrap();
            let stitch = match arguments.command {
                Command::ExportFrames { stitch, .. } | Command::ExportVideo { stitch, .. } => {
                    stitch
                }
                _ => unreachable!(),
            };
            let config = stitch.config();
            assert_eq!(config.housing, Housing::DiveCasePro);
            assert_eq!(config.environment, Environment::Underwater);
            assert_eq!(config.lens_accessory, LensAccessory::None);
            assert_eq!(config.mounting_accessory, MountingAccessory::DiveBuddy);
            assert_eq!(config.underwater_color.mode, UnderwaterColorMode::Legacy);
            assert_eq!(config.underwater_color.strength, Some(0.4));
            assert_eq!(config.underwater_color.balance, Some(0.7));
            assert_eq!(config.underwater_color.style, None);
            config.underwater_color.validate().unwrap();
        }
        for (flag, value) in [
            ("--optical-setup", "strict-auto"),
            ("--housing", "unknown"),
            ("--environment", "sea"),
            ("--underwater-color", "colorplus"),
        ] {
            assert!(Arguments::try_parse_from([
                "insta360-rs",
                "export-video",
                "source.insv",
                "output.mp4",
                flag,
                value
            ])
            .is_err());
        }
    }

    #[test]
    fn rejects_unknown_color_conversion() {
        assert!(Arguments::try_parse_from([
            "insta360-rs",
            "export-video",
            "source.insv",
            "output.mp4",
            "--color-conversion",
            "colorplus",
        ])
        .is_err());
    }
}
