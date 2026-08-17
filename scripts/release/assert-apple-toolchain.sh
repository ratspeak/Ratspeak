#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dependency_set="$repo_root/release/dependency-set.json"
expected_xcode_version="${EXPECTED_XCODE_VERSION:-$(node -p "require(process.argv[1]).toolchains.xcode" "$dependency_set")}"
expected_ios_sdk_version="${EXPECTED_IOS_SDK_VERSION:-$(node -p "require(process.argv[1]).toolchains.iosSdk" "$dependency_set")}"

for command_name in xcodebuild xcrun; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Required Apple tool is unavailable: $command_name" >&2
    exit 1
  fi
done

xcode_version="$(xcodebuild -version | awk 'NR == 1 { print $2 }')"
ios_sdk_version="$(xcrun --sdk iphoneos --show-sdk-version)"

if [[ "$xcode_version" != "$expected_xcode_version" ]]; then
  echo "Expected Xcode $expected_xcode_version; found: ${xcode_version:-unknown}" >&2
  exit 1
fi

if [[ "$ios_sdk_version" != "$expected_ios_sdk_version" ]]; then
  echo "Expected iOS SDK $expected_ios_sdk_version; found: ${ios_sdk_version:-unknown}" >&2
  exit 1
fi

echo "Apple toolchain: Xcode $xcode_version, iOS SDK $ios_sdk_version"
