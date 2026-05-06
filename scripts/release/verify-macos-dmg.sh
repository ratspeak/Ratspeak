#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <dmg-path> <expected-arch>" >&2
  exit 2
fi

dmg_path="$1"
expected_arch="$2"
mount_dir="$(mktemp -d)"

cleanup() {
  hdiutil detach "$mount_dir" >/dev/null 2>&1 || true
  rmdir "$mount_dir" >/dev/null 2>&1 || true
}
trap cleanup EXIT

hdiutil attach -nobrowse -readonly -mountpoint "$mount_dir" "$dmg_path" >/dev/null

if [ ! -f "$mount_dir/.DS_Store" ]; then
  echo "Missing Finder .DS_Store layout metadata in $dmg_path" >&2
  exit 1
fi

if [ ! -f "$mount_dir/.background/dmg-background.png" ]; then
  echo "Missing DMG background image in $dmg_path" >&2
  exit 1
fi

if ! strings "$mount_dir/.DS_Store" | grep -q "dmg-background.png"; then
  echo "Finder metadata in $dmg_path does not reference dmg-background.png" >&2
  exit 1
fi

app_path="$mount_dir/Ratspeak.app"
if [ ! -d "$app_path" ]; then
  echo "Missing Ratspeak.app in $dmg_path" >&2
  exit 1
fi

executable_name="$(plutil -extract CFBundleExecutable raw -o - "$app_path/Contents/Info.plist")"
binary_path="$app_path/Contents/MacOS/$executable_name"
if [ ! -f "$binary_path" ]; then
  echo "Missing app executable $binary_path in $dmg_path" >&2
  exit 1
fi

actual_arch="$(lipo -archs "$binary_path")"
if [ "$actual_arch" != "$expected_arch" ]; then
  echo "Unexpected macOS binary architecture in $dmg_path: expected $expected_arch, got $actual_arch" >&2
  exit 1
fi

file "$binary_path"
codesign -dv --verbose=4 "$app_path" 2>&1 | sed -n '1,36p'
codesign --verify --deep --strict --verbose=4 "$app_path"
