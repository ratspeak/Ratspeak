#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  echo "AppImage runtime staging requires Linux x86_64" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
stage_dir="$repo_root/src-tauri/resources/linux/appimage-runtime"
mkdir -p "$stage_dir"

sonames=(
  libayatana-appindicator3.so.1
  libayatana-ido3-0.4.so.0
  libayatana-indicator3.so.7
  libdbusmenu-glib.so.4
  libdbusmenu-gtk3.so.4
)

for soname in "${sonames[@]}"; do
  rm -f "$stage_dir/$soname"
done

for soname in "${sonames[@]}"; do
  source_path="$({ ldconfig -p || true; } | awk -v soname="$soname" '
    $1 == soname && $0 ~ /x86-64/ { print $NF; exit }
  ')"
  if [[ -z "$source_path" || ! -f "$source_path" ]]; then
    echo "Unable to resolve required AppImage runtime library: $soname" >&2
    exit 1
  fi

  source_path="$(readlink -f "$source_path")"
  install -m 0644 "$source_path" "$stage_dir/$soname"
  if ! LC_ALL=C file "$stage_dir/$soname" | grep -Eq 'ELF 64-bit LSB .*x86-64'; then
    echo "Unexpected architecture for staged AppImage library: $soname" >&2
    exit 1
  fi
done

echo "Staged ${#sonames[@]} AppImage indicator runtime libraries"
