#!/usr/bin/env bash
set -euo pipefail

# Tauri declares WebKitGTK, GTK, and the tray library it loads at runtime; every
# other library the binary links must be declared once in tauri.conf.json.
if [[ "$#" -lt 1 ]]; then
  echo "usage: $0 <package.deb|package.rpm>..." >&2
  exit 2
fi

# Linked libraries the WebKitGTK/GTK stack or the C runtime already brings in.
covered='^(ld-linux.*|libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|libcairo\.so\.2|libgdk-3\.so\.0|libgdk_pixbuf-2\.0\.so\.0|libgio-2\.0\.so\.0|libglib-2\.0\.so\.0|libgobject-2\.0\.so\.0|libjavascriptcoregtk-4\.1\.so\.0|libsoup-3\.0\.so\.0|libwayland-client\.so\.0)$'

declare -A deb_package=(
  [libwebkit2gtk-4.1.so.0]="libwebkit2gtk-4.1-0"
  [libgtk-3.so.0]="libgtk-3-0"
  [libasound.so.2]="libasound2t64 | libasound2"
  [libpcsclite.so.1]="libpcsclite1"
  [libudev.so.1]="libudev1"
)
deb_runtime=("libayatana-appindicator3-1")
rpm_runtime=("libayatana-appindicator3.so.1()(64bit)")

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for package in "$@"; do
  root="$work/$(basename "$package")"
  mkdir -p "$root"
  case "$package" in
    *.deb)
      dpkg-deb --extract "$package" "$root"
      mapfile -t declared < <(dpkg-deb --field "$package" Depends | sed 's/, /\n/g')
      required=("${deb_runtime[@]}")
      ;;
    *.rpm)
      source="$(realpath "$package")"
      # Some rpm2cpio builds exit 1 after a complete stream; cpio still fails on a short one.
      (cd "$root" && { rpm2cpio "$source" || true; } | cpio --quiet -idm)
      mapfile -t declared < <(rpm -qpR "$package")
      required=("${rpm_runtime[@]}")
      ;;
    *)
      echo "unsupported package: $package" >&2
      exit 2
      ;;
  esac

  duplicates="$(printf '%s\n' "${declared[@]}" | sort | uniq -d)"
  if [[ -n "$duplicates" ]]; then
    echo "$package declares duplicate dependencies:" >&2
    printf '%s\n' "$duplicates" >&2
    exit 1
  fi

  binary="$root/usr/bin/ratspeak"
  if [[ ! -x "$binary" ]]; then
    echo "$package has no /usr/bin/ratspeak" >&2
    exit 1
  fi
  while read -r soname; do
    [[ "$soname" =~ $covered ]] && continue
    case "$package" in
      *.deb)
        if [[ -z "${deb_package[$soname]:-}" ]]; then
          echo "ratspeak links $soname, which has no Debian package mapping here" >&2
          exit 1
        fi
        required+=("${deb_package[$soname]}")
        ;;
      *.rpm) required+=("$soname()(64bit)") ;;
    esac
  done < <(readelf -d "$binary" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')

  for requirement in "${required[@]}"; do
    if ! printf '%s\n' "${declared[@]}" | grep -Fxq -- "$requirement"; then
      echo "$package does not require $requirement" >&2
      printf '%s\n' "${declared[@]}" >&2
      exit 1
    fi
  done
  echo "$package dependencies cover every linked library"
done
