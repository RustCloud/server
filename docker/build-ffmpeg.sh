#!/bin/sh
# Builds the ffmpeg and ffprobe that RustCloud's media engine runs for plugins:
# static (they open no files once started, see crates/sandbox), and only with
# what the engine uses. Everything else — network, file access, devices,
# hundreds of formats — is not compiled in, so it can't be attacked.
#
# LGPL only (no --enable-gpl). Runs on Alpine; installs to $PREFIX/bin.
# Usage: PREFIX=/opt/ffmpeg sh build-ffmpeg.sh
set -eu

FFMPEG_VERSION=8.1.3
DAV1D_VERSION=1.5.1
OPENH264_VERSION=2.6.0
PREFIX=${PREFIX:-/opt/ffmpeg}
JOBS=$(nproc)

apk add --no-cache build-base nasm pkgconf meson ninja curl xz libwebp-dev libwebp-static zlib-dev zlib-static >/dev/null

work=$(mktemp -d)
cd "$work"

# Downloads to a file, trying again on a bad network.
fetch() {
  curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 -o "$2" "$1"
}

# AV1 (and AVIF) decoding: dav1d, BSD, static.
fetch "https://code.videolan.org/videolan/dav1d/-/archive/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.gz" dav1d.tar.gz
tar xzf dav1d.tar.gz
meson setup "dav1d-$DAV1D_VERSION/build" "dav1d-$DAV1D_VERSION" --buildtype=release --default-library=static \
  --prefix="$PREFIX" --libdir=lib -Denable_tools=false -Denable_tests=false >/dev/null
ninja -C "dav1d-$DAV1D_VERSION/build" install >/dev/null

# H.264 encoding for videos a browser can't play (streams): OpenH264, BSD, static.
# Built from source: Cisco's patent licence covers only Cisco's own binaries.
fetch "https://github.com/cisco/openh264/archive/refs/tags/v$OPENH264_VERSION.tar.gz" openh264.tar.gz
tar xzf openh264.tar.gz
meson setup "openh264-$OPENH264_VERSION/build" "openh264-$OPENH264_VERSION" --buildtype=release \
  --default-library=static --prefix="$PREFIX" --libdir=lib -Dtests=disabled >/dev/null
ninja -C "openh264-$OPENH264_VERSION/build" install >/dev/null

fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" ffmpeg.tar.xz
tar xJf ffmpeg.tar.xz
cd "ffmpeg-$FFMPEG_VERSION"

# Containers of photos, videos and music.
demuxers=mov,matroska,avi,mpegts,flv,ogg,mp3,flac,wav,aac,gif,image_jpeg_pipe,image_png_pipe,image_webp_pipe,image_bmp_pipe,image_tiff_pipe
decoders=h264,hevc,vp8,vp9,libdav1d,mpeg4,mpeg2video,mjpeg,png,webp,gif,bmp,tiff,aac,mp3float,mp2float,ac3,eac3,dca,opus,vorbis,flac,alac,pcm_s16le,pcm_s24le,pcm_f32le
parsers=h264,hevc,vp8,vp9,av1,mpeg4video,mpegvideo,mjpeg,png,webp,gif,bmp,aac,mpegaudio,ac3,dca,opus,vorbis,flac
# Pictures out: thumbnails and previews. Streams out: H.264 and AAC in fragmented MP4.
# Raw RGB pixels and float samples out: inputs of neural networks (use.onnx).
encoders=mjpeg,png,libwebp,libopenh264,aac,rawvideo,pcm_f32le
muxers=image2pipe,webp,mp4,null,rawvideo,pcm_f32le
filters=scale,format,thumbnail,select,crop,pad,transpose,hflip,vflip,setsar,fps,tile,null,anull,aresample,aformat

PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" ./configure \
  --prefix="$PREFIX" \
  --pkg-config-flags=--static \
  --extra-cflags="-I$PREFIX/include" \
  --extra-ldflags="-L$PREFIX/lib -static" \
  --extra-libs="-lstdc++" \
  --enable-static --disable-shared \
  --disable-everything --disable-autodetect --disable-network \
  --disable-doc --disable-ffplay --disable-debug --disable-iconv \
  --enable-ffmpeg --enable-ffprobe \
  --enable-protocol=pipe,fd \
  --enable-zlib --enable-libdav1d --enable-libwebp --enable-libopenh264 \
  --enable-demuxer="$demuxers" \
  --enable-decoder="$decoders" \
  --enable-parser="$parsers" \
  --enable-encoder="$encoders" \
  --enable-muxer="$muxers" \
  --enable-filter="$filters" >/dev/null || { tail -30 ffbuild/config.log; exit 1; }
make -j"$JOBS" >/dev/null
make install >/dev/null
strip "$PREFIX/bin/ffmpeg" "$PREFIX/bin/ffprobe"

# Licence texts of what is linked in, for the image (see NOTICE).
licenses="$PREFIX/share/licenses"
mkdir -p "$licenses"
cp LICENSE.md "$licenses/ffmpeg-LICENSE.md"
cp COPYING.LGPLv2.1 "$licenses/ffmpeg-COPYING.LGPLv2.1"
cp "$work/dav1d-$DAV1D_VERSION/COPYING" "$licenses/dav1d-COPYING"
cp "$work/openh264-$OPENH264_VERSION/LICENSE" "$licenses/openh264-LICENSE"

cd /
rm -rf "$work"
"$PREFIX/bin/ffmpeg" -hide_banner -version | head -1
