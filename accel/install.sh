#!/bin/sh
# Install the Mac acceleration curve shim for KWin. Takes effect at the next
# login. Undo with ./uninstall.sh.
#
# kwin_wayland has the cap_sys_nice file capability, so glibc runs it in
# secure-execution mode: LD_PRELOAD entries containing a slash are ignored,
# and a bare name is only loaded from the standard library directories when
# the file has the set-user-ID bit. Hence the one root step: the .so goes to
# /usr/lib, root-owned, mode 4755, and the drop-in preloads it by bare name.
set -eu
cd "$(dirname "$0")"

SHIM=/usr/lib/libmac-accel-shim.so
CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/mac-trackpad"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/mac-trackpad"
DROPIN_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/plasma-kwin_wayland.service.d"

# Hyprland applies libinput custom curves itself, so none of the KWin part
# (shim, sudo, systemd drop-in) is needed: install the tool and its data,
# write accel.conf for Hyprland's scroll factor of 1, and let `reload` write
# and apply the Hyprland config file (it prints the one line to add to
# hyprland.conf if Hyprland runs a hyprlang config; it never edits it).
case "$(printf '%s' "${XDG_CURRENT_DESKTOP:-}" | tr '[:upper:]' '[:lower:]')" in
    *hyprland*) HYPRLAND=1 ;;
    *) HYPRLAND="${HYPRLAND_INSTANCE_SIGNATURE:+1}" ;;
esac
if [ -n "$HYPRLAND" ]; then
    install -Dm755 mac-trackpad-accel "$HOME/.local/bin/mac-trackpad-accel"
    install -Dm644 mac_curves.py "$DATA_DIR/mac_curves.py"
    install -Dm644 mac_hyprland.py "$DATA_DIR/mac_hyprland.py"
    install -Dm644 mac-params.json "$DATA_DIR/mac-params.json"
    mkdir -p "$CONF_DIR"
    if [ ! -e "$CONF_DIR/accel.conf" ]; then
        python3 mac_curves.py --kwin-scroll-factor 1 > "$CONF_DIR/accel.conf"
        echo "wrote $CONF_DIR/accel.conf"
    fi
    "$HOME/.local/bin/mac-trackpad-accel" reload
    echo "Installed for Hyprland."
    exit 0
fi

make libmac-accel-shim.so
sudo install -m4755 -o root -g root libmac-accel-shim.so "$SHIM"
# Earlier installs put it here, where KWin never loaded it.
rm -f "$HOME/.local/lib/mac-trackpad/libmac-accel-shim.so"
rmdir "$HOME/.local/lib/mac-trackpad" 2>/dev/null || true
install -Dm755 mac-trackpad-accel "$HOME/.local/bin/mac-trackpad-accel"
# mac-trackpad-accel and the settings app look for these here, wherever the
# repo was cloned.
install -Dm644 mac_curves.py "$DATA_DIR/mac_curves.py"
install -Dm644 mac-params.json "$DATA_DIR/mac-params.json"

mkdir -p "$CONF_DIR"
if [ ! -e "$CONF_DIR/accel.conf" ]; then
    python3 mac_curves.py > "$CONF_DIR/accel.conf"
    echo "wrote $CONF_DIR/accel.conf"
fi

mkdir -p "$DROPIN_DIR"
cat > "$DROPIN_DIR/mac-trackpad-accel.conf" <<CONF
# Installed by mac-trackpad accel/install.sh; remove with accel/uninstall.sh
[Service]
Environment=LD_PRELOAD=libmac-accel-shim.so
CONF
systemctl --user daemon-reload
echo "Installed. Log out and back in for KWin to load it."
