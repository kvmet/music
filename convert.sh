#!/usr/bin/env bash
# Convert WAV recordings to a compressed format via ffmpeg.
#
# Usage:
#   ./convert.sh                       # flac, all recording-*.wav in cwd
#   ./convert.sh flac
#   ./convert.sh opus
#   ./convert.sh m4a                   # AAC in mp4 container
#   ./convert.sh mp3
#   ./convert.sh flac path/to/file.wav # convert a single file
#
# Existing output files are skipped (no overwrite). The original .wav files
# are left in place; delete them yourself once you're happy with the converts.

set -euo pipefail

format="${1:-flac}"
pattern="${2:-recording-*.wav}"

case "$format" in
    flac) ext=flac ; codec=(-c:a flac -compression_level 8) ;;
    opus) ext=opus ; codec=(-c:a libopus -b:a 128k) ;;
    m4a)  ext=m4a  ; codec=(-c:a aac -b:a 192k) ;;
    mp3)  ext=mp3  ; codec=(-c:a libmp3lame -q:a 2) ;;
    *)
        echo "Unknown format: $format" >&2
        echo "Supported: flac, opus, m4a, mp3" >&2
        exit 2
        ;;
esac

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "ffmpeg not found in PATH. Install via 'brew install ffmpeg' or your package manager." >&2
    exit 3
fi

shopt -s nullglob

# If the pattern matches a single concrete file, convert just that one.
# Otherwise glob in the cwd.
if [[ -f "$pattern" ]]; then
    files=("$pattern")
else
    files=( $pattern )
fi

if (( ${#files[@]} == 0 )); then
    echo "No files matching: $pattern" >&2
    exit 0
fi

for f in "${files[@]}"; do
    out="${f%.wav}.$ext"
    if [[ -e "$out" ]]; then
        echo "skip (exists): $out"
        continue
    fi
    echo "$f -> $out"
    ffmpeg -loglevel error -i "$f" "${codec[@]}" "$out"
done
