#!/bin/sh
# Install the Apple Magic Trackpad settings app for this user:
#   ~/.local/share/mac-trackpad-settings/   the app (script + Python package + QML)
#   ~/.local/bin/mac-trackpad-settings      symlink to it
#   ~/.local/share/applications/mac-trackpad-settings.desktop
#   ~/.local/share/plasma/systemsettings/externalmodules/mac-trackpad-settings.desktop
#     (its entry under Mouse & Touchpad in System Settings; harmless elsewhere)
# Needs PyQt6 with Qt Quick Controls 2 (package names: see README.md).
# Undo with ./uninstall.sh.
set -eu
cd "$(dirname "$0")"

python3 -c 'import PyQt6.QtWidgets, PyQt6.QtQml, PyQt6.QtQuick' 2>/dev/null || {
    echo "error: PyQt6 (with QtQml and QtQuick) isn't importable by python3." >&2
    echo "Install it first; package names per distribution are in the README (Settings app)." >&2
    exit 1
}

SHARE="${XDG_DATA_HOME:-$HOME/.local/share}"
DEST="$SHARE/mac-trackpad-settings"
BIN="$HOME/.local/bin"
APPS="$SHARE/applications"
MODULES="$SHARE/plasma/systemsettings/externalmodules"

rm -rf "$DEST"
mkdir -p "$DEST" "$BIN" "$APPS" "$MODULES"
install -m 755 mac-trackpad-settings "$DEST/mac-trackpad-settings"
mkdir -p "$DEST/mac_trackpad_settings/qml"
install -m 644 mac_trackpad_settings/*.py "$DEST/mac_trackpad_settings/"
install -m 644 mac_trackpad_settings/qml/*.qml "$DEST/mac_trackpad_settings/qml/"
ln -sfn "$DEST/mac-trackpad-settings" "$BIN/mac-trackpad-settings"

cat > "$APPS/mac-trackpad-settings.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Apple Magic Trackpad
GenericName=Trackpad Settings
Comment=Tracking speed, scroll speed, inertia and three finger drag
Exec=$DEST/mac-trackpad-settings
Icon=input-touchpad
Terminal=false
Categories=Settings;HardwareSettings;Qt;
Keywords=trackpad;touchpad;scrolling;inertia;tracking;mac;
EOF
# The compiled page (kcm/) takes over the System Settings entry if installed.
if command -v systemsettings >/dev/null 2>&1 &&
    systemsettings --list 2>/dev/null | grep -q kcm_apple_magic_trackpad; then
    rm -f "$MODULES/mac-trackpad-settings.desktop"
else
    {
        cat "$APPS/mac-trackpad-settings.desktop"
        echo "X-KDE-System-Settings-Parent-Category=pointing-devices"
    } > "$MODULES/mac-trackpad-settings.desktop"
fi
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS" || true

echo "installed: $BIN/mac-trackpad-settings -> $DEST"
command -v mac-trackpad-accel >/dev/null 2>&1 || [ -x "$BIN/mac-trackpad-accel" ] ||
    echo "note: mac-trackpad-accel isn't installed (accel/install.sh); curve changes will fail until it is"
