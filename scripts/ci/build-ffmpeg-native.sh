#!/usr/bin/env bash
# Build the wheel's shared runtime on macOS or Windows (MSVC through MSYS2).
set -euo pipefail

: "${INSTA360_FFMPEG_PREFIX:?set the native runtime installation prefix}"
: "${INSTA360_WHEEL_CACHE:?set the wheel cache directory}"
: "${INSTA360_WHEEL_JOBS:?set the number of compiler jobs}"
: "${INSTA360_CMAKE:?set the path to CMake 3}"

prefix=$INSTA360_FFMPEG_PREFIX
downloads=$INSTA360_WHEEL_CACHE/downloads
case "$(uname -s)" in
    Darwin) wheel_platform=macos ;;
    MSYS*|MINGW*)
        wheel_platform=windows
        prefix=$(cygpath -u "$prefix")
        downloads=$(cygpath -u "$downloads")
        INSTA360_CMAKE=$(cygpath -u "$INSTA360_CMAKE")
        ;;
    *) echo 'Use build-ffmpeg.sh for the Linux manylinux runtime' >&2; exit 1 ;;
esac
build_dir=$(dirname "$prefix")/build
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"

# shellcheck source=scripts/ci/ffmpeg-runtime-config.sh
source "$(dirname "${BASH_SOURCE[0]}")/ffmpeg-runtime-config.sh"
fetch_runtime_sources
if [[ $wheel_platform == windows ]]; then
    # Windows lacks the system zlib used on Linux/macOS. Build its shared DLL
    # from a verified source and include the source and notice in the wheel.
    zlib_version=1.3.2
    zlib_sha256=bb329a0a2cd0274d05519d61c667c062e06990d72e125ee2dfa8de64f0119d16
    zlib_archive="zlib-$zlib_version.tar.gz"
    zlib_url="https://zlib.net/fossils/$zlib_archive"
    fetch_source "$zlib_url" "$zlib_sha256" "$zlib_archive"
fi

if [[ -f "$prefix/share/insta360-rs/complete" ]]; then
    exit 0
fi

tar -xf "$downloads/$ffmpeg_archive" -C "$build_dir"
tar -xf "$downloads/$x265_archive" -C "$build_dir"
if [[ $wheel_platform == windows ]]; then
    tar -xf "$downloads/$zlib_archive" -C "$build_dir"
    zlib_options=(
        -G Ninja
        -DCMAKE_BUILD_TYPE=Release
        "-DCMAKE_INSTALL_PREFIX=$prefix"
        -DCMAKE_INSTALL_LIBDIR=lib
        -DCMAKE_C_COMPILER=cl
        -DZLIB_BUILD_TESTING=OFF
        -DZLIB_BUILD_SHARED=ON
        -DZLIB_BUILD_STATIC=OFF
    )
    "$INSTA360_CMAKE" -S "$build_dir/zlib-$zlib_version" -B "$build_dir/zlib-build" "${zlib_options[@]}"
    "$INSTA360_CMAKE" --build "$build_dir/zlib-build" --parallel "$INSTA360_WHEEL_JOBS"
    "$INSTA360_CMAKE" --install "$build_dir/zlib-build"
fi
x265_options+=(-G Ninja)
if [[ $wheel_platform == macos ]]; then
    x265_options+=(
        "-DCMAKE_OSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:?set deployment target}"
        "-DCMAKE_OSX_ARCHITECTURES=$(uname -m)"
        "-DCMAKE_INSTALL_NAME_DIR=$prefix/lib"
    )
else
    x265_options+=(-DCMAKE_C_COMPILER=cl -DCMAKE_CXX_COMPILER=cl -DSTATIC_LINK_CRT=OFF)
fi
"$INSTA360_CMAKE" -S "$build_dir/x265_$x265_version/source" -B "$build_dir/x265-build" "${x265_options[@]}"
"$INSTA360_CMAKE" --build "$build_dir/x265-build" --parallel "$INSTA360_WHEEL_JOBS"
"$INSTA360_CMAKE" --install "$build_dir/x265-build"

if [[ $wheel_platform == windows ]]; then
    # FFmpeg's MSVC flag filter maps -lz to zlib.lib and -lx265 to x265.lib.
    # The source builds install these import libraries under different names.
    cp "$prefix/lib/z.lib" "$prefix/lib/zlib.lib"
    cp "$prefix/lib/libx265.lib" "$prefix/lib/x265.lib"
    # MSVC uses Windows paths; retain the developer prompt's SDK directories.
    INCLUDE="$(cygpath -w "$prefix/include")${INCLUDE:+;$INCLUDE}"
    LIB="$(cygpath -w "$prefix/lib")${LIB:+;$LIB}"
    export INCLUDE LIB
    # Configure also executes probes linked to the freshly built DLLs.
    export PATH="$prefix/bin:$PATH"
fi

if [[ $wheel_platform == macos ]]; then
    ffmpeg_options+=(
        "--extra-cflags=-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
        "--extra-ldflags=-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
    )
else
    ffmpeg_options+=(--toolchain=msvc --arch=x86_64 --target-os=win32)
fi
cd "$build_dir/ffmpeg-$ffmpeg_version"
if ./configure "${ffmpeg_options[@]}"; then
    :
else
    configure_status=$?
    cat ffbuild/config.log >&2 || true
    exit "$configure_status"
fi
make -j "$INSTA360_WHEEL_JOBS"
make install

if [[ $wheel_platform == windows ]]; then
    # FFmpeg installs the DLLs but MSVC import libraries may remain in the build.
    for component in avcodec avformat avutil swscale swresample; do
        if [[ -f "lib$component/$component.lib" ]]; then
            cp "lib$component/$component.lib" "$prefix/lib/"
        fi
    done
fi

write_runtime_notices
if [[ $wheel_platform == windows ]]; then
    notices="$prefix/share/insta360-rs"
    cp "$build_dir/zlib-$zlib_version/LICENSE" "$notices/ZLIB-LICENSE.txt"
    {
        printf '\nzlib CMake arguments:\n'
        printf '%s\n' "${zlib_options[@]}"
    } >> "$notices/BUILD-CONFIGURATION.txt"
    {
        printf '\nWindows also bundles shared zlib %s.\n' "$zlib_version"
        printf 'zlib source: %s\nSHA-256: %s\n' "$zlib_url" "$zlib_sha256"
    } >> "$notices/SOURCES.txt"
fi
touch "$prefix/share/insta360-rs/complete"
