#!/bin/sh
# Remove what install.sh put in the system folders.
set -eu
cd "$(dirname "$0")"

[ -f build-system/install_manifest.txt ] || {
    echo "error: build-system/install_manifest.txt not found; nothing to remove" >&2
    exit 1
}
xargs -r sudo rm -f < build-system/install_manifest.txt
echo "Removed. Run app/install.sh to get the settings app's own entry back."
