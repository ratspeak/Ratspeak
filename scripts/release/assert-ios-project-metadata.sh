#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-.}"
project_model="$repo_root/src-tauri/gen/apple/project.yml"
project_file="$repo_root/src-tauri/gen/apple/ratspeak.xcodeproj/project.pbxproj"
info_plist="$repo_root/src-tauri/gen/apple/ratspeak_iOS/Info.plist"
privacy_manifest="$repo_root/src-tauri/gen/apple/ratspeak_iOS/PrivacyInfo.xcprivacy"
entitlements="$repo_root/src-tauri/gen/apple/ratspeak_iOS/ratspeak_iOS.entitlements"
expected_team_id="X92A7KF9SP"

for required_file in "$project_model" "$project_file" "$info_plist" "$privacy_manifest" "$entitlements"; do
  if [[ ! -f "$required_file" ]]; then
    echo "Required iOS project file is missing: $required_file" >&2
    exit 1
  fi
done

plist_raw() {
  plutil -extract "$2" raw -o - "$1" 2>/dev/null
}

plist_json() {
  plutil -extract "$2" json -o - "$1" 2>/dev/null
}

expect_raw() {
  local plist="$1" key="$2" expected="$3" actual
  actual="$(plist_raw "$plist" "$key" || true)"
  if [[ "$actual" != "$expected" ]]; then
    echo "$plist: expected $key=$expected, found ${actual:-<missing>}" >&2
    exit 1
  fi
}

expect_json() {
  local plist="$1" key="$2" expected="$3" actual
  actual="$(plist_json "$plist" "$key" || true)"
  if [[ "$actual" != "$expected" ]]; then
    echo "$plist: unexpected $key: ${actual:-<missing>}" >&2
    exit 1
  fi
}

plutil -lint "$info_plist" >/dev/null
plutil -lint "$privacy_manifest" >/dev/null
plutil -lint "$entitlements" >/dev/null

expect_raw "$info_plist" CFBundleIdentifier '$(PRODUCT_BUNDLE_IDENTIFIER)'
expect_raw "$info_plist" CFBundleShortVersionString '1.0.26'
expect_raw "$info_plist" CFBundleVersion '1.0.26'
expect_raw "$info_plist" LSRequiresIPhoneOS true
expect_raw "$info_plist" UILaunchStoryboardName LaunchScreen
expect_json "$info_plist" CFBundleURLTypes '[{"CFBundleURLName":"ratspeak","CFBundleURLSchemes":["ratspeak"]}]'
expect_json "$info_plist" NSBonjourServices '["_reticulum._udp"]'
expect_json "$info_plist" UIBackgroundModes '["audio","bluetooth-central","bluetooth-peripheral"]'
expect_json "$info_plist" UIRequiredDeviceCapabilities '["arm64","metal"]'
expect_json "$info_plist" UISupportedInterfaceOrientations '["UIInterfaceOrientationPortrait"]'
expect_json "$info_plist" 'UISupportedInterfaceOrientations~ipad' '["UIInterfaceOrientationPortrait","UIInterfaceOrientationPortraitUpsideDown"]'

for permission_key in \
  NSBluetoothAlwaysUsageDescription \
  NSBluetoothPeripheralUsageDescription \
  NSCameraUsageDescription \
  NSLocalNetworkUsageDescription \
  NSMicrophoneUsageDescription \
  NSPhotoLibraryAddUsageDescription \
  NSPhotoLibraryUsageDescription; do
  if [[ -z "$(plist_raw "$info_plist" "$permission_key" || true)" ]]; then
    echo "$info_plist: missing or empty $permission_key" >&2
    exit 1
  fi
done

if plutil -extract UIRequiresFullScreen raw -o - "$info_plist" >/dev/null 2>&1; then
  echo "$info_plist: UIRequiresFullScreen is deprecated and must not be declared" >&2
  exit 1
fi

for declaration in \
  'bundleIdPrefix: org.ratspeak.ios' \
  'PRODUCT_BUNDLE_IDENTIFIER: org.ratspeak.ios' \
  "DEVELOPMENT_TEAM: $expected_team_id" \
  'iOS: 14.0' \
  'TARGETED_DEVICE_FAMILY: "1,2"' \
  'path: ratspeak_iOS/PrivacyInfo.xcprivacy' \
  'buildPhase: resources'; do
  if ! grep -Fq "$declaration" "$project_model"; then
    echo "$project_model: missing declaration: $declaration" >&2
    exit 1
  fi
done

team_setting_count="$(grep -Fc "DEVELOPMENT_TEAM = $expected_team_id;" "$project_file" || true)"
if [[ "$team_setting_count" -ne 2 ]]; then
  echo "$project_file: expected both build configurations to use team $expected_team_id, found $team_setting_count" >&2
  exit 1
fi

if grep -F 'DEVELOPMENT_TEAM = ' "$project_file" | grep -Fvq "DEVELOPMENT_TEAM = $expected_team_id;"; then
  echo "$project_file: contains a signing team other than $expected_team_id" >&2
  exit 1
fi

if ! grep -Fq 'PrivacyInfo.xcprivacy in Resources' "$project_file"; then
  echo "$project_file: privacy manifest is not in the Resources phase" >&2
  exit 1
fi

multicast_enabled="$(plist_raw "$entitlements" com.apple.developer.networking.multicast || true)"
if [[ "$multicast_enabled" == "true" ]]; then
  if ! grep -Fq 'entitlements:' "$project_model" || ! grep -Fq 'CODE_SIGN_ENTITLEMENTS' "$project_file"; then
    echo "The multicast entitlement is enabled but the Xcode project does not sign it" >&2
    exit 1
  fi
else
  if grep -Fq 'entitlements:' "$project_model" || grep -Fq 'CODE_SIGN_ENTITLEMENTS' "$project_file"; then
    echo "The empty entitlement file must not be signed before multicast approval" >&2
    exit 1
  fi
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bash "$script_dir/assert-ios-privacy-manifest.sh" "$privacy_manifest"

echo "iOS project metadata is release-consistent."
