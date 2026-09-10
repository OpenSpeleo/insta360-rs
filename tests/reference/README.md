# Underwater reference provenance

`underwater-mnn-reference-v1.bin` contains every output float for both models
and two distinct nonuniform input patterns. `mnn_reference.cpp` generates it
using the official pinned MNN 3.6.1 CPU Interpreter directly, without calling
the SDK's Rust model wrapper or C shim. It sets high precision and one thread.
Inputs vary spatially and across channels, unlike the earlier constant-input
reference. The test compares every element with an absolute/relative floating
point tolerance to allow CPU instruction differences.

The eight-byte header is `MNNREF1` followed by NUL. Little-endian float32
tensors follow in model order 197, 198 and pattern order 0, 1: 14,739 values for
each model-197 case, then 576 values for each model-198 case. The file is
122,528 bytes and its SHA-256 is
`1ccb6c62e98f4a4d3aa84b44715d274810e4d7f649f5219f95d95b08a3b7f16f`.

Regenerate on a Unix development host with the verified MNN prefix, a C++17
compiler and OpenSSL installed:

```sh
export MNN_ROOT=/path/to/verified/mnn-prefix
python3 tests/reference/generate_mnn_reference.py
```

The helper unwraps the original bundled model bytes into an OS temporary
directory, verifies their complete decoded SHA-256 identities, builds the
independent executable, and removes the decoded temporary models afterward.
Original licensed data is unchanged. The checked-in tensor reference was
generated on macOS ARM64. Other platforms run comparisons against that fixture;
they do not regenerate their own expectations.

`underwater-sequence-reference-v1.bin` is different: it is explicitly a
self-derived regression snapshot, not an independently implemented restoration
oracle. It contains every RGB8 pixel for frames 0, 9, 10, 59, 60 and 61 of a
changing 64×64 image sequence for each of the four AI styles. These positions
exercise model-update cadence, temporal smoothing and style-feature refresh. The
existing test also checks reset equivalence and stable buffer allocations.
Channel and mean-error tolerances permit minor platform rounding differences.

Its eight-byte header is `MNNRGB1` followed by NUL; tightly packed RGB8 frames
follow in style order 0–3, then the listed frame order. It is 294,920 bytes with
SHA-256 `bf812085dd067885ad2f35df111fcfd0c4155c4ad2f451b07b792c592410e01f`.
Regenerate only after reviewing the intended algorithm change:

```sh
cargo run --locked --features underwater-ai --example underwater_reference -- \
  tests/fixtures/underwater-sequence-reference-v1.bin
```

Neither reference establishes correspondence with a proprietary runtime,
physical-camera housing qualification, or the visual quality of underwater
restoration. They establish adapter execution and implementation regression
contracts. Physical image qualification remains separate.
