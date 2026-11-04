#!/bin/sh
# Lumenwall installer for Linux, for the current user (no sudo):
#
#   curl -fsSL https://github.com/simnJS/lumenwall/releases/latest/download/install.sh | sh
#   curl -fsSL https://github.com/simnJS/lumenwall/releases/latest/download/install.sh | sh -s -- --uninstall
#
# Downloads the latest release archive, checks it against the release's SHA256SUMS
# and, when minisign is installed, that list against SHA256SUMS.minisig, the
# release's one signature (key below, the same one the app checks its updates
# with), then installs into ~/.local:
#
#   ~/.local/bin/lumenwall, ~/.local/bin/lumenwall-ui
#   ~/.local/share/applications/lumenwall-ui.desktop, ~/.local/share/icons/hicolor/...
#   ~/.config/autostart/lumenwall.desktop                       (unless --no-autostart)
#   ~/.local/share/gnome-shell/extensions/lumenwall@simnjs.github.io   (GNOME only)
#
# The same files as the app's installation dialog (Settings > Installation);
# settings and the wallpaper library are never touched. Published with every
# release; source: packaging/linux/install.sh. MIT OR Apache-2.0.

set -eu

REPO_URL="https://github.com/simnJS/lumenwall"
# The release signing key (docs/RELEASING.md, crates/lumenwall-update RELEASE_PUBLIC_KEY).
RELEASE_KEY="RWQt/7tWeHNqM8Z/BqlP0VmbIHU4DhM9iWA8dzM7rfrHXK5ZxtoIHMbY"
TARGET="x86_64-unknown-linux-gnu"
UUID="lumenwall@simnjs.github.io"
# Icons, relative to icons/hicolor (packaging/linux/icons/hicolor).
ICONS="16x16/apps/lumenwall.png 24x24/apps/lumenwall.png 32x32/apps/lumenwall.png
48x48/apps/lumenwall.png 64x64/apps/lumenwall.png 128x128/apps/lumenwall.png
256x256/apps/lumenwall.png 512x512/apps/lumenwall.png scalable/apps/lumenwall.svg
symbolic/apps/lumenwall-symbolic.svg symbolic/apps/lumenwall-paused-symbolic.svg"

# Testing only: another release server and key (see docs/RELEASING.md).
RELEASES_URL="${LUMENWALL_RELEASES_URL:-$REPO_URL/releases}"
KEY="${LUMENWALL_PUBLIC_KEY:-$RELEASE_KEY}"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}"
EXT_DIR="$DATA_DIR/gnome-shell/extensions/$UUID"

say() { printf '%s\n' "$*"; }
step() { printf '  %s\n' "$*"; }
die() {
    printf 'lumenwall install: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Lumenwall installer (Linux, current user, no sudo)

  install.sh [--version X.Y.Z] [--no-autostart] [--no-gnome-extension]
  install.sh --uninstall

  --version X.Y.Z         install this release instead of the latest one
  --no-autostart          do not start Lumenwall at login
  --no-gnome-extension    do not install the GNOME Shell extension
  --require-signature     fail when minisign is not installed
  --uninstall             remove what this script (or the app) installed;
                          settings and wallpapers are kept
EOF
}

VERSION=""
AUTOSTART=1
GNOME_EXT=1
UNINSTALL=0
REQUIRE_SIG=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            [ $# -ge 2 ] || die "--version needs a value"
            VERSION="${2#v}"
            shift
            ;;
        --version=*)
            VERSION="${1#--version=}"
            VERSION="${VERSION#v}"
            ;;
        --no-autostart) AUTOSTART=0 ;;
        --no-gnome-extension) GNOME_EXT=0 ;;
        --require-signature) REQUIRE_SIG=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

[ "$(id -u)" -ne 0 ] || die "run this as your own user, not root (system-wide: use the .deb package)"
[ -n "${HOME:-}" ] || die "HOME is not set"

is_gnome() {
    case ":${XDG_CURRENT_DESKTOP:-}:" in
        *:GNOME:* | *:gnome:*) return 0 ;;
    esac
    [ -z "${XDG_CURRENT_DESKTOP:-}" ] && case "${DESKTOP_SESSION:-}" in *gnome*) return 0 ;; esac
    return 1
}

# ------------------------------------------------------------------ GNOME Shell

# The enabled-extensions list, edited through GSettings. Used when the running
# Shell does not know the extension yet (a Wayland session discovers new ones at
# login): the key is read again then.
gsettings_list_edit() { # add|remove
    command -v gsettings >/dev/null 2>&1 || return 1
    list=$(gsettings get org.gnome.shell enabled-extensions 2>/dev/null) || return 1
    case "$list" in
        "@as []" | "[]") items="" ;;
        "["*"]") items=$(printf '%s' "$list" | sed -e 's/^\[//' -e 's/\]$//') ;;
        *) return 1 ;;
    esac
    new=$(printf '%s' "$items" | tr ',' '\n' | sed -e 's/^ *//' -e 's/ *$//' |
        grep -v -e "^'$UUID'\$" -e '^$' | paste -sd, - | sed 's/,/, /g')
    if [ "$1" = add ]; then
        if [ -n "$new" ]; then new="$new, '$UUID'"; else new="'$UUID'"; fi
    fi
    gsettings set org.gnome.shell enabled-extensions "[$new]"
}

# Prints "now" or "login" (when it takes effect).
enable_extension() {
    if command -v gnome-extensions >/dev/null 2>&1 &&
        gnome-extensions enable "$UUID" >/dev/null 2>&1; then
        echo now
    elif gsettings_list_edit add; then
        echo login
    else
        return 1
    fi
}

extension_enabled() {
    command -v gsettings >/dev/null 2>&1 &&
        gsettings get org.gnome.shell enabled-extensions 2>/dev/null | grep -q "'$UUID'"
}

# version-name of a metadata.json ("" when there is none).
ext_version() {
    [ -f "$1" ] || return 0
    sed -n 's/.*"version-name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

# True when dotted version $1 is older than $2 ("" is older than anything).
older() {
    [ -n "$1" ] || return 0
    [ "$1" = "$2" ] && return 1
    lowest=$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | head -n 1)
    [ "$lowest" = "$1" ]
}

# ------------------------------------------------------------------ desktop entries

# A path as one Exec argument (Desktop Entry Specification): quoted, with " ` $ \
# escaped, % doubled, then backslashes escaped again for the string syntax.
exec_arg() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/%/%%/g')" | sed 's/\\/\\\\/g'
}

string_value() {
    printf '%s' "$1" | sed 's/\\/\\\\/g'
}

write_app_entry() { # template exe dest
    E=$(exec_arg "$2") T=$(string_value "$2") awk '
        /^#/ { next }
        /^TryExec=/ { next }
        /^Exec=/ { print "TryExec=" ENVIRON["T"]; print "Exec=" ENVIRON["E"]; next }
        { print }
    ' "$1" >"$3.lumenwall-tmp"
    mv -f "$3.lumenwall-tmp" "$3"
}

# Same content as the app writes (crates/lumenwall-ui/src/setup/desktop.rs).
write_autostart_entry() { # exe dest
    {
        echo "[Desktop Entry]"
        echo "Type=Application"
        echo "Name=Lumenwall"
        echo "Comment=Starts the Lumenwall wallpaper engine and its tray icon"
        echo "Comment[fr]=Démarre le moteur de fonds d'écran Lumenwall et son icône de notification"
        echo "TryExec=$(string_value "$1")"
        echo "Exec=$(exec_arg "$1") --background"
        echo "Icon=lumenwall"
        echo "Terminal=false"
        echo "NoDisplay=true"
        echo "X-GNOME-Autostart-enabled=true"
        echo "X-GNOME-Autostart-Delay=3"
        echo "X-KDE-autostart-after=panel"
    } >"$2.lumenwall-tmp"
    mv -f "$2.lumenwall-tmp" "$2"
}

# ------------------------------------------------------------------ uninstall

uninstall() {
    say "Removing Lumenwall from this account"
    if [ -x "$BIN_DIR/lumenwall-ui" ]; then
        # Exit 0: stopped, 3: none was running; older versions do not know the option.
        if "$BIN_DIR/lumenwall-ui" --stop-engine >/dev/null 2>&1; then
            step "stopped the engine"
        fi
    fi
    for p in lumenwall lumenwall-ui; do
        if [ -e "$BIN_DIR/$p" ] || [ -L "$BIN_DIR/$p" ]; then
            rm -f "$BIN_DIR/$p"
            step "removed $BIN_DIR/$p"
        fi
    done
    for f in "$DATA_DIR/applications/lumenwall-ui.desktop" "$CONFIG_DIR/autostart/lumenwall.desktop"; do
        if [ -e "$f" ]; then
            rm -f "$f"
            step "removed $f"
        fi
    done
    removed_icons=0
    for i in $ICONS; do
        if [ -e "$DATA_DIR/icons/hicolor/$i" ]; then
            rm -f "$DATA_DIR/icons/hicolor/$i"
            removed_icons=1
        fi
    done
    [ "$removed_icons" -eq 0 ] || step "removed the icons from $DATA_DIR/icons/hicolor"
    if [ -d "$EXT_DIR" ]; then
        if extension_enabled; then
            if command -v gnome-extensions >/dev/null 2>&1 &&
                gnome-extensions disable "$UUID" >/dev/null 2>&1; then
                :
            else
                gsettings_list_edit remove || say "  could not turn the GNOME extension off; turn it off in Extensions"
            fi
        fi
        rm -rf "$EXT_DIR"
        step "removed the GNOME Shell extension"
    fi
    say "Done. Your settings (~/.config/lumenwall) and wallpapers are kept."
}

if [ "$UNINSTALL" -eq 1 ]; then
    uninstall
    exit 0
fi

# ------------------------------------------------------------------ download

[ "$(uname -s)" = Linux ] || die "this installer is for Linux; on Windows use the installer from $RELEASES_URL"
case "$(uname -m)" in
    x86_64 | amd64) ;;
    *) die "releases are built for x86-64 only; build from source: $REPO_URL" ;;
esac

# HTTPS only, redirects included (local test servers may use http or file).
case "$RELEASES_URL" in
    https://*) PROTO="=https" ;;
    *) PROTO="=https,http,file" ;;
esac
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --proto "$PROTO" --proto-redir "$PROTO" --retry 2 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
else
    die "curl or wget is needed"
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    die "sha256sum (coreutils) is needed to check the download"
fi
command -v tar >/dev/null 2>&1 || die "tar is needed"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/lumenwall-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
trap 'exit 1' INT TERM

if [ -n "$VERSION" ]; then
    base="$RELEASES_URL/download/v$VERSION"
else
    base="$RELEASES_URL/latest/download"
fi
say "Downloading Lumenwall${VERSION:+ $VERSION}"
fetch "$base/SHA256SUMS" "$TMP/SHA256SUMS" || die "cannot download $base/SHA256SUMS"
# The archive name gives the version; everything else comes from that release.
ARCHIVE=$(sed -n "s/^[0-9a-f]\{64\}  \(lumenwall-[0-9][^ ]*-$TARGET\.tar\.gz\)\$/\1/p" "$TMP/SHA256SUMS" | head -n 1)
[ -n "$ARCHIVE" ] || die "no Linux archive in the release's SHA256SUMS"
VERSION=${ARCHIVE#lumenwall-}
VERSION=${VERSION%-"$TARGET".tar.gz}
base="$RELEASES_URL/download/v$VERSION"
for f in SHA256SUMS.minisig "$ARCHIVE"; do
    fetch "$base/$f" "$TMP/$f" || die "cannot download $base/$f"
done

# ------------------------------------------------------------------ verify

# The release signs SHA256SUMS only: it binds the archive's name to its SHA-256,
# so a valid list plus a matching hash is the file the release key listed.
say "Checking Lumenwall $VERSION"
if command -v minisign >/dev/null 2>&1; then
    out=$(minisign -V -P "$KEY" -m "$TMP/SHA256SUMS" -x "$TMP/SHA256SUMS.minisig" 2>&1) ||
        die "invalid signature for SHA256SUMS: nothing was installed"
    # A valid signature of another file is refused too (same check as the app).
    printf '%s\n' "$out" | awk -v want="file:SHA256SUMS" '
        /^Trusted comment:/ { for (i = 3; i <= NF; i++) if ($i == want) ok = 1 }
        END { exit !ok }' ||
        die "the signature of SHA256SUMS was made for another file: nothing was installed"
    step "signature of SHA256SUMS: valid (Lumenwall release key)"
elif [ "$REQUIRE_SIG" -eq 1 ]; then
    die "minisign is not installed and --require-signature was given"
else
    step "signature: not checked (install minisign to check it too)"
fi
want=$(awk -v f="$ARCHIVE" '$2 == f { print $1; exit }' "$TMP/SHA256SUMS")
got=$(sha256 "$TMP/$ARCHIVE")
[ -n "$want" ] && [ "$want" = "$got" ] || die "the SHA-256 of $ARCHIVE differs from SHA256SUMS: nothing was installed"
step "SHA-256: matches SHA256SUMS"

mkdir "$TMP/x"
tar -xzf "$TMP/$ARCHIVE" -C "$TMP/x"
SRC="$TMP/x/lumenwall-$VERSION-$TARGET"
[ -f "$SRC/lumenwall" ] && [ -f "$SRC/lumenwall-ui" ] || die "the archive does not hold the Lumenwall programs"

# ------------------------------------------------------------------ install

say "Installing into ~/.local"
# An engine running from an earlier copy is restarted on the new one (exit 0:
# it was running and stopped; older versions do not know the option).
RESTART=0
if [ -x "$BIN_DIR/lumenwall-ui" ] && "$BIN_DIR/lumenwall-ui" --stop-engine >/dev/null 2>&1; then
    RESTART=1
fi
mkdir -p "$BIN_DIR" "$DATA_DIR/applications" "$DATA_DIR/icons/hicolor"
# Written beside, then renamed: a running copy keeps its file; the engine first.
for p in lumenwall lumenwall-ui; do
    cp "$SRC/$p" "$BIN_DIR/.$p.lumenwall-tmp"
    chmod 755 "$BIN_DIR/.$p.lumenwall-tmp"
    mv -f "$BIN_DIR/.$p.lumenwall-tmp" "$BIN_DIR/$p"
done
step "programs: $BIN_DIR/lumenwall, $BIN_DIR/lumenwall-ui"
if [ "$RESTART" -eq 1 ]; then
    (cd / && nohup "$BIN_DIR/lumenwall" >/dev/null 2>&1 &)
    step "restarted the engine on version $VERSION"
fi

for i in $ICONS; do
    if [ -f "$SRC/share/icons/hicolor/$i" ]; then
        mkdir -p "$DATA_DIR/icons/hicolor/$(dirname "$i")"
        cp "$SRC/share/icons/hicolor/$i" "$DATA_DIR/icons/hicolor/$i"
    fi
done
write_app_entry "$SRC/share/applications/lumenwall-ui.desktop" "$BIN_DIR/lumenwall-ui" \
    "$DATA_DIR/applications/lumenwall-ui.desktop"
step "applications menu entry and icons"

if [ "$AUTOSTART" -eq 1 ]; then
    mkdir -p "$CONFIG_DIR/autostart"
    write_autostart_entry "$BIN_DIR/lumenwall-ui" "$CONFIG_DIR/autostart/lumenwall.desktop"
    step "starts at login: $CONFIG_DIR/autostart/lumenwall.desktop"
fi

RELOGIN=0
if [ "$GNOME_EXT" -eq 1 ] && is_gnome && [ -d "$SRC/share/gnome-shell/extensions/$UUID" ]; then
    new=$(ext_version "$SRC/share/gnome-shell/extensions/$UUID/metadata.json")
    old=$(ext_version "$EXT_DIR/metadata.json")
    if [ ! -d "$EXT_DIR" ] || older "$old" "$new"; then
        mkdir -p "$(dirname "$EXT_DIR")"
        rm -rf "$EXT_DIR.lumenwall-tmp"
        cp -R "$SRC/share/gnome-shell/extensions/$UUID" "$EXT_DIR.lumenwall-tmp"
        rm -rf "$EXT_DIR"
        mv "$EXT_DIR.lumenwall-tmp" "$EXT_DIR"
        RELOGIN=1
        step "GNOME Shell extension $new"
    fi
    if ! extension_enabled; then
        if when=$(enable_extension); then
            [ "$when" = now ] || RELOGIN=1
            step "GNOME Shell extension turned on"
        else
            say "  could not turn the GNOME Shell extension on: run 'gnome-extensions enable $UUID' after logging in again"
        fi
    fi
fi

say ""
say "Lumenwall $VERSION is installed."
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) say "Note: $BIN_DIR is not in your PATH; the menu entry works anyway." ;;
esac
if [ "$RELOGIN" -eq 1 ]; then
    say "Log out and back in to turn on pausing behind maximised windows and desktop clicks (GNOME extension)."
fi
say "Start it from your applications menu (Lumenwall), or run: lumenwall-ui"
say "It updates itself; to remove it: curl -fsSL $RELEASES_URL/latest/download/install.sh | sh -s -- --uninstall"
