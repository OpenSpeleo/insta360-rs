#!/usr/bin/env bash
# Called inside the manylinux container by build-wheel.sh.
set -euo pipefail

: "${INSTA360_FFMPEG_PREFIX:?set the native runtime installation prefix}"
: "${INSTA360_WHEEL_JOBS:?set the number of compiler jobs}"
prefix=$INSTA360_FFMPEG_PREFIX
downloads=/wheel-cache/downloads
build_dir=$(dirname "$prefix")/build

# shellcheck source=scripts/ci/ffmpeg-runtime-config.sh
source "$(dirname "${BASH_SOURCE[0]}")/ffmpeg-runtime-config.sh"
fetch_runtime_sources

if [[ -f "$prefix/share/insta360-rs/complete" ]]; then
    printf 'Using cached FFmpeg SDK: %s\n' "$prefix"
    exit 0
fi

printf 'Building FFmpeg SDK: %s\n' "$prefix"
dnf install -y --setopt=install_weak_deps=False cmake nasm
tar -xf "$downloads/$ffmpeg_archive" -C "$build_dir"
tar -xf "$downloads/$x265_archive" -C "$build_dir"

# x265 4.1 uses policies removed by CMake 4; AlmaLinux's CMake 3 is supported.
/usr/bin/cmake -S "$build_dir/x265_$x265_version/source" -B "$build_dir/x265-build" "${x265_options[@]}"
/usr/bin/cmake --build "$build_dir/x265-build" --parallel "$INSTA360_WHEEL_JOBS"
/usr/bin/cmake --install "$build_dir/x265-build"

cd "$build_dir/ffmpeg-$ffmpeg_version"
./configure "${ffmpeg_options[@]}"
make -j "$INSTA360_WHEEL_JOBS"
make install

write_runtime_notices
touch "$prefix/share/insta360-rs/complete"
