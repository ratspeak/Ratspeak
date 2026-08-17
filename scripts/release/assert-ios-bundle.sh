#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
artifact="${2:-}"
expected_bundle_id="${EXPECTED_IOS_BUNDLE_ID:-org.ratspeak.apple}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dependency_set="$repo_root/release/dependency-set.json"
expected_version="${EXPECTED_IOS_VERSION:-$(node -p "require(process.argv[1]).product.marketingVersion" "$dependency_set")}"
expected_build="${EXPECTED_IOS_BUILD:-$(node -p "require(process.argv[1]).product.platformBuilds.iosBundleVersion" "$dependency_set")}"

if [[ "$mode" != "simulator" && "$mode" != "testflight" ]]; then
  echo "Usage: $0 <simulator|testflight> <app-or-ipa>" >&2
  exit 1
fi
if [[ ! -e "$artifact" ]]; then
  echo "iOS artifact does not exist: ${artifact:-<not supplied>}" >&2
  exit 1
fi

temporary_dir=""
asset_info=""
signature_entitlements=""
signature_errors=""
cleanup() {
  if [[ -n "$temporary_dir" ]]; then
    rm -rf "$temporary_dir"
  fi
  for temporary_file in "$asset_info" "$signature_entitlements" "$signature_errors"; do
    if [[ -n "$temporary_file" ]]; then
      rm -f "$temporary_file"
    fi
  done
}
trap cleanup EXIT

if [[ -d "$artifact" && "$artifact" == *.app ]]; then
  app="$artifact"
elif [[ -f "$artifact" && "$artifact" == *.ipa ]]; then
  temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/ratspeak-ipa.XXXXXX")"
  ditto -x -k "$artifact" "$temporary_dir"
  app="$(find "$temporary_dir/Payload" -maxdepth 1 -type d -name '*.app' -print -quit)"
  if [[ -z "$app" ]]; then
    echo "IPA contains no application bundle: $artifact" >&2
    exit 1
  fi
else
  echo "Expected an .app bundle or .ipa archive: $artifact" >&2
  exit 1
fi

info="$app/Info.plist"
if [[ ! -f "$info" ]]; then
  echo "Application Info.plist is missing: $info" >&2
  exit 1
fi
plutil -lint "$info" >/dev/null

plist_raw() {
  plutil -extract "$2" raw -o - "$1" 2>/dev/null
}

plist_json() {
  plutil -extract "$2" json -o - "$1" 2>/dev/null
}

expect_raw() {
  local key="$1" expected="$2" actual
  actual="$(plist_raw "$info" "$key" || true)"
  if [[ "$actual" != "$expected" ]]; then
    echo "$info: expected $key=$expected, found ${actual:-<missing>}" >&2
    exit 1
  fi
}

expect_json() {
  local key="$1" expected="$2" actual
  actual="$(plist_json "$info" "$key" || true)"
  if [[ "$actual" != "$expected" ]]; then
    echo "$info: unexpected $key: ${actual:-<missing>}" >&2
    exit 1
  fi
}

expect_raw CFBundleIdentifier "$expected_bundle_id"
expect_raw CFBundleName Ratspeak
expect_raw CFBundleShortVersionString "$expected_version"
expect_raw LSRequiresIPhoneOS true
expect_raw MinimumOSVersion 14.0
expect_raw UILaunchStoryboardName LaunchScreen
expect_json UIDeviceFamily '[1,2]'
expect_json CFBundleURLTypes '[{"CFBundleURLName":"ratspeak","CFBundleURLSchemes":["ratspeak"]}]'
expect_json UIBackgroundModes '["audio","bluetooth-central","bluetooth-peripheral"]'
expect_json UIRequiredDeviceCapabilities '["arm64","metal"]'
expect_json UISupportedInterfaceOrientations '["UIInterfaceOrientationPortrait"]'
expect_json 'UISupportedInterfaceOrientations~ipad' '["UIInterfaceOrientationPortrait","UIInterfaceOrientationPortraitUpsideDown","UIInterfaceOrientationLandscapeLeft","UIInterfaceOrientationLandscapeRight"]'

for permission_key in \
  NSBluetoothAlwaysUsageDescription \
  NSBluetoothPeripheralUsageDescription \
  NSCameraUsageDescription \
  NSLocalNetworkUsageDescription \
  NSMicrophoneUsageDescription \
  NSPhotoLibraryAddUsageDescription \
  NSPhotoLibraryUsageDescription; do
  if [[ -z "$(plist_raw "$info" "$permission_key" || true)" ]]; then
    echo "$info: missing or empty $permission_key" >&2
    exit 1
  fi
done

if plutil -extract UIRequiresFullScreen raw -o - "$info" >/dev/null 2>&1; then
  echo "$info: UIRequiresFullScreen is deprecated and must not be declared" >&2
  exit 1
fi

build_number="$(plist_raw "$info" CFBundleVersion || true)"
if [[ ! "$build_number" =~ ^[0-9]+([.][0-9]+){0,2}$ ]]; then
  echo "$info: invalid CFBundleVersion: ${build_number:-<missing>}" >&2
  exit 1
fi
if [[ "$build_number" != "$expected_build" ]]; then
  echo "$info: expected CFBundleVersion=$expected_build, found $build_number" >&2
  exit 1
fi

xcode_build="$(plist_raw "$info" DTXcode || true)"
sdk_name="$(plist_raw "$info" DTSDKName || true)"
if [[ ! "$xcode_build" =~ ^[0-9]+$ ]] || ((10#$xcode_build < 2600)); then
  echo "$info: artifact was not built with Xcode 26 or newer: ${xcode_build:-<missing>}" >&2
  exit 1
fi
if [[ ! "$sdk_name" =~ ^(iphoneos|iphonesimulator)26([.]|$) ]]; then
  echo "$info: artifact was not built with an iOS 26 SDK: ${sdk_name:-<missing>}" >&2
  exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bash "$script_dir/assert-ios-privacy-manifest.sh" "$app"

icon_count=0
while IFS= read -r -d '' icon; do
  icon_count=$((icon_count + 1))
  if [[ "$mode" == "testflight" ]] && \
     [[ "$(sips -g hasAlpha "$icon" 2>/dev/null | awk '/hasAlpha:/ { print $2 }')" != "no" ]]; then
    echo "Compiled App Store icon contains transparency: $icon" >&2
    exit 1
  fi
done < <(find "$app" -maxdepth 1 -type f -name 'AppIcon*.png' -print0)
if ((icon_count < 2)); then
  echo "$app: expected compiled phone and iPad app icons" >&2
  exit 1
fi

assets_car="$app/Assets.car"
if [[ ! -f "$assets_car" ]]; then
  echo "$app: compiled asset catalog is missing" >&2
  exit 1
fi
asset_info="$(mktemp "${TMPDIR:-/tmp}/ratspeak-assets.XXXXXX")"
if ! xcrun --sdk iphoneos assetutil --info "$assets_car" >"$asset_info"; then
  echo "Could not inspect compiled iOS assets" >&2
  exit 1
fi
if ! awk '
  /"AssetType" : "Icon Image"/ { in_icon = 1; icon_count++; opaque = 0; next }
  in_icon && /"Opaque" : true/ { opaque = 1 }
  in_icon && /^  }[,]?$/ { if (!opaque) bad++; in_icon = 0 }
  END { exit !(icon_count >= 4 && bad == 0) }
' "$asset_info"; then
  echo "$app: compiled icon renditions are missing or not all opaque" >&2
  exit 1
fi
rm -f "$asset_info"
asset_info=""

if [[ "$mode" == "testflight" ]]; then
  if ! codesign --verify --deep --strict "$app"; then
    echo "$app: code signature verification failed" >&2
    exit 1
  fi

  signature_entitlements="$(mktemp "${TMPDIR:-/tmp}/ratspeak-entitlements.XXXXXX")"
  signature_errors="$(mktemp "${TMPDIR:-/tmp}/ratspeak-codesign.XXXXXX")"
  if ! codesign -d --entitlements :- "$app" >"$signature_entitlements" 2>"$signature_errors"; then
    cat "$signature_errors" >&2
    echo "$app: could not read signed entitlements" >&2
    exit 1
  fi
  if grep -qi 'invalid entitlements blob' "$signature_errors"; then
    cat "$signature_errors" >&2
    echo "$app: invalid signed entitlement blob" >&2
    exit 1
  fi
  plutil -lint "$signature_entitlements" >/dev/null
  get_task_allow="$(plist_raw "$signature_entitlements" get-task-allow || true)"
  if [[ "$get_task_allow" == "true" ]]; then
    echo "$app: TestFlight signature enables get-task-allow" >&2
    exit 1
  fi
  multicast_enabled="$(plist_raw "$signature_entitlements" 'com\.apple\.developer\.networking\.multicast' || true)"
  if [[ "$multicast_enabled" != "true" ]]; then
    echo "$app: TestFlight signature does not include com.apple.developer.networking.multicast=true" >&2
    exit 1
  fi

  embedded_profile="$app/embedded.mobileprovision"
  if [[ -z "${APPLE_TEAM_ID:-}" ]]; then
    echo "APPLE_TEAM_ID is required to validate a TestFlight artifact" >&2
    exit 1
  fi
  bash "$script_dir/assert-ios-signing-profile.sh" \
    "$embedded_profile" "$APPLE_TEAM_ID" "$expected_bundle_id" >/dev/null
  rm -f "$signature_entitlements" "$signature_errors"
  signature_entitlements=""
  signature_errors=""
fi

echo "Validated $mode iOS bundle: $app"
