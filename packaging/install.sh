#!/bin/sh
# Installs Haze for the current user: binary, icons, desktop entry. Re-run to update.
# Undo: rm ~/.local/bin/haze ~/.local/share/applications/haze.desktop ~/.local/share/icons/hicolor/*/apps/haze.*
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}

cargo build --release -p katana-studio --manifest-path "$root/Cargo.toml"
install -Dm755 "$root/target/release/haze" "$HOME/.local/bin/haze"
install -Dm644 "$root/packaging/haze.svg" "$data/icons/hicolor/scalable/apps/haze.svg"
for s in 32 48 64 128 256 512; do
    install -Dm644 "$root/packaging/haze-$s.png" "$data/icons/hicolor/${s}x${s}/apps/haze.png"
done
install -Dm644 "$root/packaging/haze.desktop" "$data/applications/haze.desktop"
gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$data/applications" 2>/dev/null || true
echo "Haze zainstalowany: ~/.local/bin/haze"
