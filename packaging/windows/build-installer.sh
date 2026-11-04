#!/bin/sh
# Builds the Windows installer of a release from its zip, with NSIS (makensis 3.x),
# on Linux (sudo apt install nsis) or Windows. Used by .github/workflows/release.yml.
#
#   packaging/windows/build-installer.sh 0.2.0 dist/lumenwall-0.2.0-x86_64-pc-windows-msvc.zip dist
#
# Writes <out-dir>/Lumenwall-Setup.exe (the same name for every version), then checks
# that it holds the programs (with 7z when it is installed: it reads NSIS installers).

set -eu

die() {
    printf 'build-installer: %s\n' "$*" >&2
    exit 1
}

[ $# -eq 3 ] || die "usage: build-installer.sh <version> <release zip> <out dir>"
version=$1
zip=$2
out=$3
case "$version" in
    v*) die "the version is given without the v" ;;
esac
command -v makensis >/dev/null 2>&1 || die "makensis (NSIS 3) is not installed"
[ -f "$zip" ] || die "$zip is missing"
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)

tmp=$(mktemp -d "${TMPDIR:-/tmp}/lumenwall-installer.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
python3 -m zipfile -e "$zip" "$tmp" 2>/dev/null ||
    python -m zipfile -e "$zip" "$tmp" 2>/dev/null ||
    unzip -q "$zip" -d "$tmp" ||
    die "cannot extract $zip (python3 or unzip is needed)"
src="$tmp/lumenwall-$version-x86_64-pc-windows-msvc"
for f in lumenwall.exe lumenwall-ui.exe lumenwall.bundle; do
    [ -f "$src/$f" ] || die "$f is missing from $zip"
done

# Windows version resources take four numbers: 0.3.0-beta.1 -> 0.3.0.0.
numeric="${version%%-*}.0"
setup="$out/Lumenwall-Setup.exe"
rm -f "$setup"
makensis -V2 -INPUTCHARSET UTF8 \
    -DVERSION="$version" -DVERSION_NUMERIC="$numeric" \
    -DSRCDIR="$src" -DOUTFILE="$setup" \
    "$here/lumenwall.nsi"
[ -s "$setup" ] || die "makensis wrote no installer"

if command -v 7z >/dev/null 2>&1; then
    listing=$(7z l "$setup")
    for f in lumenwall.exe lumenwall-ui.exe lumenwall.bundle LICENSE-MIT; do
        printf '%s\n' "$listing" | grep -q "$f" || die "$f is not in the installer"
    done
    echo "checked: the installer holds the programs"
else
    echo "7z is not installed: contents not checked"
fi
echo "$setup"
