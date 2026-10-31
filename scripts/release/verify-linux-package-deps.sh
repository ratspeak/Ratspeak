#!/usr/bin/env bash
set -euo pipefail

# The tray library is loaded at runtime, so packages must require it explicitly.
if [[ "$#" -lt 1 ]]; then
  echo "usage: $0 <package.deb|package.rpm>..." >&2
  exit 2
fi

for package in "$@"; do
  case "$package" in
    *.deb)
      depends="$(dpkg-deb --field "$package" Depends)"
      required=(libayatana-appindicator3-1 libwebkit2gtk-4.1-0 libgtk-3-0)
      ;;
    *.rpm)
      depends="$(rpm -qpR "$package")"
      required=('libayatana-appindicator3.so.1()(64bit)' 'libwebkit2gtk-4.1.so.0()(64bit)' 'libgtk-3.so.0()(64bit)')
      ;;
    *)
      echo "unsupported package: $package" >&2
      exit 2
      ;;
  esac
  for requirement in "${required[@]}"; do
    if ! grep -Fq -- "$requirement" <<<"$depends"; then
      echo "$package does not require $requirement" >&2
      printf '%s\n' "$depends" >&2
      exit 1
    fi
  done
  echo "$package dependencies verified"
done
