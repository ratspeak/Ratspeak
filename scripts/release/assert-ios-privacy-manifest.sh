#!/usr/bin/env bash
set -euo pipefail

search_root="${1:-src-tauri/gen/apple/build}"

if [[ ! -e "$search_root" ]]; then
  echo "iOS build output does not exist: $search_root" >&2
  exit 1
fi

if [[ -f "$search_root" && "$(basename "$search_root")" == "PrivacyInfo.xcprivacy" ]]; then
  manifest="$search_root"
elif [[ -d "$search_root" && "$search_root" == *.app ]]; then
  manifest="$search_root/PrivacyInfo.xcprivacy"
else
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
fi

if [[ ! -f "$manifest" ]]; then
  echo "PrivacyInfo.xcprivacy is missing: $manifest" >&2
  exit 1
fi

plutil -lint "$manifest" >/dev/null

tracking="$(plutil -extract NSPrivacyTracking raw -o - "$manifest" 2>/dev/null || true)"
tracking_domains="$(plutil -extract NSPrivacyTrackingDomains json -o - "$manifest" 2>/dev/null || true)"
collected_data="$(plutil -extract NSPrivacyCollectedDataTypes json -o - "$manifest" 2>/dev/null || true)"
if [[ "$tracking" != "false" || "$tracking_domains" != '[]' || "$collected_data" != '[]' ]]; then
  echo "$manifest: privacy collection or tracking declarations differ from Ratspeak's current behavior" >&2
  exit 1
fi

manifest_xml="$(plutil -convert xml1 -o - "$manifest")"
if [[ "$(grep -c '<key>NSPrivacyAccessedAPIType</key>' <<<"$manifest_xml")" -ne 4 ]] || \
   [[ "$(grep -c '<key>NSPrivacyAccessedAPITypeReasons</key>' <<<"$manifest_xml")" -ne 4 ]]; then
  echo "$manifest: expected exactly four required-reason API declarations" >&2
  exit 1
fi

for expected_value in \
  NSPrivacyAccessedAPICategorySystemBootTime 35F9.1 \
  NSPrivacyAccessedAPICategoryFileTimestamp C617.1 \
  NSPrivacyAccessedAPICategoryDiskSpace E174.1 \
  NSPrivacyAccessedAPICategoryUserDefaults CA92.1; do
  if [[ "$(grep -c ">${expected_value}<" <<<"$manifest_xml")" -ne 1 ]]; then
    echo "$manifest: missing or duplicated required-reason value: $expected_value" >&2
    exit 1
  fi
done

echo "Validated iOS privacy manifest: $manifest"
