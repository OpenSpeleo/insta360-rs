#!/usr/bin/env bash
# Shared source inputs, codec selection, and notices for every wheel platform.
# The caller sets prefix, downloads, and build_dir before sourcing this file.
: "${prefix:?caller must set prefix}"
: "${downloads:?caller must set downloads}"
: "${build_dir:?caller must set build_dir}"

ffmpeg_version=8.1.2
ffmpeg_sha256=464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
x265_version=4.1
x265_sha256=a31699c6a89806b74b0151e5e6a7df65de4b49050482fe5ebf8a4379d7af8f29
ffmpeg_archive="ffmpeg-$ffmpeg_version.tar.xz"
x265_archive="x265_$x265_version.tar.gz"
ffmpeg_url="https://ffmpeg.org/releases/$ffmpeg_archive"
x265_url="https://download.videolan.org/pub/videolan/x265/$x265_archive"

fetch_source() {
    local url=$1 sha256=$2 archive="$downloads/$3"
    if [[ ! -f "$archive" ]]; then
        curl --fail --location --retry 3 "$url" -o "$archive"
    fi
    if command -v sha256sum >/dev/null; then
        printf '%s  %s\n' "$sha256" "$archive" | sha256sum -c -
    else
        printf '%s  %s\n' "$sha256" "$archive" | shasum -a 256 -c -
    fi
}

fetch_runtime_sources() {
    mkdir -p "$downloads" "$build_dir"
    fetch_source "$ffmpeg_url" "$ffmpeg_sha256" "$ffmpeg_archive"
    fetch_source "$x265_url" "$x265_sha256" "$x265_archive"
}

# x265 dispatches optimized instructions at runtime. Avoid optional NUMA/VMAF
# dependencies and compiler options that target only the build host's CPU.
x265_options=(
    -DCMAKE_BUILD_TYPE=Release
    "-DCMAKE_INSTALL_PREFIX=$prefix"
    -DLIB_INSTALL_DIR=lib
    -DENABLE_SHARED=ON
    -DENABLE_CLI=OFF
    -DENABLE_PIC=ON
    -DENABLE_LIBNUMA=OFF
    -DENABLE_LIBVMAF=OFF
    -DNATIVE_BUILD=OFF
)

# Keep packet/container support broad and restrict codecs to supported media.
# CI's fixture-generator executable is separate from this shared runtime.
ffmpeg_options=(
    "--prefix=$prefix"
    "--libdir=$prefix/lib"
    --enable-shared
    --disable-static
    --enable-pic
    --disable-autodetect
    --enable-zlib
    --disable-doc
    --disable-debug
    --disable-programs
    --disable-network
    --disable-avdevice
    --disable-avfilter
    --disable-hwaccels
    --disable-encoders
    --disable-decoders
    "--enable-decoder=h264,hevc,mpeg4,mjpeg,png,rawvideo,aac,alac,pcm_s16le"
    --enable-gpl
    --enable-libx265
    --enable-encoder=libx265
)

configure_native_compiler_cache() {
    local platform=$1
    if [[ -z ${INSTA360_SCCACHE_ROOT:-} && ${INSTA360_SCCACHE_ENABLED:-0} != 1 ]]; then
        return
    fi

    local cache=${SCCACHE_PATH:-} cache_dir cache_program script_dir toolchain commands option
    if [[ -z $cache ]]; then
        cache=$(command -v sccache)
    fi
    if [[ $platform == windows ]]; then
        cache=$(cygpath -u "$cache")
    fi
    "$cache" --version >/dev/null
    cache_dir=$(cd "$(dirname "$cache")" && pwd)
    cache_program=$(basename "$cache")
    export PATH="$cache_dir:$PATH"

    # FFmpeg accepts a compiler launcher before the compiler command. Keep the
    # launcher on PATH so a tool-cache directory containing spaces is safe.
    local cc=gcc cxx=g++
    if [[ $platform == windows ]]; then
        cc=cl
        cxx=cl
    fi
    cc=${CC:-$cc}
    cxx=${CXX:-$cxx}
    mkdir -p "$build_dir"
    commands="$build_dir/sccache-compilers"
    python - "$cache_program" "$platform" "$cc" "$cxx" "${HOST_CC:-$cc}" "${OBJC:-$cc}" > "$commands" <<'PYTHON'
import shlex
import sys

cache, platform, *commands = sys.argv[1:]
for flag, command in zip(('cc', 'cxx', 'host-cc', 'objcc'), commands):
    arguments = shlex.split(command, posix=platform != 'windows')
    if not arguments:
        raise ValueError(f'Empty compiler command for {flag}')
    executable = arguments[0].strip('"\'').replace('\\', '/').rsplit('/', 1)[-1].lower()
    if executable not in ('sccache', 'sccache.exe'):
        command = cache + ' ' + command
    # Preserve caller quoting and arguments; parse only to detect an existing
    # launcher. NUL separators also preserve whitespace in the option value.
    sys.stdout.buffer.write(f'--{flag}={command}'.encode() + b'\0')
PYTHON
    while IFS= read -r -d '' option; do
        ffmpeg_options+=("$option")
    done < "$commands"

    # The shared launcher covers native C/C++ and assembly and embeds MSVC
    # debug information per object. Preserve explicitly selected toolchains.
    if [[ -z ${CMAKE_TOOLCHAIN_FILE:-} ]]; then
        script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
        toolchain="$script_dir/sccache.cmake"
        if [[ $platform == windows ]]; then
            toolchain=$(cygpath -w "$toolchain")
        fi
        export CMAKE_TOOLCHAIN_FILE=$toolchain
    fi
}

write_runtime_notices() {
    local notices="$prefix/share/insta360-rs"
    mkdir -p "$notices"
    cp "$build_dir/ffmpeg-$ffmpeg_version/COPYING.GPLv2" "$notices/FFMPEG-LICENSE.txt"
    cp "$build_dir/ffmpeg-$ffmpeg_version/LICENSE.md" "$notices/FFMPEG-LICENSING.txt"
    cp "$build_dir/x265_$x265_version/COPYING" "$notices/X265-LICENSE.txt"
    {
        printf 'FFmpeg configure arguments:\n'
        printf '%s\n' "${ffmpeg_options[@]}"
        printf '\nx265 CMake arguments:\n'
        printf '%s\n' "${x265_options[@]}"
    } > "$notices/BUILD-CONFIGURATION.txt"
    cat > "$notices/SOURCES.txt" <<EOF
This wheel bundles shared FFmpeg $ffmpeg_version and x265 $x265_version libraries.
FFmpeg is configured with --enable-gpl and --enable-libx265.
These libraries carry the accompanying GPL license texts. Project-authored
insta360-rs source retains its Apache-2.0 license; the Insta360 data resources
retain the vendor terms and provenance recorded in the project NOTICE.md.

Exact unmodified upstream source archives and the build scripts are included
under insta360_rs/_licenses/sources in the wheel and source distribution.

FFmpeg source: $ffmpeg_url
SHA-256: $ffmpeg_sha256
x265 source: $x265_url
SHA-256: $x265_sha256

See BUILD-CONFIGURATION.txt for the exact configuration and
BUILD-ENVIRONMENT.txt for the platform and compiler versions.
EOF
}
