#!/usr/bin/env bash
# Verify the exact final macOS artifact set and write its checksum manifest.
set -euo pipefail

release_version="${1:?usage: finalize-macos-dmgs.sh RELEASE_VERSION NOTARIZED}"
notarized="${2:?usage: finalize-macos-dmgs.sh RELEASE_VERSION NOTARIZED}"
if [[ "$release_version" == *"/"* || "$release_version" == *"\\"* ]]; then
  echo "Release version must not contain path separators: $release_version" >&2
  exit 1
fi
if [[ "$notarized" != "true" && "$notarized" != "false" ]]; then
  echo "NOTARIZED must be true or false" >&2
  exit 1
fi

declare -a artifacts=(
  "dist/macos/Ratspeak-${release_version}-macos-arm64.dmg:arm64"
  "dist/macos/Ratspeak-${release_version}-macos-x64.dmg:x86_64"
)

for artifact in "${artifacts[@]}"; do
  dmg="${artifact%%:*}"
  expected_arch="${artifact##*:}"
  test -f "$dmg"
  hdiutil verify "$dmg"
  bash scripts/release/verify-macos-dmg.sh "$dmg" "$expected_arch"
  if [[ "$notarized" == "true" ]]; then
    codesign --verify --strict --verbose=2 "$dmg"
    xcrun stapler validate -v "$dmg"
    spctl -a -vv -t open --context context:primary-signature "$dmg"
  fi
done

bash scripts/release/assert-no-tauri-dev-url.sh dist/macos

checksum_file="checksums-macos.txt"
: > "$checksum_file"
find dist/macos -type f -print0 |
  sort -z |
  while IFS= read -r -d '' artifact; do
    hash="$(shasum -a 256 "$artifact" | cut -d ' ' -f 1)"
    printf '%s  %s\n' "$hash" "$(basename "$artifact")" >> "$checksum_file"
done
test -s "$checksum_file"
mv "$checksum_file" dist/macos/checksums-macos.txt
