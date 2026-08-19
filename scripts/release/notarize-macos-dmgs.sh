#!/usr/bin/env bash
# Submit signed outer DMGs to Apple's notary service, retain submission IDs,
# bound the local wait, and support resuming without rebuilding/resubmitting.
set -euo pipefail

mode="${1:-}"
state_dir="${2:-}"
shift 2 || true

if [[ "$mode" != "submit" && "$mode" != "resume" ]]; then
  echo "usage: notarize-macos-dmgs.sh <submit|resume> STATE_DIR DMG..." >&2
  exit 2
fi
if [[ -z "$state_dir" || "$#" -eq 0 ]]; then
  echo "usage: notarize-macos-dmgs.sh <submit|resume> STATE_DIR DMG..." >&2
  exit 2
fi

for name in APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
  if [[ -z "${!name:-}" ]]; then
    echo "Missing required macOS notarization value: $name" >&2
    exit 1
  fi
done

wait_timeout="${NOTARY_WAIT_TIMEOUT:-30m}"
if [[ ! "$wait_timeout" =~ ^[1-9][0-9]*[smh]$ ]]; then
  echo "NOTARY_WAIT_TIMEOUT must be a positive duration such as 30m" >&2
  exit 1
fi

mkdir -p "$state_dir"
submissions="$state_dir/submissions.tsv"
if [[ "$mode" == "submit" ]]; then
  : > "$submissions"
elif [[ ! -f "$submissions" ]]; then
  echo "Missing notarization recovery state: $submissions" >&2
  exit 1
fi

notary_auth=(
  --apple-id "$APPLE_ID"
  --password "$APPLE_PASSWORD"
  --team-id "$APPLE_TEAM_ID"
)

submission_id_for() {
  local filename="$1"
  awk -F $'\t' -v filename="$filename" '$1 == filename { print $2; exit }' "$submissions"
}

submit_if_needed() {
  local dmg="$1"
  local filename output submission_id
  filename="$(basename "$dmg")"
  submission_id="$(submission_id_for "$filename")"
  if [[ -n "$submission_id" ]]; then
    echo "Reusing Apple notarization submission $submission_id for $filename"
    return
  fi

  output="$state_dir/$filename.submit.json"
  xcrun notarytool submit "$dmg" \
    --no-wait \
    --output-format json \
    "${notary_auth[@]}" > "$output"
  submission_id="$(plutil -extract id raw -o - "$output")"
  if [[ ! "$submission_id" =~ ^[0-9A-Fa-f-]{36}$ ]]; then
    echo "Invalid Apple notarization submission ID for $filename" >&2
    exit 1
  fi
  printf '%s\t%s\n' "$filename" "$submission_id" >> "$submissions"
  echo "Submitted $filename to Apple notarization as $submission_id"
}

for dmg in "$@"; do
  if [[ ! -f "$dmg" ]]; then
    echo "Missing signed DMG: $dmg" >&2
    exit 1
  fi
  submit_if_needed "$dmg"
done

wait_for_submission() {
  local dmg="$1"
  local filename submission_id output status
  filename="$(basename "$dmg")"
  submission_id="$(submission_id_for "$filename")"
  output="$state_dir/$filename.wait.json"

  echo "Waiting up to $wait_timeout for $filename ($submission_id)"
  if ! xcrun notarytool wait "$submission_id" \
    --timeout "$wait_timeout" \
    --output-format json \
    "${notary_auth[@]}" > "$output"; then
    return 1
  fi
  status="$(plutil -extract status raw -o - "$output")"
  if [[ "$status" != "Accepted" ]]; then
    echo "Apple notarization returned $status for $filename ($submission_id)" >&2
    return 1
  fi
}

pids=()
for dmg in "$@"; do
  wait_for_submission "$dmg" &
  pids+=("$!")
done

wait_failed=0
for pid in "${pids[@]}"; do
  if ! wait "$pid"; then
    wait_failed=1
  fi
done

if [[ "$wait_failed" -ne 0 ]]; then
  while IFS=$'\t' read -r filename submission_id; do
    [[ -n "$filename" && -n "$submission_id" ]] || continue
    xcrun notarytool info "$submission_id" \
      --output-format json \
      "${notary_auth[@]}" > "$state_dir/$filename.info.json" || true
    xcrun notarytool log "$submission_id" \
      "$state_dir/$filename.log.json" \
      "${notary_auth[@]}" || true
  done < "$submissions"
  echo "Apple notarization did not finish within the bounded wait." >&2
  echo "Preserve $submissions and resume these submission IDs without rebuilding." >&2
  exit 1
fi

for dmg in "$@"; do
  filename="$(basename "$dmg")"
  submission_id="$(submission_id_for "$filename")"
  xcrun notarytool log "$submission_id" \
    "$state_dir/$filename.log.json" \
    "${notary_auth[@]}"
  xcrun stapler staple -v "$dmg"
  xcrun stapler validate -v "$dmg"
done
