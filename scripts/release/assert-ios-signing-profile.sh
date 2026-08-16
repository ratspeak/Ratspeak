#!/usr/bin/env bash
set -euo pipefail

profile="${1:-}"
expected_team_id="${2:-}"
expected_bundle_id="${3:-org.ratspeak.apple}"

if [[ ! -f "$profile" ]]; then
  echo "Provisioning profile is missing: ${profile:-<not supplied>}" >&2
  exit 1
fi
if [[ -z "$expected_team_id" ]]; then
  echo "An Apple team ID is required to validate the provisioning profile" >&2
  exit 1
fi

decoded_profile="$(mktemp "${TMPDIR:-/tmp}/ratspeak-profile.XXXXXX")"
trap 'rm -f "$decoded_profile"' EXIT

if ! security cms -D -i "$profile" -o "$decoded_profile"; then
  echo "Could not decode provisioning profile: $profile" >&2
  exit 1
fi
plutil -lint "$decoded_profile" >/dev/null

plist_raw() {
  plutil -extract "$2" raw -o - "$1" 2>/dev/null
}

uuid="$(plist_raw "$decoded_profile" UUID || true)"
profile_team="$(plist_raw "$decoded_profile" TeamIdentifier.0 || true)"
application_identifier="$(plist_raw "$decoded_profile" Entitlements.application-identifier || true)"
entitlement_team="$(plist_raw "$decoded_profile" 'Entitlements.com\.apple\.developer\.team-identifier' || true)"
debuggable="$(plist_raw "$decoded_profile" Entitlements.get-task-allow || true)"
multicast_enabled="$(plist_raw "$decoded_profile" 'Entitlements.com\.apple\.developer\.networking\.multicast' || true)"
expires="$(plist_raw "$decoded_profile" ExpirationDate || true)"

if [[ ! "$uuid" =~ ^[A-Fa-f0-9-]+$ ]]; then
  echo "Provisioning profile has no valid UUID" >&2
  exit 1
fi
if [[ "$profile_team" != "$expected_team_id" || "$entitlement_team" != "$expected_team_id" ]]; then
  echo "Provisioning profile team does not match APPLE_TEAM_ID" >&2
  exit 1
fi
if [[ "$application_identifier" != "$expected_team_id.$expected_bundle_id" ]]; then
  echo "Provisioning profile targets $application_identifier, expected $expected_team_id.$expected_bundle_id" >&2
  exit 1
fi
if [[ "$debuggable" != "false" ]]; then
  echo "Provisioning profile is not an App Store distribution profile (get-task-allow=$debuggable)" >&2
  exit 1
fi
if [[ "$multicast_enabled" != "true" ]]; then
  echo "Provisioning profile does not include com.apple.developer.networking.multicast=true" >&2
  exit 1
fi
provisions_all_devices="$(plist_raw "$decoded_profile" ProvisionsAllDevices || true)"
if [[ "$provisions_all_devices" == "true" ]]; then
  echo "Provisioning profile is an enterprise profile, not an App Store profile" >&2
  exit 1
fi
if plutil -extract ProvisionedDevices json -o - "$decoded_profile" >/dev/null 2>&1; then
  echo "Provisioning profile is device-scoped, not an App Store profile" >&2
  exit 1
fi

if [[ -z "$expires" ]]; then
  echo "Provisioning profile has no expiration date" >&2
  exit 1
fi
expiration_epoch="$(date -j -f '%Y-%m-%d %H:%M:%S %z' "$expires" '+%s' 2>/dev/null || true)"
if [[ -z "$expiration_epoch" ]]; then
  expiration_epoch="$(date -j -f '%Y-%m-%dT%H:%M:%SZ' "$expires" '+%s' 2>/dev/null || true)"
fi
if [[ -z "$expiration_epoch" || "$expiration_epoch" -le "$(date '+%s')" ]]; then
  echo "Provisioning profile is expired or has an unreadable expiration date: $expires" >&2
  exit 1
fi

printf '%s\n' "$uuid"
