#!/bin/sh
# Remove the KWin acceleration shim. Takes effect at the next login;
# until then, choosing "Flat" acceleration in System Settings bypasses it.
set -eu
CONF="${XDG_CONFIG_HOME:-$HOME/.config}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/mac-trackpad"

# Hyprland (see install.sh): remove the tool, its data and the generated
# Hyprland file; there is no shim, sudo or drop-in to undo. A hyprlang file
# that hyprland.conf still sources is kept (Hyprland would report the missing
# file), with a note to remove the line first.
case "$(printf '%s' "${XDG_CURRENT_DESKTOP:-}" | tr '[:upper:]' '[:lower:]')" in
    *hyprland*) HYPRLAND=1 ;;
    *) HYPRLAND="${HYPRLAND_INSTANCE_SIGNATURE:+1}" ;;
esac
if [ -n "$HYPRLAND" ]; then
    rm -f "$HOME/.local/bin/mac-trackpad-accel"
    rm -f "$DATA/mac_curves.py" "$DATA/mac_hyprland.py" "$DATA/mac-params.json"
    rmdir "$DATA" 2>/dev/null || true
    rm -f "${XDG_STATE_HOME:-$HOME/.local/state}/omarchy/toggles/hypr/mac-trackpad-accel.lua"
    for f in "$CONF/hypr/mac-trackpad.conf:hyprland.conf" "$CONF/hypr/mac-trackpad.lua:hyprland.lua"; do
        file="${f%:*}" main="$CONF/hypr/${f##*:}"
        [ -e "$file" ] || continue
        if [ -f "$main" ] && grep -qF "$(basename "$file")" "$main"; then
            echo "Kept $file: remove the line that loads it from $main, then delete it."
        else
            rm -f "$file"
        fi
    done
    echo "Removed. Run 'hyprctl reload' (or log out and in) to drop the curves. (Curve config left in $CONF/mac-trackpad/.)"
    exit 0
fi

rm -f "$CONF/systemd/user/plasma-kwin_wayland.service.d/mac-trackpad-accel.conf"
rmdir "$CONF/systemd/user/plasma-kwin_wayland.service.d" 2>/dev/null || true
systemctl --user daemon-reload
rm -f "$HOME/.local/bin/mac-trackpad-accel"
rm -f "$DATA/mac_curves.py" "$DATA/mac-params.json"
rmdir "$DATA" 2>/dev/null || true
sudo rm -f /usr/lib/libmac-accel-shim.so
echo "Removed. Log out and back in. (Curve config left in $CONF/mac-trackpad/.)"
