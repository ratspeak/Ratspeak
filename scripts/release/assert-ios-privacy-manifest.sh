#!/usr/bin/env bash
set -euo pipefail

search_root="${1:-src-tauri/gen/apple/build}"

if [[ ! -e "$search_root" ]]; then
  echo "iOS build output does not exist: $search_root" >&2
  exit 1
fi

latest_app=""
latest_mtime=0
while IFS= read -r -d '' app_bundle; do
  if mtime="$(stat -f '%m' "$app_bundle" 2>/dev/null)"; then
    :
  else
    mtime="$(stat -c '%Y' "$app_bundle")"
  fi
  if ((mtime > latest_mtime)); then
    latest_mtime="$mtime"
    latest_app="$app_bundle"
  fi
done < <(find "$search_root" -type d -name '*.app' -print0)

if [[ -z "$latest_app" ]]; then
  echo "No iOS app bundle exists under: $search_root" >&2
  exit 1
fi

manifest="$latest_app/PrivacyInfo.xcprivacy"
if [[ ! -f "$manifest" ]]; then
  echo "PrivacyInfo.xcprivacy is missing from the newest iOS app bundle: $latest_app" >&2
  exit 1
fi

plutil -lint "$manifest" >/dev/null
echo "Bundled iOS privacy manifest: $manifest"
