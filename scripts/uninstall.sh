#!/usr/bin/env bash
# Removes a user-level install created by install.sh.
#
# The data directory is never deleted: sessions, credentials and logs belong to
# the user, and an uninstall that erased them would be data loss. The script
# says what was left behind so removing it is a deliberate choice.
set -euo pipefail

DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
PREFIX="$DATA_DIR/monocode-native"
APPLICATIONS="$DATA_DIR/applications"
ICONS="$DATA_DIR/icons/hicolor/256x256/apps"

usage() {
    cat <<'EOF'
Usage: uninstall.sh [--prefix <dir>] [--purge]

  --prefix <dir>   Application directory to remove
  --purge          Also delete MonoCode's user data
  --help           Show this message

Removes the installed files. User data is kept unless --purge is given.
The remote host service is removed separately with `monocode-host uninstall`.
EOF
}

purge=0
while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="${2:?--prefix needs a path}"; shift 2 ;;
        --purge) purge=1; shift ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

removed=0
remove() {
    if [ -e "$1" ]; then
        rm -rf "$1"
        removed=$((removed + 1))
    fi
}

remove "$PREFIX"
remove "$BIN_DIR/monocode-native"
remove "$APPLICATIONS/monocode-native.desktop"
remove "$ICONS/monocode-native.png"

command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$APPLICATIONS" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -q "$DATA_DIR/icons/hicolor" 2>/dev/null || true

if [ "$removed" -eq 0 ]; then
    echo "Nothing was installed at $PREFIX."
fi

if [ "$purge" -eq 1 ]; then
    # Only the directories this application owns, named explicitly so a typo in
    # a variable cannot empty a user directory.
    for owned in "$DATA_DIR/monocode-native" "$HOME/.monocode-host"; do
        if [ -d "$owned" ]; then
            rm -rf "$owned"
            echo "Removed $owned"
        fi
    done
else
    # `--purge` only removes the remote host's state directory. The desktop's
    # own data lives elsewhere and is never deleted here, so say where it is
    # instead of implying this script could remove it.
    echo "MonoCode's application files were removed."
    echo "Your own data was kept:"
    echo "  ${XDG_DATA_HOME:-$HOME/.local/share}/Monocode Development   (sessions and settings)"
    echo "  $HOME/.monocode-host                                        (remote host state, if any)"
    echo "Re-run with --purge to remove the remote host state directory as well."
fi
