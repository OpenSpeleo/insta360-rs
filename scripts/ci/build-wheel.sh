#!/usr/bin/env bash
# Build a repaired Linux wheel from its sdist, with a reproducible FFmpeg runtime.
set -euo pipefail

# One native identity for both the Actions SDK cache and the container prefix.
# Rust/Python dependency and wheel-packaging changes do not rebuild FFmpeg.
wheel_image=quay.io/pypa/manylinux_2_28_x86_64@sha256:53390351aeb4688114b02c36a23b3e6ce1166ee9b7afc5df1a4f776354fc764c
native_cache_key() {
    local scripts_dir=$1
    {
        cat "$scripts_dir/build-ffmpeg.sh" "$scripts_dir/ffmpeg-runtime-config.sh" || return 1
        printf '%s\n' "$wheel_image"
    } | {
        if command -v sha256sum >/dev/null; then
            sha256sum
        else
            shasum -a 256
        fi
    } | cut -d ' ' -f 1
}

if [[ "${1:-}" != "--container" && "${1:-}" != "--container-prewarm" ]]; then
    repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
    case "${1:-}" in
        "") container_mode=--container ;;
        --prewarm) container_mode=--container-prewarm ;;
        --cache-key) native_cache_key "$repo_dir/scripts/ci"; exit 0 ;;
        --image) printf '%s\n' "$wheel_image"; exit 0 ;;
        *) printf 'Usage: bash scripts/ci/build-wheel.sh [--prewarm|--cache-key|--image]\n' >&2; exit 2 ;;
    esac
    mkdir -p "$repo_dir/dist" "$repo_dir/.cache/wheel" "$repo_dir/target/wheel-build"
    exec docker run --rm --platform linux/amd64 \
        --mount "type=bind,source=$repo_dir,target=/source,readonly" \
        --mount "type=bind,source=$repo_dir/dist,target=/dist" \
        --mount "type=bind,source=$repo_dir/.cache/wheel,target=/wheel-cache" \
        --mount "type=bind,source=$repo_dir/target/wheel-build,target=/wheel-target" \
        --env "INSTA360_WHEEL_IMAGE=$wheel_image" \
        --env "INSTA360_WHEEL_UID=$(id -u)" \
        --env "INSTA360_WHEEL_GID=$(id -g)" \
        --env "INSTA360_WHEEL_JOBS=${INSTA360_WHEEL_JOBS:-2}" \
        "$wheel_image" bash -c \
        'cp /source/scripts/ci/build-wheel.sh /tmp/build-wheel.sh; exec bash /tmp/build-wheel.sh "$1"' \
        -- "$container_mode"
fi

[[ $(uname -m) == x86_64 ]]
trap 'chown -R "$INSTA360_WHEEL_UID:$INSTA360_WHEEL_GID" /dist /wheel-cache /wheel-target' EXIT

# Keep toolchains and Cargo artifacts separate from native host builds.
export CARGO_HOME=/wheel-cache/cargo
export RUSTUP_HOME=/wheel-cache/rustup
export CARGO_TARGET_DIR=/wheel-target
export CARGO_BUILD_JOBS=$INSTA360_WHEEL_JOBS
export PATH="$CARGO_HOME/bin:/opt/python/cp311-cp311/bin:$PATH"

# Snapshot inputs before long native builds. A local edit to the mounted source
# must not replace a running script or change the package halfway through.
mkdir -p /build/source
tar -C /source --exclude=.git --exclude=target --exclude=.cache \
    --exclude=dist --exclude=.venv --exclude=__pycache__ -cf - . |
    tar -C /build/source -xf -

native_key=$(native_cache_key /build/source/scripts/ci)
export INSTA360_FFMPEG_PREFIX="/wheel-cache/native/$native_key/prefix"
export PKG_CONFIG_PATH="$INSTA360_FFMPEG_PREFIX/lib/pkgconfig"
export LD_LIBRARY_PATH="$INSTA360_FFMPEG_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
prefix=$INSTA360_FFMPEG_PREFIX
downloads=/wheel-cache/downloads
build_dir=$(dirname "$prefix")/build
# shellcheck source=scripts/ci/ffmpeg-runtime-config.sh
source /build/source/scripts/ci/ffmpeg-runtime-config.sh
bash /build/source/scripts/ci/build-ffmpeg.sh
python - <<'PY'
import ctypes
import os

codec = ctypes.CDLL(os.environ["INSTA360_FFMPEG_PREFIX"] + "/lib/libavcodec.so")
codec.avcodec_find_encoder_by_name.restype = ctypes.c_void_p
assert codec.avcodec_find_encoder_by_name(b"libx265"), "HEVC software encoder missing"
PY

if [[ "${1:-}" == "--container-prewarm" ]]; then
    printf 'native/%s/prefix\n' "$native_key" > /wheel-cache/sdk-prefix.txt
    # Rewrite archive member names, preserving relative library symlink targets.
    tar --create --gzip --file /dist/ffmpeg-sdk.tar.gz \
        --directory /wheel-cache --transform 'flags=r;s,^,.cache/wheel/,' \
        "native/$native_key/prefix" downloads sdk-prefix.txt
    exit 0
fi

dnf install -y --setopt=install_weak_deps=False clang clang-libs cmake
toolchain=$(python - <<'PY'
from pathlib import Path
import tomllib

print(tomllib.loads(Path("/build/source/rust-toolchain.toml").read_text())["toolchain"]["channel"])
PY
)
export RUSTUP_TOOLCHAIN=$toolchain
if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    curl --fail --location --retry 3 https://sh.rustup.rs -o /tmp/rustup-init.sh
    bash /tmp/rustup-init.sh -y --profile minimal --default-toolchain none
fi
rustup toolchain install "$toolchain" --profile minimal --no-self-update

python -m pip install -r /build/source/scripts/ci/requirements-wheel.txt

notices=/build/source/src-python/python/insta360_rs/_licenses
mkdir -p "$notices/sources" /dist/sources
cp "$INSTA360_FFMPEG_PREFIX/share/insta360-rs/"*.txt "$notices/"
cp /build/source/scripts/ci/build-{wheel,ffmpeg}.sh "$notices/sources/"
cp /build/source/scripts/ci/ffmpeg-runtime-config.sh "$notices/sources/"
cp /build/source/scripts/ci/requirements-wheel.txt "$notices/sources/"
cp "$downloads/$ffmpeg_archive" "$downloads/$x265_archive" "$notices/sources/"
cp "$notices/sources/"* /dist/sources/
{
    printf 'Build image: %s\n' "$INSTA360_WHEEL_IMAGE"
    rustc --version
    cargo --version
    python -m maturin --version
    gcc --version
    /usr/bin/cmake --version
} > "$notices/BUILD-ENVIRONMENT.txt"

# These files exist only in the release staging directory. Register their
# licenses in wheel metadata without imposing generated files on local builds.
python /build/source/src-python/scripts/stage-project-licenses.py \
    /build/source/src-python --runtime

cd /build/source/src-python
# --sdist builds the wheel from the generated source archive, verifying that
# the path dependency and bundled data survived source distribution packaging.
python -m maturin build --release --locked --sdist --strip \
    --interpreter /opt/python/cp310-cp310/bin/python \
    --compatibility linux --out /build/unrepaired
auditwheel repair --plat manylinux_2_28_x86_64 \
    --wheel-dir /dist /build/unrepaired/*.whl
cp /build/unrepaired/*.tar.gz /dist/

python - "$ffmpeg_archive" "$x265_archive" <<'PY'
from pathlib import Path
import sys
import zipfile

wheels = list(Path("/dist").glob("*cp310-abi3-manylinux_2_28_x86_64.whl"))
assert len(wheels) == 1, f"expected one Linux abi3 wheel, got {wheels}"
assert "manylinux_2_28_x86_64" in wheels[0].name, wheels[0]
with zipfile.ZipFile(wheels[0]) as archive:
    names = archive.namelist()
    for library in ("libavcodec", "libavformat", "libavutil", "libswscale", "libx265"):
        assert any(".libs/" in name and library in name for name in names), library
    for filename in ("FFMPEG-LICENSE.txt", "X265-LICENSE.txt", "SOURCES.txt"):
        assert any(".dist-info/licenses/" in name and name.endswith(filename)
                   for name in names), filename
    for filename in (*sys.argv[1:], "ffmpeg-runtime-config.sh"):
        assert any("/_licenses/sources/" in name and name.endswith(filename)
                   for name in names), filename
print(f"Verified repaired wheel and bundled runtime notices: {wheels[0].name}")
PY
