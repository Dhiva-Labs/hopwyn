#!/usr/bin/env bash
# Builds Hopwyn and installs it for the current user (no sudo):
#   ~/.local/bin/hopwyn-app, its icon and its launcher entry.
# Usage: scripts/install-linux.sh [--autostart] [--uninstall]
set -euo pipefail

APP_ID="app.hopwyn.Hopwyn"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA="$ROOT/crates/hopwyn-app/data"
BIN_DIR="${HOME}/.local/bin"
SHARE="${XDG_DATA_HOME:-$HOME/.local/share}"
ICON_DIR="$SHARE/icons/hicolor/scalable/apps"
APPS_DIR="$SHARE/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"

refresh_caches() {
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS_DIR" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$SHARE/icons/hicolor" || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$BIN_DIR/hopwyn-app" "$ICON_DIR/$APP_ID.svg" "$APPS_DIR/$APP_ID.desktop" "$AUTOSTART_DIR/$APP_ID.desktop"
  refresh_caches
  echo "Hopwyn removed. Your pairing and settings remain in ~/.config/hopwyn (delete it to reset)."
  exit 0
fi

echo "Building Hopwyn (release)..."
cargo build --release -p hopwyn-app --manifest-path "$ROOT/Cargo.toml"

install -Dm755 "$ROOT/target/release/hopwyn-app" "$BIN_DIR/hopwyn-app"
install -Dm644 "$DATA/icons/hicolor/scalable/apps/$APP_ID.svg" "$ICON_DIR/$APP_ID.svg"
install -Dm644 "$DATA/$APP_ID.desktop" "$APPS_DIR/$APP_ID.desktop"
# Launchers don't use the shell's PATH, so point Exec at the installed binary.
sed -i "s|^Exec=.*|Exec=$BIN_DIR/hopwyn-app|" "$APPS_DIR/$APP_ID.desktop"

if [[ "${1:-}" == "--autostart" ]]; then
  install -Dm644 "$APPS_DIR/$APP_ID.desktop" "$AUTOSTART_DIR/$APP_ID.desktop"
  echo "Hopwyn will start when you log in."
fi

refresh_caches
echo "Installed. Open Hopwyn from your app launcher, or run: $BIN_DIR/hopwyn-app"
