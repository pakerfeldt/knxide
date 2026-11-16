#!/bin/sh
# Build knxide and put it where the launcher of a Linux desktop finds it, for this user only:
# the program in ~/.local/bin, a launcher entry and the icon under ~/.local/share.
# `install.sh --remove` takes all of it away again. Projects and their edits are not touched.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin=${XDG_BIN_HOME:-$HOME/.local/bin}
data=${XDG_DATA_HOME:-$HOME/.local/share}
entry=$data/applications/knxide.desktop
icons=$data/icons/hicolor

refresh() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$data/applications" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true
}

if [ "${1:-}" = "--remove" ]; then
    rm -f "$bin/knxide" "$entry" "$icons/scalable/apps/knxide.svg"
    for size in 32 128 256 512; do
        rm -f "$icons/${size}x${size}/apps/knxide.png"
    done
    refresh
    echo "knxide removed"
    exit 0
fi

cargo build --release --manifest-path "$here/Cargo.toml"

install -Dm755 "$here/target/release/knxide" "$bin/knxide"
install -Dm644 "$here/icons/icon.svg" "$icons/scalable/apps/knxide.svg"
install -Dm644 "$here/icons/32x32.png" "$icons/32x32/apps/knxide.png"
install -Dm644 "$here/icons/128x128.png" "$icons/128x128/apps/knxide.png"
install -Dm644 "$here/icons/128x128@2x.png" "$icons/256x256/apps/knxide.png"
install -Dm644 "$here/icons/icon.png" "$icons/512x512/apps/knxide.png"
mkdir -p "$data/applications"
# The launcher starts the program by its full path: ~/.local/bin is not on every PATH.
sed "s|^Exec=.*|Exec=$bin/knxide|" "$here/knxide.desktop" > "$entry"
refresh
echo "knxide installed: $bin/knxide"
