#!/bin/sh
# Remove what install.sh put in place. Leaves ~/.config/mac-trackpad alone.
set -eu
SHARE="${XDG_DATA_HOME:-$HOME/.local/share}"

rm -f "$HOME/.local/bin/mac-trackpad-settings"
rm -rf "$SHARE/mac-trackpad-settings"
rm -f "$SHARE/applications/mac-trackpad-settings.desktop"
rm -f "$SHARE/plasma/systemsettings/externalmodules/mac-trackpad-settings.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$SHARE/applications" || true
echo "removed mac-trackpad-settings"
