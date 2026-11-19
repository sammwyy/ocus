#!/bin/sh
set -eu
packages=$(cd "${1:-target/release}" && pwd)
expected=${2:-0.1.0}
work=$(mktemp -d)
app=/Applications/Ocus.app
test ! -e "$app" || { echo 'Refusing to replace an existing Ocus installation' >&2; exit 1; }
mounted=false
cleanup() {
    if "$mounted"; then hdiutil detach "$work/mount"; fi
    rm -rf "$app" "$work"
}
trap cleanup EXIT HUP INT TERM
set -- "$packages"/*.dmg
test "$#" -eq 1 && test -f "$1"
hdiutil attach "$1" -nobrowse -mountpoint "$work/mount"
mounted=true
ditto "$work/mount/Ocus.app" "$app"
hdiutil detach "$work/mount"
mounted=false
output=$("$app/Contents/MacOS/ocus" --version)
printf '%s\n' "$output"
printf '%s\n' "$output" | grep -E "^ocus $expected \([0-9a-f]{7}\)$"
test -f "$app/Contents/Info.plist"
test -n "$(find "$app/Contents/Resources" -name '*.icns' -print -quit)"
rm -rf "$app"
test ! -e "$app"
echo 'macOS app, icon and DMG install/uninstall smoke test passed'
