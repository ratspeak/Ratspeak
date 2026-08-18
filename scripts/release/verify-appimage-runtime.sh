#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 <appimage-path>" >&2
  exit 2
fi

appimage="$(realpath "$1")"
if [[ ! -f "$appimage" || ! -x "$appimage" ]]; then
  echo "AppImage is missing or not executable: $appimage" >&2
  exit 1
fi

extract_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$extract_dir"
}
trap cleanup EXIT

(
  cd "$extract_dir"
  "$appimage" --appimage-extract >/dev/null
)

runtime_dir="$extract_dir/squashfs-root/usr/lib"
sonames=(
  libayatana-appindicator3.so.1
  libayatana-ido3-0.4.so.0
  libayatana-indicator3.so.7
  libdbusmenu-glib.so.4
  libdbusmenu-gtk3.so.4
)

for soname in "${sonames[@]}"; do
  library="$runtime_dir/$soname"
  if [[ ! -s "$library" ]]; then
    echo "AppImage is missing required indicator runtime library: $soname" >&2
    exit 1
  fi
  if ! LC_ALL=C file "$library" | grep -Eq 'ELF 64-bit LSB .*x86-64'; then
    echo "AppImage contains an invalid indicator runtime library: $soname" >&2
    exit 1
  fi
done

dependency_report="$(LD_LIBRARY_PATH="$runtime_dir" ldd "$runtime_dir/libayatana-appindicator3.so.1")"
for dependency in "${sonames[@]:1}"; do
  if ! grep -F "$dependency => $runtime_dir/$dependency" <<<"$dependency_report" >/dev/null; then
    echo "AppImage indicator dependency escaped its bundled runtime: $dependency" >&2
    printf '%s\n' "$dependency_report" >&2
    exit 1
  fi
done

echo "AppImage indicator runtime closure verified"
