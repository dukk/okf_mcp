#!/usr/bin/env bash
# Builds the reader's self-hosted fonts in src/http/read/fonts/ (reader
# polish design, "Type"): Latin-subset woff2 of Inter (the interface) and
# Literata (concept bodies, roman and italic), both under the SIL Open Font
# License, with their license files beside them. Run it once, when a font is
# added or upgraded, and commit what it writes: the build only embeds them.
#
#   scripts/subset-reader-fonts.sh INTER_DIR LITERATA_DIR
#
# INTER_DIR     the unpacked Inter 4.1 release archive (Inter-4.1.zip from
#               the Inter project's releases), holding InterVariable.ttf
#               and LICENSE.txt.
# LITERATA_DIR  the unpacked Literata 3.103 release archive (from the
#               Literata project's releases), holding OFL.txt and
#               fonts/variable/Literata[opsz,wght].ttf and
#               fonts/variable/Literata-Italic[opsz,wght].ttf.
#
# It pins the axes the reader uses (Inter 400 to 700, Literata 400 to 600,
# both at their default optical size), keeps the Latin range only, refuses a
# file over 150 KB, and records each output's SHA-256 in
# scripts/reader-fonts.sha256 (check later with `sha256sum -c` from the
# repository root). uvx runs a pinned fonttools; the system Python is never
# used.
set -euo pipefail

readonly FONTTOOLS='fonttools[woff]==4.55.3'
# The "latin" range Google Fonts serves.
readonly LATIN='U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD'
readonly MAX_BYTES=153600
readonly OUT=src/http/read/fonts
readonly SUMS=scripts/reader-fonts.sha256

[ $# -eq 2 ] || { echo "usage: $0 INTER_DIR LITERATA_DIR" >&2; exit 2; }
command -v uvx >/dev/null || { echo "$0: needs uv (uvx)" >&2; exit 2; }
[ -f build.rs ] || { echo "$0: run it from the repository root" >&2; exit 2; }
inter_dir=$1
literata_dir=$2
inter=$inter_dir/InterVariable.ttf
literata=$literata_dir/fonts/variable/Literata[opsz,wght].ttf
literata_italic=$literata_dir/fonts/variable/Literata-Italic[opsz,wght].ttf
for file in "$inter" "$inter_dir/LICENSE.txt" "$literata" "$literata_italic" "$literata_dir/OFL.txt"; do
    [ -f "$file" ] || { echo "$0: missing $file" >&2; exit 2; }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$OUT"

# build INPUT OUTPUT AXIS...: pins INPUT's axes as given, keeps the Latin
# range, and writes OUTPUT as woff2.
build() {
    local input=$1 output=$2
    shift 2
    uvx --from "$FONTTOOLS" fonttools varLib.instancer "$input" "$@" --output "$work/pinned.ttf"
    uvx --from "$FONTTOOLS" pyftsubset "$work/pinned.ttf" --unicodes="$LATIN" \
        --flavor=woff2 --output-file="$output"
    local size
    size=$(stat -c %s "$output")
    if [ "$size" -gt "$MAX_BYTES" ]; then
        echo "$0: $output is $size bytes, over $MAX_BYTES" >&2
        exit 1
    fi
    echo "$output: $size bytes"
}

build "$inter" "$OUT/inter-latin.woff2" wght=400:700 opsz=drop
build "$literata" "$OUT/literata-latin.woff2" wght=400:600 opsz=drop
build "$literata_italic" "$OUT/literata-italic-latin.woff2" wght=400:600 opsz=drop
cp "$inter_dir/LICENSE.txt" "$OUT/OFL-Inter.txt"
cp "$literata_dir/OFL.txt" "$OUT/OFL-Literata.txt"
sha256sum "$OUT"/*.woff2 "$OUT"/OFL-*.txt >"$SUMS"
echo "inputs:"
sha256sum "$inter" "$literata" "$literata_italic"
