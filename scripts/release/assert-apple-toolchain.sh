#!/usr/bin/env bash
set -euo pipefail

minimum_xcode_major="${MINIMUM_XCODE_MAJOR:-26}"
minimum_ios_sdk_major="${MINIMUM_IOS_SDK_MAJOR:-26}"

for command_name in xcodebuild xcrun; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Required Apple tool is unavailable: $command_name" >&2
    exit 1
  fi
done

xcode_version="$(xcodebuild -version | awk 'NR == 1 { print $2 }')"
xcode_major="${xcode_version%%.*}"
ios_sdk_version="$(xcrun --sdk iphoneos --show-sdk-version)"
ios_sdk_major="${ios_sdk_version%%.*}"

if [[ ! "$xcode_major" =~ ^[0-9]+$ ]] || ((xcode_major < minimum_xcode_major)); then
  echo "Xcode ${minimum_xcode_major} or newer is required; found: ${xcode_version:-unknown}" >&2
  exit 1
fi

if [[ ! "$ios_sdk_major" =~ ^[0-9]+$ ]] || ((ios_sdk_major < minimum_ios_sdk_major)); then
  echo "iOS SDK ${minimum_ios_sdk_major} or newer is required; found: ${ios_sdk_version:-unknown}" >&2
  exit 1
fi

echo "Apple toolchain: Xcode $xcode_version, iOS SDK $ios_sdk_version"
