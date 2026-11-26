#!/bin/sh
set -eu
packages=$(realpath "${1:-target/release}")
expected=${2:-0.1.1}
work=$(mktemp -d)
installed=false
cleanup() {
    if "$installed"; then sudo dpkg --remove ocus; fi
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM
check_version() {
    output=$(env -u DISPLAY -u WAYLAND_DISPLAY "$1" --version)
    printf '%s\n' "$output"
    printf '%s\n' "$output" | grep -E "^ocus $expected \([0-9a-f]{7}; [^;]+; (stable|nightly|indev)\)$"
}
if dpkg-query -W -f='${Status}' ocus 2>/dev/null | grep -q 'ok installed'; then
    echo 'Refusing to replace an existing Ocus installation' >&2
    exit 1
fi
set -- "$packages"/*.deb
test "$#" -eq 1 && test -f "$1"
echo "Installing $1"
installed=true
sudo dpkg -i "$1"
check_version /usr/bin/ocus
desktop=$(dpkg -L ocus | grep '/applications/.*\.desktop$')
icon=$(dpkg -L ocus | grep '/icons/.*\.png$' | head -n 1)
test -f "$desktop" && test -f "$icon"
echo "Desktop entry: $desktop; icon: $icon"
sudo dpkg --remove ocus
installed=false
test ! -e /usr/bin/ocus && test ! -e "$desktop" && test ! -e "$icon"
echo 'Deb uninstall verified'
set -- "$packages"/*.AppImage
test "$#" -eq 1 && test -f "$1"
image=$1
echo "Extracting $image"
cd "$work"
chmod +x "$image"
"$image" --appimage-extract > extract.log
check_version "$work/squashfs-root/AppRun"
test -n "$(find squashfs-root -name '*.desktop' -print -quit)"
test -n "$(find squashfs-root -name '*.png' -print -quit)"
rm -rf "$work/squashfs-root"
test ! -e "$work/squashfs-root"
echo 'AppImage desktop entry/icon and removal verified'
echo 'Linux installer smoke tests passed'
