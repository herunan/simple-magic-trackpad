#!/bin/sh
# Build and install the mac-trackpad daemon for this user. Does not start
# it or touch three-finger-drag; see the README "Switching over" section.
set -eu
cd "$(dirname "$0")"
cargo build --release
install -Dm755 target/release/mac-trackpad "$HOME/.local/bin/mac-trackpad"
install -Dm644 mac-trackpad.service "$HOME/.config/systemd/user/mac-trackpad.service"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}/mac-trackpad/daemon.json"
if [ ! -e "$CONF" ]; then
    old="${XDG_CONFIG_HOME:-$HOME/.config}/linux-3-finger-drag/3fd-config.json"
    mkdir -p "$(dirname "$CONF")"
    if [ -e "$old" ]; then cp "$old" "$CONF"; else cp daemon.json "$CONF"; fi
    echo "wrote $CONF"
fi
systemctl --user daemon-reload
# A running daemon keeps using the old binary until restarted.
if systemctl --user -q is-active mac-trackpad.service; then
    systemctl --user restart mac-trackpad.service
    echo "restarted mac-trackpad.service"
fi
