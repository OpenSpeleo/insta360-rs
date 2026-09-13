# Stitching and underwater processing qualification

This records the September 13, 2026 qualification of the portable stitching
optimizations, X5 standard underwater housing correction, underwater color
processing and native preview changes. It supplements the reproducible commands
in [CI](CI.md), [testing](testing.md) and
[Python testing](../src-python/docs/testing.md).

## Scope and environment

Local execution used macOS ARM64 and a physical Metal adapter, Rust 1.97.1,
source-built FFmpeg 9.0 and the pinned MNN 3.6.1 CPU engine. The declared
minimum Rust version, 1.90, was checked separately without changing the primary
toolchain. Locked Linux and Windows GPU dependencies require that minimum; the
previous 1.88 declaration was inaccurate.

Native commands inherited the host application's existing SDK bootstrap and
environment and ran serially. GPU, underwater AI and AI stitching were required
through their `INSTA360_RS_REQUIRE_*` flags, so missing capabilities could not
silently turn those tests into successful skips. This is qualification of the
current MNN implementation; the separate Burn migration proposal has not been
implemented.

## Findings and regression contracts

The final seam mapping could fold even when the raw displacement field passed
its Jacobian check: latitude tapering and confidence gating changed the mapping
after validation. The correction bakes those operations into the displacement
vertices shared by CPU and WGSL, checks all corners of every bilinear cell,
repairs unsafe local neighborhoods and verifies the quantized result. A bounded
fallback attenuates an unresolved field toward identity. Tests cover latitude
boundaries, wrapped azimuth, confidence holes, invalid vectors, and preservation
of safe corrections away from a bad region. The existing Optical Flow quality
improvement threshold remains unchanged. See
[the optimization design](stitching-optimization.md).

The installed Python suite previously exposed stitching modes without executing
each optimized export. It now exports and independently decodes actual Dynamic,
Optical Flow and AI images, or checks the typed failure when AI is intentionally
absent. The strict wheel runner and clean-container smoke test require the
shipped AI stitching capability.

A new paired-color regression warms distinct lens histories and fails lens B
after lens A was already processed. The next valid pair must match a fresh
processor on both lenses. An uninterrupted reference proves the fixture can
detect retained history. This verifies the existing rollback implementation; it
did not require a production behavior change.

Resource verification covers all 59 original payloads, totaling 51,289,811
bytes, including source and stored digests, contiguous model reconstruction,
complete model groups and notices. The new video stitching model and its seven
CoreML files match their original Studio assets byte for byte. These checks
establish asset identity; [model qualification](ai-stitching-model.md) describes
the separate inference and algorithm boundaries.

## Completed automated checks

| Check                          | Result                                                                                                                                              |
| ------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| All-feature Rust targets       | 497 passed; six opt-in tests excluded from this combined invocation                                                                                 |
| Core-only Rust targets         | 267 passed; three opt-in tests excluded                                                                                                             |
| Rust doctests                  | Two all-feature and one core-only passed                                                                                                            |
| Isolated neural references     | Both passed in separate processes                                                                                                                   |
| Exhaustive integer LUT check   | Passed across the complete 24-bit input domain in release mode                                                                                      |
| Python binding Rust tests      | Six passed both without AI features and with both AI features                                                                                       |
| Installed Python distributions | 143 tests passed on each of Python 3.10–3.14 against both the initial wheel and the wheel rebuilt from its source archive                           |
| Build and release tooling      | 92 tests passed                                                                                                                                     |
| Feature boundaries             | Core, media, GPU, CLI, underwater AI, AI stitching and media+GPU compile checks passed; disabled AI capability, renderer and export failures passed |
| Minimum compiler               | Rust 1.90 all-target, all-feature check passed                                                                                                      |
| Release artifacts              | All-feature library, CLI and examples built successfully                                                                                            |
| Cargo distributions            | All seven archives passed size, extracted-source tests, doctests, isolated neural references and locked release builds                              |
| API documentation              | All-feature documentation built with warnings denied                                                                                                |
| Opt-in performance diagnostics | Integer-neighbor and LUT diagnostics passed; AquaVision diagnostics passed separately with one, two and four MNN threads                            |

The combined-suite exclusions comprise two isolated neural references, the
exhaustive LUT check and three performance diagnostics. They are deliberately
separate from ordinary parallel tests: neural thread-budget checks require a
fresh MNN process, exhaustive enumeration runs in release mode, and timing
diagnostics do not define portable performance thresholds.

## Supplied recording checks

The original `VID_20181001_225939_00_002.insv` corpus file was configured for
the full Rust and extracted-package runs. The separate tunnel recording,
`VID_20260823_123154_00_003.insv`, was checked at 230 and 232 seconds. It
contains 47,274,447,150 bytes; the concatenated first and last 1 MiB SHA-256 is
`c6a28f3f89a328d9cdbcf948482503eacedc53706a154264be92b73038b8ee0b`. This
partial-file identity is not a whole-file integrity digest.

The native selected-preview regression at 230 seconds matched repeated native
decoding. Both paths validated 16 pairs. Selection materialized five pairs,
including four speculative pairs, and made 16 hardware transfer attempts;
repeated decoding materialized all 16 pairs and made 32 transfer attempts. These
are observed counters for this seek, not a general throughput claim.

Release diagnostics rendered 1280×640 CPU and Metal output for Off, Dynamic,
Optical Flow and AI at both timestamps. All 16 renders succeeded on the
explicitly requested backend without fallback, and each repeated image was
byte-identical on that backend. The actual source timestamps were 230,013,117
and 232,015,117 microseconds. Every report resolved recorded standard underwater
housing from source lens 113 to target 117 with sensor-crop normalization.

Across the eight CPU/Metal image comparisons, mean absolute error was below
0.367 per 8-bit channel. Maximum differences were 24 at 230 seconds and 19 at
232 seconds; fewer than 0.317% of channel samples differed by more than two.
This records the measured interpolation difference rather than asserting
byte-identical backends. Synthetic parity tests retain their independent
regression tolerances.

The generated panoramas were visually inspected. These diagnostics used
stabilization Off and underwater restoration Off; they do not reproduce the
orientation or color settings of the supplied Studio image. Glare and polar
stretching remain visible. A matched Studio export is still needed to assess the
remaining visual difference objectively.

Reproduce these checks with `examples/stitch_diagnostics.rs`, setting
`INSTA360_STITCH_INPUT`, a fresh `INSTA360_STITCH_OUTPUT`,
`INSTA360_STITCH_TIME=230` or `232`, `INSTA360_STITCH_WIDTH=1280`,
`INSTA360_STITCH_MODES=off,dynamic,opticalFlow,ai` and
`INSTA360_STITCH_BACKENDS=cpu,gpu`. Inspect every result in `report.json` for
errors: the diagnostic preserves per-mode failures in the report. The run used
one repeated sample and no additional warmup, so its durations are smoke-check
observations rather than a benchmark.

## CI and release contract

Full `prek run --all-files --show-diff-on-failure` passed after reviewing the
Markdown and Python formatter edits. This included both hook configurations,
workflow lint, YAML checks, Rust formatting, all-feature workspace Clippy,
unused-dependency checks, Ruff and Bandit. Original vendor payloads remained
excluded from formatting.

The test workflow retains Linux, macOS ARM64, macOS x86_64 and Windows coverage,
the repaired-wheel tests and Linux compatibility checks for Python 3.10–3.13. It
now requires shipped AI stitching, checks the manifest-declared minimum Rust
compiler, runs the exhaustive LUT regression on Linux, and includes both
disabled-AI renderer and export failure cases.

Release verification checks the exact seven publishable crates against the
workspace, package verifier and dependency order. The Python extension remains
nonpublishable on crates.io. Release still requires the successful source CI run
and attempt for the tagged commit, builds fresh distributions and waits for all
wheel builds before either publication job. Native MNN setup, complete model
assets, license notices, package size limits and Cargo package-build
verification remain mandatory. See [release prerequisites](RELEASE.md),
including the initial publication and trusted-publisher setup for the new data
crate.

## Qualification limits

This local run does not execute the hosted Linux, Windows or Intel macOS jobs
and does not publish a release. Workflow validation is distinct from successful
execution on those runners.

Synthetic chart error, tensor references, asset hashes and CPU/GPU parity do not
establish Insta360 Studio pixel equivalence, underwater scene quality on every
camera, or temporal stability across a complete recording. Real-media checks
require their explicit out-of-tree sources; ordinary environment-gated test
success is not evidence that those sources ran.

Native preview decoding can use hardware acceleration, while stitched exports
still use software decoding and synchronous GPU output readback. MNN inference
remains CPU based. Existing performance figures predate the final seam safety
repair; they must not be presented as measurements of its latency. A repeated
still-frame diagnostic excludes decoding and presentation and is not playback
FPS. See [performance](performance.md) for the remaining pipeline boundaries.
