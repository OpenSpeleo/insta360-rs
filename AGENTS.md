# AGENTS.md

Guidance for AI/code agents working in the `OpenSpeleo/insta360-rs` repository.

This file is intentionally opinionated and feature-focused so agents can make
correct changes without rediscovering architecture every session. All paths and
commands below are relative to this independent repository's root.

## Project Overview

`insta360-rs` is a portable Rust library, CLI, and Python package for
inspecting, extracting, calibrating, stitching, stabilizing, and exporting
Insta360 INSV media. Geometry-stable 360-degree output for underwater
photogrammetry is the primary use case.

**Stack**: Rust, FFmpeg through `ffmpeg-next`, Rayon, optional `wgpu` compute,
PyO3, and Maturin. The independent Cargo workspace contains the main library,
five bundled-data crates, and the Python extension, with one root `Cargo.lock`.

Metadata recognition covers ONE, ONE R/RS panorama modules, ONE X through X6,
and X4 Air. Encoded stream extraction accepts one- and two-file inputs
independently of camera calibration. High-level decoded export accepts
registered V1/V2/V3/V6 calibrations and validated dual-track, two-file or
metadata-proven packed panoramas. V1 sensor-crop conversion remains unavailable;
real-recording qualification is narrower than synthetic camera/layout coverage.
Keep these support boundaries explicit.

## Core Principles

- **Simplicity First**: Use the smallest clear change that solves the problem.
- **Root Causes**: Diagnose failures and fix their cause; avoid temporary
  patches.
- **Minimal Impact**: Preserve unrelated behavior and existing user changes.
- **Readability and Maintainability**: Centralize shared logic and keep module
  responsibilities clear.
- **Performance Conscious**: Preserve bounded reads, allocations, queues, and
  frame lifetimes when processing large recordings.
- **Evidence Before Assumptions**: Derive calibration and motion behavior from
  recording metadata and validated profiles. Reject unsupported cases
  explicitly.
- **Meaningful Tests**: Test behavior, failure paths, and public contracts with
  independent expectations and generated fixtures.

## Task Management

1. Store all working plans in a unique OS temporary directory outside the
   repository, such as one created with `mktemp -d`. Plans must remain
   untracked: never stage or commit them or create plan directories inside the
   repository. This rule supersedes any inherited instructions about plan
   locations.
2. Share the intended approach before implementation and proceed within the
   user's authorized scope.
3. Mark completed work and revise the plan when findings change the approach.
4. Explain meaningful findings, behavior changes, and remaining uncertainties.
5. Report verification and material limitations in the final response.
6. Capture durable corrections in this guide or other relevant existing
   documentation, without creating unsolicited task or lesson files.
7. Update the relevant `docs/` or `src-python/docs/` files when contracts,
   architecture, or workflows change.

## Workflow Orchestration

### Planning

- Plan tasks involving multiple steps, architecture, or release changes before
  implementation. Use plan mode when available and appropriate.
- Re-plan when evidence invalidates an assumption.
- Include verification and documentation in the plan.

### Subagent Strategy

- Use subagents for bounded, independent research, exploration, and review when
  useful work can continue locally.
- Give each subagent one focused task and avoid overlapping edits.
- Review delegated findings against the repository before applying them.

### Self-Improvement

- Review relevant existing lessons before repeating a workflow.
- Turn corrections into specific guidance that prevents recurrence.

### Verification Before Done

- Match tests to the changed behavior and run the applicable checks below.
- Inspect the final diff, including formatter changes.
- Report skipped tests, unavailable prerequisites, and qualification limits
  accurately. Compilation alone does not establish real-camera correctness.

### Design Quality

- Prefer shared calibration, timing, and configuration boundaries over special
  cases inside individual renderers or bindings.
- Keep routine changes small; challenge unnecessary abstractions.

### Autonomous Bug Fixing

- Reproduce failures using logs, failing tests, or a focused fixture.
- Resolve ordinary implementation choices without repeated confirmation.
- Complete the fix, relevant tests, and documentation before reporting
  completion.

## Repository Map

- `Cargo.toml`, `Cargo.lock`: Workspace metadata, exact internal dependency
  pins, feature definitions, and shared dependency resolution.
- `rust-toolchain.toml`: Compiler authority for local commands, CI, and wheels.
- `src-rust/`: Core Rust implementation.
  - `container.rs`, `profile.rs`, `calibration.rs`: File parsing, camera and
    optical profiles, and per-recording calibration.
  - `motion/`, `motion.rs`, `telemetry.rs`, `timing.rs`: Motion decoding, sensor
    normalization, attitude, exposure mapping, and readout correction.
  - `stitch.rs`, `gpu.rs`, `stitch_gpu.wgsl`, `color.rs`: CPU/GPU projection,
    blending, and color transforms.
  - `stream.rs`, `extraction/`, `extraction.rs`: Direct packet/frame access and
    original stream/metadata extraction.
  - `sequence.rs`, `paired.rs`, `paired/preview.rs`: Chapter association, exact
    native frame pairs, and reusable preview decoding.
  - `media/`, `media.rs`: FFmpeg export pipeline, stabilization preparation,
    jobs, cancellation, and CLI implementation.
  - `assets/`, `assets.rs`: Verified bundled and application-supplied resources.
  - `bin/insta360-rs.rs`: Thin executable entry point.
- `data/core/`, `data/enhancement/`: Publishable data crates, original licensed
  assets, manifests, and vendor notices.
- `src-python/`: PyO3 bindings, Python wrapper, type stubs, tests, and package
  docs.
- `tests/`, `examples/`: Rust integration tests, small checked-in fixtures, and
  public API examples.
- `scripts/ci/`: FFmpeg/wheel builders, package/release checks, and their tests.
- `.github/`: Shared setup actions, CI, releases, and Dependabot configuration.
- `docs/`: Format, architecture, calibration, stabilization, testing, packaging,
  performance, and release documentation.

## Architecture Boundaries

- The default Rust feature set contains parsing, calibration, motion, assets,
  color, and CPU stitch primitives without FFmpeg or application frameworks.
- `media` adds FFmpeg stream access, extraction, decoding, and export. `cli`
  implies `media`. `gpu` is independent of `media` and adds safe `wgpu` compute.
- Keep FFmpeg ownership in the media, stream, and extraction layers. Core
  geometry and motion APIs operate on library-owned values.
- Camera profiles supply known lens/setup mappings and fallback geometry;
  per-unit intrinsics, distortion, principal points, and extrinsics come from
  the recording. Never replace them with generic calibration.
- Resolve optical setups and geometry once. CPU and GPU consume the same
  resolved calibration, source masks, and motion conventions.
- Preserve fixed projection and high-frequency seam geometry for photogrammetry.
  Color compensation must not change feature ownership or projected coordinates.
- Validate exposure/video clock mapping, IMU axes, units, gravity, and telemetry
  coverage before preparing stabilization. File renderers receive prepared
  poses. Chapter capture clocks must advance even when overlapping gyro values
  are identical.
- Compare native frame timestamps relative to their native stream origin before
  rounding for display or FFmpeg seeking. A rounded origin must not discard an
  exact first frame.
- Stream extraction preserves original encoded packets and metadata; it must not
  acquire the camera/calibration restrictions of the stitched exporter.
- Keep housing, environment, lens accessories and mounting separate. State 10/11
  is X5 Pro 119/120, not standard 117/118. Source housing masks are radial;
  recorded sensor crops normalize calibration coordinates separately.
- Keep Python wrappers focused on conversion and delegation. Update native
  bindings, wrapper exports, `__init__.pyi`, and API tests together.
- Production code must not load or execute vendor runtime libraries. Bundled
  data is distinct from qualified algorithm support.
- Preserve original vendor asset bytes and licensing. Keep manifests, hashes,
  compatibility rules, and complete model groups consistent. An embedded AI
  model does not establish an implemented or qualified inference path.

See [architecture](docs/architecture.md), [public API](docs/public-api.md),
[calibration](docs/calibration.md), and [stabilization](docs/stabilization.md).

## Testing Requirements

Use the compiler in `rust-toolchain.toml`. All-feature builds require the pinned
independent MNN CPU prefix in `MNN_ROOT`; see `scripts/ci/build-mnn.py` and
`docs/CI.md`. The optional `underwater-ai` feature enables that engine; Legacy
and Off have no native engine dependency. Run Cargo checks with `--locked`
except when intentionally updating versions or dependency resolution.

For core changes, start with the affected tests and the default-feature
boundary:

```sh
cargo test --locked --all-targets --no-default-features
cargo test --locked --doc --no-default-features
```

For media, GPU, CLI, or shared public API changes, run the relevant
configurations from CI's `default`, `media`, `gpu`, `cli`, and `all` feature
matrix. The combined configuration uses:

```sh
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
cargo build --locked --release --lib --bins --examples --all-features
```

Default workspace members exclude the Python extension to preserve feature
isolation. Test its Rust code separately:

```sh
cargo test --locked --manifest-path src-python/Cargo.toml --all-targets
```

Do not add `--all-features` to that Python Rust test command or replace these
commands with `cargo test --workspace --all-features`: `extension-module` is
wheel-only and removes the libpython linkage needed by Rust test executables.

Rebuild the native extension after Rust changes. In an activated Python virtual
environment with Maturin and the native prerequisites installed:

```sh
maturin develop --locked --manifest-path src-python/Cargo.toml
python -I -X faulthandler src-python/scripts/run-tests.py
```

For package and build-tool changes, use the applicable checks:

```sh
python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v
python3 scripts/ci/check-packages.py --all-features
python src-python/scripts/test.py
```

The final command requires Python's `build` package and verifies wheels and
source archives in fresh environments. See
[Python testing](src-python/docs/testing.md).

Media builds need FFmpeg headers and linkable libraries. Generated media tests
also need `ffmpeg`, `ffprobe`, and suitable fixture encoders. The strict Python
runner requires the installed native extension and linked software HEVC support.
Use `INSTA360_RS_REQUIRE_GPU=1` when GPU coverage is required; CI uses Mesa's
software Vulkan adapter. Set `INSTA360_RS_X5_SAMPLE` to the path of the real X5
recording for tests that require it.

Synthetic fixtures and software GPU tests establish specific contracts, not
real-camera or physical-GPU release qualification. Follow
[testing](docs/testing.md) and [CI](docs/CI.md) for prerequisites and the
complete matrix.

## Dependency Updates

- Consolidate requested Dependabot updates on an integration branch before
  reconciling overlapping manifest and lockfile changes.
- Use `prek autoupdate` for hooks and `cargo outdated` to inspect Rust updates.
  Review Python constraints in `src-python/pyproject.toml` and
  `scripts/ci/requirements-wheel.txt`.
- Keep the seven Rust packages on the shared lockfile. Update the compiler
  through `rust-toolchain.toml`; keep declared minimum Rust support accurate.
- Keep GitHub Actions pinned by commit and shared build-tool versions aligned.
  Preserve the CMake constraint required by the current x265 build until that
  compatibility issue is resolved.
- Regenerate affected dependency resolution and run the applicable full checks
  before committing dependency updates.
- Report unresolved constraints as `Can't update A until X Y Z is satisfied` in
  the dependency-update summary.

## Linter

Run relevant hooks while iterating and the complete hook suite before merge:

```sh
prek run --files path/to/changed-file
prek run --all-files --show-diff-on-failure
```

Root prek discovers both `.pre-commit-config.yaml` files. It runs common file
checks, Markdown formatting, actionlint, yamllint, Rust formatting, Clippy, and
cargo-machete; the Python configuration adds Ruff and Bandit. Hooks do not
replace the Python test suite. Original vendor payloads are excluded from text
formatting.

Direct Rust checks and formatting:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets --all-features -- -D warnings
cargo machete
cargo fmt --all
```

Markdown and whitespace hooks may rewrite files. Review their edits and rerun
until clean. For documentation-only work, changed-file hooks and
`git diff --check` are sufficient unless the documented behavior needs
validation.

## CI/CD

`.github/workflows/ci.yml` owns all test suites and runs on `master` pushes and
manual dispatch. Pull requests, tags, and other branch pushes do not trigger it;
dispatch CI explicitly when branch validation is needed. Linux runs the
test/lint matrix and Python 3.10-3.14 tests against a wheel built for testing.

After all checks pass, CI fetches tags and dispatches `release.yml` on the
stable `vMAJOR.MINOR.PATCH` tag matching both the workspace version and tested
commit. A tag pushed after CI finishes requires another CI run on that commit.
Release accepts only `workflow_dispatch`, verifies the successful source CI run
and attempt against the tag's commit, and builds fresh distributions. It never
reuses CI's package artifacts or reruns its test suites. Rust publication keeps
Cargo's package build verification and archive-size checks enabled.

Release builds Linux, macOS ARM64/x86_64, and Windows x86_64 wheels and a Linux
source distribution; both publishing jobs wait for every wheel build. macOS and
Windows jobs build and repair wheels without runtime test suites. The shared
source-built Linux FFmpeg SDK uses an exact native/image cache key, then falls
back to the verified CI run's SDK artifact, then compilation if unavailable.
Other platforms keep target-specific caches. Keep the existing Rust profiles and
Maturin build settings unchanged unless explicitly requested. The Python
extension keeps `publish = false` for crates.io.

`[workspace.package].version` is the package-version authority. All seven crates
inherit it and Maturin derives the Python version dynamically. The six exact
internal dependency pins must match. Use `cargo-edit` to update them together;
replace this example version with the intended release:

```sh
cargo set-version --workspace 0.1.1 --dry-run
cargo set-version --workspace 0.1.1
python3 scripts/ci/check-release.py v0.1.1
```

Review both `Cargo.toml` and `Cargo.lock`. Preserve `version.workspace = true`
in member packages and the `=` dependency requirements.

Trusted publishers use `OpenSpeleo/insta360-rs`, workflow `release.yml`, and
environments `crates-io` and `pypi`. Each Rust crate needs its own registration
after its initial manual upload. Follow [release instructions](docs/RELEASE.md)
for bootstrap, tag publication, attestations, and recovery.

Before publication, verify all six package archives and their extracted
contents. Each compressed crate must be below 10,000,000 bytes. Keep Cargo's
publication verification enabled and preserve byte-identical licensed assets in
crates, wheels, and source distributions.

## Documentation Expectations for Agents

Update the relevant documentation with feature intent, architecture boundaries,
public contracts, tests, performance implications, and reasons for design
choices.

- Keep README support tables aligned with implemented and qualified behavior.
- Keep Rust API docs, Python wrapper/stubs, examples, and Python docs
  consistent.
- Distinguish camera recognition, calibration parsing, encoded extraction,
  decoded export, compiled GPU support, and real-recording qualification.
- Document unavailable prerequisites and unsupported modes explicitly.
- Use measured, scoped performance claims with reproducible conditions.
- Keep operational instructions in `docs/CI.md` and `docs/RELEASE.md`
  synchronized with the actual workflows and scripts.

## Performance and Regression Checklist

Before finishing a relevant change, verify:

1. Probing stays bounded and avoids scanning the complete video payload.
2. Stream extraction preserves packet data, timestamps, metadata, and input
   ownership without introducing decoding or transcoding.
3. Calibration, source masks, motion, and color behavior agree across CPU/GPU.
4. GPU resources are reused and queues/frame retention stay bounded.
5. Stitch backend and HEVC encoder selection remain independent; export decoding
   is software and GPU output uses synchronous readback. Native random-access
   previews have a separate hardware-decoding policy with software fallback.
6. Automatic GPU fallback restarts file-export jobs on CPU only for typed GPU
   failures. A reusable preview renderer may retry its unpublished frame; batch
   callers use its strict method and own whole-attempt rollback. Explicit
   backends stay strict.
7. Cancellation and failure clean only job-owned temporary outputs. Successful
   publication never overwrites an existing destination and finalized video has
   its encoder flushed and MP4 trailer written.
8. Relevant tests and lint checks pass with the required prerequisites present.

## Practical Do/Do-Not

### Do

- Keep CLI and Python bindings thin and typed errors actionable.
- Use independent references for geometry, motion, packet, and color tests.
- Preserve byte-for-byte vendor resources and their notices.
- Check the actual compiled/runtime capabilities before choosing acceleration.
- Keep documentation and release claims scoped to demonstrated support.

### Do Not

- Guess physical calibration, clock offsets, sensor axes, or accessory profiles.
- Add camera-specific geometry switches separately to CPU and GPU renderers.
- Describe bundled AI payloads as available inference features.
- Treat an incomplete `.insta360-rs-part` file as a finished playable export.
- Enable the wheel-only PyO3 feature in Rust test executables.
- Bypass package verification, size limits, or asset integrity checks to
  release.

## Changelog Maintenance

Assess each product change for a meaningful user-visible feature, fix, or
performance improvement. Update `CHANGELOG.md` under `Unreleased` when it has
such an outcome; omit routine maintenance, dependency updates, CI, tests,
documentation, and internal implementation details.

The changelog is a curated public release history:

- Use high-level language and minimal technical detail.
- End each entry with the short commit ID representing the final implementation.
  Once that commit exists, record its ID before merge or release.
- Group entries under `Features`, `Performance`, or `Fixes` as applicable. The
  initial release records supported capabilities without Fixes or UI/UX
  sections.
- Replace superseded entries for the same evolving change with its final outcome
  and latest implementation commit.
- Name a dedicated changelog commit `[Changelog Update]`.
- When preparing a version tag, move applicable entries into
  `## vMAJOR.MINOR.PATCH - YYYY-MM-DD` and leave `Unreleased` at the top.
