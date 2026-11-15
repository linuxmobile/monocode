#!/usr/bin/env bash
# Installs MonoCode into a user-level directory.
#
# Everything lands under ~/.local, so no root is needed. The application itself
# manages its remote host service; this script only puts the files where the
# desktop and the launcher will find them.
#
# Deliberately plain Bash: the work is copying files and writing one desktop
# entry, and a build system would be more code than the task.
set -euo pipefail

APP_ID="dev.usemono.gpui"
BINARY_NAME="monocode-gpui"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
PREFIX="$DATA_DIR/monocode-native"
APPLICATIONS="$DATA_DIR/applications"
ICONS="$DATA_DIR/icons/hicolor/256x256/apps"

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  --binary <path>   Executable to install (default: build output)
  --prefix <dir>    Application directory (default: ~/.local/share/monocode-native)
  --help            Show this message

Installs the desktop entry, icon and licences under ~/.local. The remote host
service is installed separately with `monocode-host install`.
EOF
}

source_dir() {
    # Resolve the repository root from this script, so the installer works from
    # any working directory and through a symlink.
    local script
    script="$(readlink -f "${BASH_SOURCE[0]}")"
    dirname "$(dirname "$script")"
}

binary=""
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) binary="${2:?--binary needs a path}"; shift 2 ;;
        --prefix) PREFIX="${2:?--prefix needs a path}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

root="$(source_dir)"
if [ -z "$binary" ]; then
    binary="$root/crates/monocode-gpui/target/release/$BINARY_NAME"
fi
if [ ! -x "$binary" ]; then
    echo "No executable at $binary." >&2
    echo "Build one first: cargo xtask build --release" >&2
    exit 2
fi

# A stale install would otherwise keep running the previous binary, which is
# confusing to debug, so the destination is always replaced.
echo "Installing $PREFIX"
mkdir -p "$PREFIX/licenses" "$BIN_DIR" "$APPLICATIONS" "$ICONS"
install -m 0755 "$binary" "$PREFIX/$BINARY_NAME"

for notice in \
    "$root/LICENSE" \
    "$root/NOTICE" \
    "$root/docs/native/LICENSE-GPUI-APACHE" \
    "$root/docs/native/LICENSE-MERMAID-MIT" \
    "$root/crates/monocode-gpui/assets/hugeicons.LICENSE" \
    "$root/crates/monocode-gpui/assets/dark-plus.LICENSE" \
    "$root/crates/monocode-gpui/assets/native-editor.LICENSE.txt" \
    "$root/crates/monocode-gpui/assets/material/LICENSE"
do
    [ -f "$notice" ] || continue
    name="$(basename "$notice")"
    # Material's licence is MIT; the others are named after their asset.
    if [ "$name" = "LICENSE" ] && [ "$(basename "$(dirname "$notice")")" = "material" ]; then
        name="material-icons.LICENSE"
    fi
    install -m 0644 "$notice" "$PREFIX/licenses/$name"
done

# The packaged app reads this to know it was installed rather than run from a
# build directory, which changes where it stores its data.
printf '{"app_id":"%s","kind":"install"}\n' "$APP_ID" > "$PREFIX/native-install.json"

install -m 0755 "$root/assets/icons/monocode.png" "$ICONS/monocode-native.png"

# A wrapper rather than a symlink: it keeps the real path stable when a new
# version replaces the binary, and gives one place to set the working directory.
cat > "$BIN_DIR/monocode-native" <<EOF
#!/bin/sh
exec "$PREFIX/$BINARY_NAME" "\$@"
EOF
chmod 0755 "$BIN_DIR/monocode-native"

cat > "$APPLICATIONS/monocode-native.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=MonoCode
Exec=$BIN_DIR/monocode-native %F
Icon=monocode-native
Categories=Development;
Terminal=false
StartupWMClass=$APP_ID
EOF
chmod 0644 "$APPLICATIONS/monocode-native.desktop"

# Refresh the caches so the launcher shows the entry without a logout. Both are
# optional: a minimal system may not have them, and failing here would undo a
# successful install.
command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$APPLICATIONS" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -q "$DATA_DIR/icons/hicolor" 2>/dev/null || true

echo "Installed. Run: $BIN_DIR/monocode-native"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "$BIN_DIR is not on PATH; add it to use the monocode-native command." ;;
esac
