#!/usr/bin/env bash
# Short generated clips, one per format a video wallpaper uses, for decoding tests of
# Lumenwall's FFmpeg build (ci.yml: made on Linux, decoded on Windows by lumenwall-video's
# video_png example with the build's DLLs). Made with the host's own ffmpeg (Ubuntu:
# apt-get install ffmpeg), whose encoders never reach a release: only these files do,
# and only into CI artifacts.
#
#   packaging/ffmpeg/test-clips.sh OUT_DIR
#
#   h264-aac.mp4       H.264 High 8-bit + AAC (the Workshop's usual .mp4)
#   hevc-aac.mp4       HEVC Main 8-bit + AAC
#   hevc10.mkv         HEVC Main 10 (P010 frames) in Matroska, no sound
#   vp9-opus.webm      VP9 profile 0 + Opus
#   vp8-vorbis.webm    VP8 + Vorbis
# AV1 is left out: the build has no software AV1 decoder (packaging/ffmpeg/README.md), and
# CI runners have no GPU to decode it.

set -euo pipefail
[[ $# -eq 1 ]] || { echo "usage: packaging/ffmpeg/test-clips.sh OUT_DIR" >&2; exit 1; }
out=$1
mkdir -p "$out"
ff() { ffmpeg -nostdin -hide_banner -loglevel error -y "$@"; }
video=(-f lavfi -i testsrc2=size=1280x720:rate=30 -t 2)
sound=(-f lavfi -i sine=frequency=440:sample_rate=48000 -t 2)

ff "${video[@]}" "${sound[@]}" -c:v libx264 -pix_fmt yuv420p -c:a aac -movflags +faststart "$out/h264-aac.mp4"
ff "${video[@]}" "${sound[@]}" -c:v libx265 -x265-params log-level=error -pix_fmt yuv420p -tag:v hvc1 \
    -c:a aac "$out/hevc-aac.mp4"
ff "${video[@]}" -c:v libx265 -x265-params log-level=error -pix_fmt yuv420p10le "$out/hevc10.mkv"
ff "${video[@]}" "${sound[@]}" -c:v libvpx-vp9 -b:v 1M -deadline realtime -cpu-used 8 -pix_fmt yuv420p \
    -c:a libopus "$out/vp9-opus.webm"
ff "${video[@]}" "${sound[@]}" -c:v libvpx -b:v 1M -deadline realtime -cpu-used 8 -c:a libvorbis \
    "$out/vp8-vorbis.webm"
ls -l "$out"
