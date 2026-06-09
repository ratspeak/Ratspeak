#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

if [ "$#" -eq 0 ]; then
  host_target="$(rustc -vV | awk '/^host:/ { print $2 }')"
  targets=("$host_target")
else
  targets=("$@")
fi

if [ "${#targets[@]}" -eq 0 ] || [ -z "${targets[0]}" ]; then
  echo "failed to determine Rust host target triple" >&2
  exit 1
fi

mkdir -p src-tauri/binaries

for target in "${targets[@]}"; do
  case "$target" in
    *windows*) exe_ext=".exe" ;;
    *) exe_ext="" ;;
  esac

  cargo build -p ratspeak-cli --release --bins --target "$target"

  for bin in ratspeakctl ratspeakd; do
    src="target/$target/release/$bin$exe_ext"
    dest="src-tauri/binaries/$bin-$target$exe_ext"
    if [ ! -f "$src" ]; then
      echo "expected sidecar binary missing: $src" >&2
      exit 1
    fi
    cp "$src" "$dest"
    chmod 755 "$dest"
  done

  "src-tauri/binaries/ratspeakctl-$target$exe_ext" version >/dev/null
done
