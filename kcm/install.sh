#!/bin/sh
# Build the Apple Magic Trackpad page and install it into System Settings
# (Mouse & Touchpad). The install step needs sudo, because Plasma only looks
# for these modules in the system's Qt plugin folder. Rebuild after a major
# Plasma or Qt update. Undo with ./uninstall.sh.
# Needs cmake, extra-cmake-modules, g++, and the KF6 KCMUtils, CoreAddons and
# I18n development files.
set -eu
cd "$(dirname "$0")"

cmake -S . -B build-system -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTING=OFF -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build-system -j"$(nproc)"
sudo cmake --install build-system

# This page replaces the settings app's own System Settings entry; the app
# stays available from the launcher.
rm -f "${XDG_DATA_HOME:-$HOME/.local/share}/plasma/systemsettings/externalmodules/mac-trackpad-settings.desktop"
echo "Installed: System Settings > Mouse & Touchpad > Apple Magic Trackpad"
